//! Process what the outbound Palpo transport has taken into custody (TS parity:
//! `lib/fleet-outbound-client.js` `processOnce` + `lib/fleet-protocol.js`
//! `recordEvent` / `POST /api/fleet/v1/probe`).
//!
//! - Matrix lane: every App Service transaction Palpo relays is scanned for the
//!   fleet's connection-probe event sent by its representative; each one is
//!   recorded as a push receipt (at most 100 kept, as TS does).
//! - Work lane, `probe`: the named event must have arrived in such a
//!   transaction. It is then re-read as the representative, the room must be
//!   invite-only, unencrypted and joined by the representative, and the room is
//!   bound as the fleet's reception. The receipt rides the next `/updates`.
//! - Work lane, `request`: legacy requests wait for a console verdict.
//! - Work lane, `workflow`: Palpo-authorized project decisions commit their
//!   immutable business receipt before completing transport custody.
//!
//! A failure is retried later, never turned into a terminal refusal: the bridge
//! does not decide the connection is dead.
use super::probe::{PROBE_EVENT, ProbeError, ProbeReceipt, decide};
use hagency_core::custody::{Kind, Lane};
use hagency_palpo::{Adapter, CancellationToken, ProbeReceipts};
use hagency_store::{DomainStore, private};
use reqwest::{StatusCode, Url, redirect::Policy};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

#[cfg(test)]
mod cross_service;

/// The fleet's App Service identity, from `palpo-appservice.json`.
pub(super) struct Appservice {
    homeserver: String,
    as_token: String,
    representative: String,
}
impl Appservice {
    pub(super) fn load(state: &Path, server_name: &str) -> Option<Self> {
        let raw = private::read_secret(&state.join("palpo-appservice.json")).ok()?;
        let value: Value = serde_json::from_slice(&raw).ok()?;
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        Some(Self {
            homeserver: text("homeserver")?,
            as_token: text("as_token")?,
            representative: format!("@{}:{server_name}", text("sender_localpart")?),
        })
    }
}

/// Durable probe bookkeeping: push receipts seen on the Matrix lane and the
/// verified receipts awaiting publication. Both files are owner-private.
pub(super) struct Probes {
    seen: PathBuf,
    outbox: PathBuf,
    /// Request ids Palpo delivered on the work lane: the only requests Palpo
    /// knows, so the only ones whose status it accepts (an unknown request
    /// rejects the whole update).
    requests: PathBuf,
    lock: Mutex<()>,
    statuses: Mutex<Vec<Value>>,
    status_cursor: Mutex<String>,
    retirement_round: Mutex<usize>,
}
impl Probes {
    pub(super) fn new(state: &Path) -> Arc<Self> {
        Arc::new(Self {
            seen: state.join("palpo-probe-events.json"),
            outbox: state.join("palpo-probe-receipts.json"),
            requests: state.join("palpo-requests.json"),
            lock: Mutex::new(()),
            statuses: Mutex::new(Vec::new()),
            status_cursor: Mutex::new(String::new()),
            retirement_round: Mutex::new(0),
        })
    }
    fn read(path: &Path) -> Vec<Value> {
        private::read_secret(path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<Vec<Value>>(&raw).ok())
            .unwrap_or_default()
    }
    fn write(path: &Path, rows: &[Value]) {
        if let Ok(bytes) = serde_json::to_vec(rows) {
            let _ = private::replace(path, &bytes);
        }
    }
    fn record_event(&self, row: Value) {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows = Self::read(&self.seen);
        rows.retain(|r| r.get("sourceEventId") != row.get("sourceEventId"));
        rows.push(row);
        let excess = rows.len().saturating_sub(100);
        rows.drain(..excess);
        Self::write(&self.seen, &rows);
    }
    fn event(&self, id: &str) -> Option<Value> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Self::read(&self.seen)
            .into_iter()
            .find(|r| r.get("sourceEventId").and_then(Value::as_str) == Some(id))
    }
    fn remember_request(&self, id: &str) {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows = Self::read(&self.requests);
        if !rows.iter().any(|r| r.as_str() == Some(id)) {
            rows.push(json!(id));
        }
        Self::write(&self.requests, &rows);
    }
    fn palpo_request(&self, id: &str) -> bool {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Self::read(&self.requests)
            .iter()
            .any(|r| r.as_str() == Some(id))
    }
    fn queue_receipt(&self, receipt: Value) {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows = Self::read(&self.outbox);
        if !rows.contains(&receipt) {
            rows.push(receipt);
        }
        Self::write(&self.outbox, &rows);
    }
}
impl ProbeReceipts for Probes {
    fn pending(&self) -> Vec<Value> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Self::read(&self.outbox)
    }
    fn statuses(&self) -> Vec<Value> {
        self.statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn published(&self, receipts: &[Value]) {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows = Self::read(&self.outbox);
        rows.retain(|r| !receipts.contains(r));
        Self::write(&self.outbox, &rows);
    }
    fn statuses_published(&self, statuses: &[Value]) {
        let mut pending = self.statuses.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|row| !statuses.contains(row));
    }
}

/// Reads as the representative through the App Service token (masquerade).
struct Reader {
    client: reqwest::Client,
    origin: Url,
    token: String,
    user: String,
}
impl Reader {
    fn new(appservice: &Appservice) -> Option<Self> {
        Some(Self {
            client: reqwest::Client::builder()
                .redirect(Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(20))
                .build()
                .ok()?,
            origin: Url::parse(&appservice.homeserver).ok()?,
            token: appservice.as_token.clone(),
            user: appservice.representative.clone(),
        })
    }
    async fn get(&self, segments: &[&str]) -> Result<Option<Value>, ()> {
        self.call_as(&self.user, reqwest::Method::GET, segments, &[], None)
            .await
    }
    /// One App Service request acting as `user` (a member of the fleet namespace).
    async fn call_as(
        &self,
        user: &str,
        method: reqwest::Method,
        segments: &[&str],
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> Result<Option<Value>, ()> {
        let mut url = self.origin.clone();
        url.path_segments_mut().map_err(|_| ())?.extend(segments);
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("user_id", user);
            for (k, v) in query {
                pairs.append_pair(k, v);
            }
        }
        let mut request = self.client.request(method, url).bearer_auth(&self.token);
        if let Some(body) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(&body).map_err(|_| ())?);
        }
        let response = request.send().await.map_err(|_| ())?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if response.status() != StatusCode::OK {
            return Err(());
        }
        let bytes = response.bytes().await.map_err(|_| ())?;
        if bytes.len() > 1024 * 1024 {
            return Err(());
        }
        serde_json::from_slice(&bytes).map(Some).map_err(|_| ())
    }
    async fn event(&self, room: &str, event: &str) -> Result<Value, ()> {
        self.get(&["_matrix", "client", "v3", "rooms", room, "event", event])
            .await?
            .ok_or(())
    }
    /// TS `plaintextPrivate` reads, in the shape `probe::decide` takes.
    async fn room_facts(&self, room: &str) -> Result<Value, ()> {
        let members = self
            .get(&["_matrix", "client", "v3", "rooms", room, "joined_members"])
            .await?
            .ok_or(())?;
        let rules = self
            .get(&[
                "_matrix",
                "client",
                "v3",
                "rooms",
                room,
                "state",
                "m.room.join_rules",
                "",
            ])
            .await?
            .ok_or(())?;
        let encryption = self
            .get(&[
                "_matrix",
                "client",
                "v3",
                "rooms",
                room,
                "state",
                "m.room.encryption",
                "",
            ])
            .await?
            .unwrap_or(Value::Null);
        Ok(
            json!({"joined": members.get("joined").cloned().unwrap_or(Value::Null),
            "join_rules": rules, "encryption": encryption}),
        )
    }
}

impl Reader {
    async fn state_as(
        &self,
        user: &str,
        room: &str,
        kind: &str,
        key: &str,
    ) -> Result<Option<Value>, ()> {
        self.call_as(
            user,
            reqwest::Method::GET,
            &["_matrix", "client", "v3", "rooms", room, "state", kind, key],
            &[],
            None,
        )
        .await
    }
    /// The authority facts `verify_request` checks, read fresh as `user`
    /// (TS `plaintextPrivate` / `verifyFleetTarget` / the private-owner reads).
    async fn observe(
        &self,
        user: &str,
        room: &str,
        binding: Option<&str>,
    ) -> Result<hagency_core::authority::RoomObservation, ()> {
        let members = self
            .call_as(
                user,
                reqwest::Method::GET,
                &["_matrix", "client", "v3", "rooms", room, "joined_members"],
                &[],
                None,
            )
            .await?
            .ok_or(())?;
        let joined = members
            .get("joined")
            .and_then(Value::as_object)
            .ok_or(())?
            .keys()
            .cloned()
            .collect();
        let rules = self
            .state_as(user, room, "m.room.join_rules", "")
            .await?
            .ok_or(())?;
        let encryption = self.state_as(user, room, "m.room.encryption", "").await?;
        let levels = self
            .state_as(user, room, "m.room.power_levels", "")
            .await?
            .unwrap_or(Value::Null);
        let name = self.state_as(user, room, "m.room.name", "").await?;
        let binding = match binding {
            Some(fleet) => {
                self.state_as(user, room, "com.hagency.admin.binding.v1", fleet)
                    .await?
            }
            None => None,
        };
        Ok(hagency_core::authority::RoomObservation {
            room_id: room.to_owned(),
            joined,
            invite_only: rules.get("join_rule").and_then(Value::as_str) == Some("invite"),
            encryption: encryption.and_then(|e| {
                e.get("algorithm")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }),
            powers: levels
                .get("users")
                .and_then(Value::as_object)
                .map(|u| {
                    u.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_i64()?)))
                        .collect()
                })
                .unwrap_or_default(),
            default_power: levels
                .get("users_default")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            invite_power: levels.get("invite").and_then(Value::as_i64).unwrap_or(0),
            binding,
            name: name.and_then(|n| n.get("name").and_then(Value::as_str).map(str::to_owned)),
        })
    }
}

/// Admit one Palpo agent request (TS `POST /api/fleet/v1/requests`): the source
/// event is re-read, the reception, target project and private approval room
/// are observed fresh, and the port's own `verify_request` decides. An admitted
/// request becomes a pending engagement for the operator's console verdict.
async fn verify_project_request(
    reader: &Reader,
    domain: &DomainStore,
    fleet: &str,
    payload: &Value,
) -> Result<hagency_core::authority::VerifiedRequest, String> {
    use hagency_core::authority::{
        ProjectRequest, RequestObservation, SourceObservation, verify_request,
    };
    let request: ProjectRequest =
        serde_json::from_value(payload.clone()).map_err(|e| format!("request shape: {e}"))?;
    let registration = domain
        .provisioning_registration(fleet.to_owned())
        .await
        .map_err(|_| "fleet registration unavailable".to_owned())?;
    let rep = registration.representative_mxid.clone();
    let event = reader
        .call_as(
            &rep,
            reqwest::Method::GET,
            &[
                "_matrix",
                "client",
                "v3",
                "rooms",
                &request.source_room_id,
                "event",
                &request.source_event_id,
            ],
            &[],
            None,
        )
        .await
        .map_err(|_| "source event unreadable".to_owned())?
        .ok_or("source event missing")?;
    let source = SourceObservation {
        event_id: request.source_event_id.clone(),
        room_id: request.source_room_id.clone(),
        sender: event
            .get("sender")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        event_type: event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        content: event.get("content").cloned().unwrap_or(Value::Null),
    };
    let reception = reader
        .observe(&rep, &request.source_room_id, None)
        .await
        .map_err(|_| "reception unreadable".to_owned())?;
    let project = reader
        .observe(&rep, &request.target_room_id, Some(fleet))
        .await
        .map_err(|_| "project room unreadable (is the representative joined?)".to_owned())?;
    let owner_room = reader
        .observe(
            &registration.approval_bot_mxid,
            &request.owner_dm_room_id,
            None,
        )
        .await
        .map_err(|_| {
            "private approval room unreadable (has the approval bot joined?)".to_owned()
        })?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    let observation = RequestObservation {
        registration_generation: registration.generation,
        observed_at_ms: now,
        source,
        reception,
        project,
        owner_room,
    };
    let verified = verify_request(&registration, request, observation)
        .map_err(|e| format!("verification: {}", e.0))?;
    Ok(verified)
}
async fn admit_request(
    reader: &Reader,
    domain: &DomainStore,
    fleet: &str,
    payload: &Value,
) -> Result<String, String> {
    let verified = verify_project_request(reader, domain, fleet, payload).await?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    let engagement = domain
        .admit(verified, now)
        .await
        .map_err(|_| "admission unavailable".to_owned())?;
    Ok(engagement.id)
}

fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default() as i64;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn attempt(lane: &str) -> String {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    format!(
        "host_{lane}_{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

enum Outcome {
    Idle,
    Done,
    Later,
}

/// Matrix lane: record the probe events of one relayed transaction.
async fn matrix_once(
    adapter: &Adapter,
    probes: &Probes,
    fleet: &str,
    representative: &str,
    generation: u64,
) -> Outcome {
    let work = match adapter.take(Lane::Matrix, attempt("matrix")).await {
        Ok(Some(work)) => work,
        Ok(None) => return Outcome::Idle,
        Err(error) => {
            eprintln!("palpo matrix: claim refused: {error:?}");
            return Outcome::Later;
        }
    };
    let transaction = work
        .payload
        .get("transactionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let events = work
        .payload
        .get("body")
        .and_then(|b| b.get("events"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut recorded = 0;
    for event in events {
        let content = event.get("content").cloned().unwrap_or(Value::Null);
        let challenge = content
            .get("challenge")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if event.get("type").and_then(Value::as_str) != Some(PROBE_EVENT)
            || event.get("sender").and_then(Value::as_str) != Some(representative)
            || content.get("fleetId").and_then(Value::as_str) != Some(fleet)
            || !(16..=128).contains(&challenge.len())
            || !challenge
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            continue;
        }
        let (Some(room), Some(id)) = (
            event.get("room_id").and_then(Value::as_str),
            event.get("event_id").and_then(Value::as_str),
        ) else {
            continue;
        };
        probes.record_event(json!({"sourceRoomId": room, "sourceEventId": id, "challenge": challenge,
            "receivedAt": now_iso(), "mode": "edge", "generation": generation, "transactionId": transaction}));
        recorded += 1;
    }
    match adapter
        .complete(work.ticket, json!({"probes": recorded}))
        .await
    {
        Ok(()) => Outcome::Done,
        Err(_) => Outcome::Later,
    }
}

async fn workflow_command(
    adapter: &Adapter,
    domain: &DomainStore,
    reader: &Reader,
    fleet: &str,
    payload: &Value,
    cancel: &CancellationToken,
) -> Result<hagency_core::project_commands::ProjectReceipt, ()> {
    use hagency_core::project_commands::{ProjectAuthorization, ProjectCommand};
    let command = ProjectCommand::decode(payload.clone()).map_err(|_| ())?;
    let registration = domain
        .provisioning_registration(fleet.to_owned())
        .await
        .map_err(|_| ())?;
    command.validate(&registration).map_err(|_| ())?;
    if let Some(receipt) = domain
        .project_command_receipt(command.clone(), registration.clone())
        .await
        .map_err(|_| ())?
    {
        return Ok(receipt);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| ())?
        .as_millis() as u64;
    // Expiry is a local, definitive refusal; it must not wait indefinitely for
    // Matrix or an unreachable Palpo. This placeholder can never authorize an
    // operation; the writer checks expiry again after acquiring its lock.
    if command.expires_at_ms <= now {
        let expired = ProjectAuthorization {
            v: 1,
            command_id: command.command_id.clone(),
            command_digest: command.digest().map_err(|_| ())?,
            allowed: false,
            valid_until_ms: 0,
        };
        return domain
            .apply_project_command(command, registration, expired, None)
            .await
            .map_err(|_| ());
    }
    let mut authorization = adapter
        .authorize_project_command(&command, domain, cancel)
        .await
        .map_err(|_| ())?;
    let proof = if authorization.allowed
        && let Some(request) = command.request()
    {
        let proof = verify_project_request(
            reader,
            domain,
            fleet,
            &serde_json::to_value(request).map_err(|_| ())?,
        )
        .await
        .map_err(|_| ())?;
        // Matrix observations can be slow. Recheck current Palpo roles after
        // them, immediately before the atomic admission/decision transaction.
        authorization = adapter
            .authorize_project_command(&command, domain, cancel)
            .await
            .map_err(|_| ())?;
        Some(proof)
    } else {
        None
    };
    domain
        .apply_project_command(command, registration, authorization, proof)
        .await
        .map_err(|_| ())
}

/// Work lane: verify one probe and bind the reception.
async fn work_once(
    adapter: &Adapter,
    probes: &Probes,
    domain: &DomainStore,
    reader: &Reader,
    fleet: &str,
    cancel: &CancellationToken,
) -> Outcome {
    let work = match adapter.take(Lane::Work, attempt("work")).await {
        Ok(Some(work)) => work,
        Ok(None) => return Outcome::Idle,
        Err(error) => {
            // Never silent: a refused claim looks exactly like an empty lane.
            eprintln!("palpo work: claim refused: {error:?}");
            return Outcome::Later;
        }
    };
    if work.kind == Kind::Workflow {
        return match workflow_command(adapter, domain, reader, fleet, &work.payload, cancel).await {
            Ok(receipt) => match adapter
                .complete(
                    work.ticket,
                    serde_json::to_value(receipt).expect("fixed receipt"),
                )
                .await
            {
                Ok(()) => Outcome::Done,
                Err(_) => Outcome::Later,
            },
            Err(()) => {
                let _ = adapter.retry_later(work.ticket).await;
                Outcome::Later
            }
        };
    }
    if work.kind == Kind::Request {
        if let Some(id) = work.payload.get("requestId").and_then(Value::as_str) {
            probes.remember_request(id);
        }
        return match admit_request(reader, domain, fleet, &work.payload).await {
            Ok(engagement) => {
                eprintln!("palpo request admitted as {engagement} (pending the console verdict)");
                match adapter
                    .complete(work.ticket, json!({"engagementId": engagement}))
                    .await
                {
                    Ok(()) => Outcome::Done,
                    Err(_) => Outcome::Later,
                }
            }
            Err(reason) => {
                // TS keeps the request at submission_pending and retries; the
                // bridge never refuses it on a transient read.
                eprintln!("palpo request not admitted yet: {reason}");
                let _ = adapter.retry_later(work.ticket).await;
                Outcome::Later
            }
        };
    }
    if work.kind != Kind::Probe {
        let _ = adapter.retry_later(work.ticket).await;
        return Outcome::Later;
    }
    let body = work.payload.clone();
    let id = body
        .get("sourceEventId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let Some(seen) = probes.event(&id) else {
        // The relayed transaction has not been processed yet (TS probe_pending).
        let _ = adapter.retry_later(work.ticket).await;
        return Outcome::Later;
    };
    let receipt = ProbeReceipt {
        source_room_id: seen["sourceRoomId"].as_str().unwrap_or_default().to_owned(),
        source_event_id: id.clone(),
        challenge: seen["challenge"].as_str().unwrap_or_default().to_owned(),
    };
    let verified = async {
        let registration = domain
            .provisioning_registration(fleet.to_owned())
            .await
            .map_err(|_| None)?;
        let event = reader
            .event(&receipt.source_room_id, &id)
            .await
            .map_err(|_| None)?;
        let facts = reader
            .room_facts(&receipt.source_room_id)
            .await
            .map_err(|_| None)?;
        let bound = decide(&registration, Some(&receipt), &body, &event, &facts).map_err(Some)?;
        domain
            .bind_reception(
                registration.fleet_id.clone(),
                registration.generation,
                bound.source_room_id.clone(),
            )
            .await
            .map_err(|_| None)?;
        Ok::<_, Option<ProbeError>>(bound)
    }
    .await;
    match verified {
        Ok(bound) => {
            let published = json!({"v": 1, "received": true, "fleetId": fleet,
                "sourceRoomId": bound.source_room_id, "sourceEventId": bound.source_event_id,
                "challenge": bound.challenge, "receivedAt": seen["receivedAt"], "mode": "edge",
                "generation": seen["generation"]});
            probes.queue_receipt(published.clone());
            match adapter.complete(work.ticket, published).await {
                Ok(()) => Outcome::Done,
                Err(_) => Outcome::Later,
            }
        }
        Err(refusal) => {
            eprintln!(
                "palpo probe {id} not verified yet: {}",
                refusal
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "Matrix or store unavailable".into())
            );
            let _ = adapter.retry_later(work.ticket).await;
            Outcome::Later
        }
    }
}

/// The fleet's approval bot accepts invites to private approval rooms (TS: the
/// bridge bot's invite poll, `handleBotInvite`, which in audit mode accepts and
/// lets the later verification decide). Accepted: an encrypted, invite-only
/// room, invited by a local human outside the fleet's own namespace. Palpo then
/// verifies the room holds exactly the owner and this bot.
async fn approval_invites_once(reader: &Reader, bot: &str, fleet: &str, server: &str) {
    let filter = r#"{"room":{"timeline":{"limit":0},"ephemeral":{"types":[]},"account_data":{"types":[]}},"presence":{"types":[]},"account_data":{"types":[]}}"#;
    let Ok(Some(sync)) = reader
        .call_as(
            bot,
            reqwest::Method::GET,
            &["_matrix", "client", "v3", "sync"],
            &[("timeout", "0"), ("filter", filter)],
            None,
        )
        .await
    else {
        return;
    };
    let Some(invites) = sync.pointer("/rooms/invite").and_then(Value::as_object) else {
        return;
    };
    for (room, invite) in invites {
        let state = invite
            .pointer("/invite_state/events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let find = |kind: &str| {
            state
                .iter()
                .find(|e| e.get("type").and_then(Value::as_str) == Some(kind))
        };
        let encrypted = find("m.room.encryption")
            .and_then(|e| e.pointer("/content/algorithm"))
            .and_then(Value::as_str)
            == Some("m.megolm.v1.aes-sha2");
        let invite_only = find("m.room.join_rules")
            .and_then(|e| e.pointer("/content/join_rule"))
            .and_then(Value::as_str)
            == Some("invite");
        let inviter = state
            .iter()
            .find(|e| {
                e.get("type").and_then(Value::as_str) == Some("m.room.member")
                    && e.get("state_key").and_then(Value::as_str) == Some(bot)
            })
            .and_then(|e| e.get("sender").and_then(Value::as_str))
            .unwrap_or_default();
        let local_human =
            inviter.ends_with(&format!(":{server}")) && !inviter.starts_with(&format!("@{fleet}_"));
        if !(encrypted && invite_only && local_human) {
            eprintln!(
                "palpo approval bot: leaving invite to {room} pending (encrypted={encrypted} invite_only={invite_only} inviter={inviter})"
            );
            continue;
        }
        let joined = reader
            .call_as(
                bot,
                reqwest::Method::POST,
                &["_matrix", "client", "v3", "join", room],
                &[],
                Some(json!({})),
            )
            .await;
        eprintln!(
            "palpo approval bot: join {room} invited by {inviter}: {}",
            if joined.is_ok() {
                "ok"
            } else {
                "failed, retrying"
            }
        );
    }
}

fn retirement_candidate<T>(candidates: &[T], round: usize) -> Option<&T> {
    (!candidates.is_empty()).then(|| &candidates[round % candidates.len()])
}

#[test]
fn retirement_pages_do_not_starve_behind_a_refused_identity() {
    let pages = [vec!["refused-a", "b"], vec!["refused-c", "d"]];
    for page in pages {
        assert_eq!(
            (0..2)
                .map(|round| *retirement_candidate(&page, round).unwrap())
                .collect::<Vec<_>>(),
            page
        );
    }
    assert_eq!(retirement_candidate::<u8>(&[], 1), None);
}

/// The fleet's request statuses in the TS `fleetPublicEngagement` shape, observed
/// now. Rust `reserved` is TS's approved-and-fulfilling `active` with an open
/// fulfillment phase; `ready` needs an active, bound agent (TS: state active and
/// bound), which the provisioning slice establishes.
async fn refresh_statuses(
    adapter: &Adapter,
    domain: &DomainStore,
    probes: &Probes,
    fleet: &str,
    reader: &Reader,
    cancel: &CancellationToken,
) {
    use hagency_core::project::EngagementState as S;
    // Hold each page until its actual publication is acknowledged. Continuously
    // replacing it while transport retries would starve later agents forever.
    if !probes.statuses().is_empty() {
        return;
    }
    let cursor = probes
        .status_cursor
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let Ok(registration) = domain.provisioning_registration(fleet.to_owned()).await else {
        return;
    };
    let Ok(engagements) = domain
        .palpo_status_page(registration.clone(), cursor, 25)
        .await
    else {
        return;
    };
    *probes
        .status_cursor
        .lock()
        .unwrap_or_else(|e| e.into_inner()) =
        engagements.last().map(|e| e.id.clone()).unwrap_or_default();
    if engagements.is_empty() {
        let mut round = probes
            .retirement_round
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *round = round.wrapping_add(1);
        return;
    }
    let observed = now_iso();
    let mut out = Vec::new();
    let mut retirements = Vec::new();
    for e in engagements {
        if !probes.palpo_request(&e.request_id)
            && !domain
                .is_project_command_agent(registration.clone(), e.id.clone())
                .await
                .unwrap_or(false)
        {
            continue;
        }
        let Ok(Some((context, _, _))) = domain
            .provisioning_request_evidence(fleet.to_owned(), e.request_id.clone())
            .await
        else {
            continue;
        };
        let Ok(c) = serde_json::from_str::<Value>(&context) else {
            continue;
        };
        let Ok(lifecycle) = domain
            .palpo_agent_lifecycle(registration.clone(), e.id.clone())
            .await
        else {
            continue;
        };
        if lifecycle.runtime_stopped && lifecycle.agent_mxid.is_some() && !lifecycle.matrix_retired
        {
            retirements.push((e.id.clone(), out.len()));
        }
        let resource = domain
            .resource_configuration(e.resource_id.clone())
            .await
            .ok();
        let (state, phase, bound) = match e.state {
            S::Pending => ("pending", None, false),
            S::Reserved => ("active", Some("provisioning"), false),
            S::Active => ("active", Some("complete"), true),
            S::Rejected => ("rejected", None, false),
            S::Revoked | S::Failed => ("ended", None, false),
        };
        // ADR-186 §A4/§C4: the granted amount, raised by any top-up; the
        // request when the operator granted it unchanged.
        let allocated = matches!(e.state, S::Reserved | S::Active).then(|| json!(e.allocation()));
        // The serving identity is the fleet-namespaced account the App Service
        // factory created for this engagement; `ready` is TS's rule (active and
        // bound) plus the observed fact the agent is joined in the target room.
        let agent = lifecycle.agent_mxid.clone();
        let target = c["targetRoomId"].as_str().unwrap_or_default().to_owned();
        let joined = bound
            && agent.is_some()
            && reader
                .get(&[
                    "_matrix",
                    "client",
                    "v3",
                    "rooms",
                    &target,
                    "joined_members",
                ])
                .await
                .ok()
                .flatten()
                .is_some_and(|m| {
                    m.pointer(&format!(
                        "/joined/{}",
                        agent
                            .as_deref()
                            .unwrap_or_default()
                            .replace('~', "~0")
                            .replace('/', "~1")
                    ))
                    .is_some()
                });
        out.push(json!({
            "v": 1, "fleetId": fleet, "requestId": e.request_id, "engagementId": e.id, "state": state,
            "targetProjectId": c["targetProjectId"], "targetRoomId": c["targetRoomId"],
            "sourceRoomId": c["sourceRoomId"], "sourceEventId": c["sourceEventId"], "role": e.role,
            "agentDefinition": c["agentDefinition"], "requestedTokens": c["requestedTokens"],
            "allocatedTokens": allocated, "agentMxid": agent, "bound": bound,
            "serving": resource.map(|r| json!({"framework": r.framework, "model": r.model,
                "reasoning": r.reasoning})),
            "fulfillment": phase.map(|p| json!({"phase": p, "incomplete": false})),
            "ready": joined, "decidedAt": null, "endedAt": null, "observedAt": observed,
            "lifecycle": lifecycle,
        }));
    }
    // One bounded call per page; rotate across each complete status scan so a
    // permanently refused first identity cannot starve the rest of that page.
    // Lost responses repeat the exact target, and only verified replies journal.
    let round = *probes
        .retirement_round
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some((id, index)) = retirement_candidate(&retirements, round) {
        if adapter
            .retire_project_agent(id.clone(), domain, cancel)
            .await
            .is_ok()
        {
            if let Ok(latest) = domain.palpo_agent_lifecycle(registration, id.clone()).await {
                out[*index]["lifecycle"] = json!(latest);
            }
        }
    }
    *probes.statuses.lock().unwrap_or_else(|e| e.into_inner()) = out;
}

/// Runs beside the transport lanes until cancelled.
pub(super) async fn run(
    adapter: &Adapter,
    probes: &Probes,
    domain: &DomainStore,
    appservice: &Appservice,
    fleet: &str,
    generation: u64,
    cancel: &CancellationToken,
) {
    let Some(reader) = Reader::new(appservice) else {
        eprintln!("palpo work: invalid homeserver in palpo-appservice.json; probes wait");
        return;
    };
    let server = appservice
        .representative
        .split_once(':')
        .map(|(_, s)| s.to_owned())
        .unwrap_or_default();
    let bot = domain
        .provisioning_registration(fleet.to_owned())
        .await
        .map(|r| r.approval_bot_mxid)
        .unwrap_or_default();
    let mut invites_due = std::time::Instant::now();
    // A work item that could not finish is retried on a slower clock: each
    // retry is a new custody attempt row, and those are finite.
    while !cancel.is_cancelled() {
        refresh_statuses(adapter, domain, probes, fleet, &reader, cancel).await;
        if !bot.is_empty() && std::time::Instant::now() >= invites_due {
            approval_invites_once(&reader, &bot, fleet, &server).await;
            invites_due = std::time::Instant::now() + Duration::from_secs(15);
        }
        let matrix = matrix_once(
            adapter,
            probes,
            fleet,
            &appservice.representative,
            generation,
        )
        .await;
        // A retried item steps aside in custody (WORK_RETRY_MS), not the
        // whole lane: the independent items behind it are claimed now.
        let work = work_once(adapter, probes, domain, &reader, fleet, cancel).await;
        let pause = match (matrix, work) {
            (Outcome::Done, _) | (_, Outcome::Done) => Duration::from_millis(100),
            (_, Outcome::Later) | (Outcome::Later, _) => Duration::from_secs(3),
            _ => Duration::from_secs(1),
        };
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(pause) => {}
        }
    }
}
