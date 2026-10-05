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
//! - Work lane, `request`: fresh Matrix evidence precedes admission. Delegated
//!   coordinator decisions reserve capacity and queue normal provisioning;
//!   legacy requests retain their explicitly configured console workflow.
//!
//! Transport/read failures retry. A deterministic coordinator admission refusal
//! has a durable receipt; it does not assert that the connection is dead.
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

/// The fleet's App Service identity, from `palpo-appservice.json`.
pub(super) struct Appservice {
    homeserver: String,
    as_token: String,
    representative: String,
    matrix_root: Option<Vec<u8>>,
}
impl Appservice {
    pub(super) fn load(state: &Path, server_name: &str) -> Option<Self> {
        let raw = private_json(&state.join("palpo-appservice.json"), 65536)?;
        let value: Value = serde_json::from_slice(&raw).ok()?;
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        Some(Self {
            homeserver: text("homeserver")?,
            as_token: text("as_token")?,
            representative: format!("@{}:{server_name}", text("sender_localpart")?),
            matrix_root: crate::bootstrap::config::matrix_root(state).ok()?,
        })
    }
}

fn private_json(path: &Path, limit: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = private::open(path, false).ok()?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

pub(super) async fn verify_coordinator_account(state: &Path, server: &str, user: &str) -> bool {
    if !user.starts_with('@')
        || user.starts_with("@hf_")
        || user.split_once(':').map(|(_, s)| s) != Some(server)
    {
        return false;
    }
    let Some(aservice) = Appservice::load(state, server) else {
        return false;
    };
    let Some(reader) = Reader::new(&aservice) else {
        return false;
    };
    matches!(
        reader
            .get(&["_matrix", "client", "v3", "profile", user])
            .await,
        Ok(Some(_))
    )
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
    runtime: Mutex<Option<super::fleet::Routes>>,
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
            runtime: Mutex::new(None),
        })
    }
    pub(super) fn attach_runtime(&self, routes: super::fleet::Routes) {
        *self.runtime.lock().unwrap_or_else(|e| e.into_inner()) = Some(routes);
    }
    fn runtime_availability(&self, agent: &str) -> &'static str {
        self.runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|runtime| runtime.agent_availability(agent))
            .unwrap_or("not_attached")
    }
    fn read(path: &Path) -> Vec<Value> {
        private_json(path, 1024 * 1024)
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
    fn statuses_published(&self, statuses: &[Value]) {
        let mut pending = self.statuses.lock().unwrap_or_else(|e| e.into_inner());
        if pending.as_slice() == statuses {
            pending.clear();
        }
    }
    fn published(&self, receipts: &[Value]) {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows = Self::read(&self.outbox);
        rows.retain(|r| !receipts.contains(r));
        Self::write(&self.outbox, &rows);
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
        let mut client = reqwest::Client::builder()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20));
        if let Some(pem) = &appservice.matrix_root {
            client = client.add_root_certificate(reqwest::Certificate::from_pem(pem).ok()?);
        }
        Some(Self {
            client: client.build().ok()?,
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
/// request becomes a pending engagement. A scoped coordinator decision reserves
/// capacity and queues provisioning here; only legacy requests await the console.
async fn verify_project_request(
    reader: &Reader,
    domain: &DomainStore,
    fleet: &str,
    payload: &Value,
) -> Result<hagency_core::authority::VerifiedRequest, AdmissionError> {
    use hagency_core::authority::{
        ProjectRequest, RequestObservation, SourceObservation, verify_request,
    };
    let request: ProjectRequest = serde_json::from_value(payload.clone())
        .map_err(|_| AdmissionError::Refused("invalid_request"))?;
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
    verify_request(&registration, request, observation)
        .map_err(|_| AdmissionError::Refused("invalid_request"))
}

#[derive(Debug, thiserror::Error)]
enum AdmissionError {
    #[error("{0}")]
    Pending(String),
    #[error("{0}")]
    Refused(&'static str),
}
impl From<String> for AdmissionError {
    fn from(error: String) -> Self {
        Self::Pending(error)
    }
}
impl From<&str> for AdmissionError {
    fn from(error: &str) -> Self {
        Self::Pending(error.into())
    }
}
impl From<AdmissionError> for String {
    fn from(error: AdmissionError) -> Self {
        error.to_string()
    }
}
impl From<hagency_store::Error> for AdmissionError {
    fn from(error: hagency_store::Error) -> Self {
        match hagency_store::coordinator::coordinator_refusal_reason(&error) {
            Some(code) => Self::Refused(code),
            None => Self::Pending(error.to_string()),
        }
    }
}
/// Each attempt uses the original approved rooms. Matrix failures are durable
/// setup results; another authenticated recovery can retry without a new grant.
async fn run_project_setup(
    reader: &Reader,
    domain: &DomainStore,
    command: hagency_store::coordinator::ProjectSetupCommand,
) -> Result<Value, AdmissionError> {
    let work = domain
        .begin_project_setup(command.clone())
        .await
        .map_err(AdmissionError::from)?;
    if let Some(done) = work.completed {
        return Ok(done);
    }
    let result = async {
        let fleet = command.context.server_engagement_id.as_str();
        let registration = domain
            .provisioning_registration(fleet.to_owned())
            .await
            .map_err(|_| "setup_unavailable")?;
        for (user, room, reason) in [
            (
                &registration.representative_mxid,
                &work.definition.room_id,
                "project_membership_pending",
            ),
            (
                &registration.approval_bot_mxid,
                &work.definition.owner_dm_room_id,
                "private_membership_pending",
            ),
        ] {
            domain
                .validate_project_setup(command.clone())
                .await
                .map_err(|_| "authority_changed")?;
            reader
                .call_as(
                    user,
                    reqwest::Method::POST,
                    &["_matrix", "client", "v3", "join", room],
                    &[],
                    Some(json!({})),
                )
                .await
                .map_err(|_| reason)?
                .ok_or(reason)?;
        }
        domain
            .validate_project_setup(command.clone())
            .await
            .map_err(|_| "authority_changed")?;
        let project = reader
            .observe(
                &registration.representative_mxid,
                &work.definition.room_id,
                Some(fleet),
            )
            .await
            .map_err(|_| "project_room_unreadable")?;
        domain
            .validate_project_setup(command.clone())
            .await
            .map_err(|_| "authority_changed")?;
        let owner_room = reader
            .observe(
                &registration.approval_bot_mxid,
                &work.definition.owner_dm_room_id,
                None,
            )
            .await
            .map_err(|_| "private_room_unreadable")?;
        domain
            .validate_project_setup(command.clone())
            .await
            .map_err(|_| "authority_changed")?;
        let observed_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "setup_unavailable")?
            .as_millis() as u64;
        domain
            .coordinator_project_ready(hagency_store::coordinator::ProjectReadiness {
                registration,
                project_id: command.project_id.as_str().into(),
                observed_at_ms,
                project,
                owner_room,
            })
            .await
            .map_err(|_| "room_authority_changed")?;
        Ok::<(), &str>(())
    }
    .await;
    domain
        .finish_project_setup(command, result.err().map(String::from))
        .await
        .map_err(AdmissionError::from)
}

async fn admit_request(
    reader: &Reader,
    domain: &DomainStore,
    fleet: &str,
    payload: &Value,
) -> Result<String, AdmissionError> {
    let verified = verify_project_request(reader, domain, fleet, payload).await?;
    if payload.get("coordinatorApproval").is_some() {
        domain
            .verify_coordinator_project(verified.clone())
            .await
            .map_err(AdmissionError::from)?;
    }
    let engagement = domain
        .admit(
            verified.clone(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or_default(),
        )
        .await
        .map_err(AdmissionError::from)?;
    if let Some(command) = payload.get("coordinatorApproval") {
        let command: hagency_store::coordinator::AgentApproval =
            serde_json::from_value(command.clone())
                .map_err(|_| AdmissionError::Refused("invalid_request"))?;
        domain
            .approve_coordinated_agent(command, verified)
            .await
            .map_err(AdmissionError::from)?;
    }
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

/// Work lane: verify one probe and bind the reception.
async fn work_once(
    adapter: &Adapter,
    probes: &Probes,
    domain: &DomainStore,
    reader: &Reader,
    fleet: &str,
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
    if work.kind == Kind::Request {
        if work.payload["operation"] == "coordinator_agent_control" {
            let result = async {
                let command: hagency_store::coordinator::AgentControl =
                    serde_json::from_value(work.payload["command"].clone())
                        .map_err(|_| AdmissionError::Refused("invalid_request"))?;
                if let Some(outcome) = domain
                    .coordinator_command_outcome(fleet.into(), work.payload.clone())
                    .await
                    .map_err(AdmissionError::from)?
                {
                    return Ok(outcome);
                }
                domain
                    .control_coordinator_agent(fleet.into(), command)
                    .await
                    .map_err(AdmissionError::from)
            }
            .await;
            let result = match result {
                Ok(receipt) => Ok(receipt),
                Err(AdmissionError::Refused(reason)) => domain
                    .refuse_coordinator_command(fleet.into(), work.payload.clone(), reason.into())
                    .await
                    .map_err(AdmissionError::from),
                Err(e) => Err(e),
            };
            return match result {
                Ok(receipt) => match adapter.complete(work.ticket, receipt).await {
                    Ok(()) => Outcome::Done,
                    Err(_) => Outcome::Later,
                },
                Err(error) => {
                    eprintln!("palpo agent control: {error}");
                    let _ = adapter.retry_later(work.ticket).await;
                    Outcome::Later
                }
            };
        }
        if matches!(
            work.payload["operation"].as_str(),
            Some("coordinator_project_approval" | "coordinator_token_top_up")
        ) {
            match domain
                .receive_coordinator_command(fleet.into(), work.payload.clone())
                .await
            {
                Ok(Some(result))
                    if result["state"] == "refused"
                        || work.payload["operation"] == "coordinator_token_top_up" =>
                {
                    return match adapter.complete(work.ticket, result).await {
                        Ok(()) => Outcome::Done,
                        Err(_) => Outcome::Later,
                    };
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("palpo command binding refused: {error}");
                    let _ = adapter.retry_later(work.ticket).await;
                    return Outcome::Later;
                }
            }
        }
        if work.payload["operation"] == "coordinator_token_top_up" {
            let result = async {
                let command: hagency_store::coordinator::TokenTopUpApproval =
                    serde_json::from_value(work.payload["command"].clone())
                        .map_err(|_| AdmissionError::Refused("invalid_request"))?;
                if command.context.server_engagement_id.as_str() != fleet {
                    return Err(AdmissionError::Refused("invalid_request"));
                }
                let agent = domain
                    .engagement(command.request.agent_allocation_id.as_str().into())
                    .await
                    .map_err(AdmissionError::from)?;
                let (context, _, _) = domain
                    .provisioning_request_evidence(fleet.into(), agent.request_id)
                    .await
                    .map_err(AdmissionError::from)?
                    .ok_or_else(|| "agent evidence missing".to_owned())?;
                let request: Value = serde_json::from_str(&context)
                    .map_err(|_| "agent evidence invalid".to_owned())?;
                let proof = verify_project_request(reader, domain, fleet, &request).await?;
                domain
                    .approve_coordinator_top_up(command, proof)
                    .await
                    .map_err(AdmissionError::from)
            }
            .await;
            return match result {
                Ok(agent) => match adapter
                    .complete(
                        work.ticket,
                        json!({"engagementId":agent.id,"allocatedTokens":agent.allocation()}),
                    )
                    .await
                {
                    Ok(()) => Outcome::Done,
                    Err(_) => Outcome::Later,
                },
                Err(AdmissionError::Refused(reason)) => {
                    match domain
                        .refuse_coordinator_command(
                            fleet.into(),
                            work.payload.clone(),
                            reason.into(),
                        )
                        .await
                    {
                        Ok(receipt) => match adapter.complete(work.ticket, receipt).await {
                            Ok(()) => Outcome::Done,
                            Err(_) => Outcome::Later,
                        },
                        Err(_) => {
                            let _ = adapter.retry_later(work.ticket).await;
                            Outcome::Later
                        }
                    }
                }
                Err(reason) => {
                    eprintln!("palpo coordinator top-up: {reason}");
                    let _ = adapter.retry_later(work.ticket).await;
                    Outcome::Later
                }
            };
        }
        if matches!(
            work.payload["operation"].as_str(),
            Some("coordinator_project_approval" | "coordinator_project_setup")
        ) {
            let result = async {
                let setup = if work.payload["operation"] == "coordinator_project_approval" {
                    let command: hagency_store::coordinator::ProjectApproval =
                        serde_json::from_value(work.payload["command"].clone())
                            .map_err(|_| AdmissionError::Refused("invalid_request"))?;
                    if command.context.server_engagement_id.as_str() != fleet {
                        return Err(AdmissionError::Refused("invalid_request"));
                    }
                    domain
                        .approve_coordinator_project(
                            command.clone(),
                            work.payload["definition"].clone(),
                        )
                        .await
                        .map_err(AdmissionError::from)?;
                    hagency_store::coordinator::ProjectSetupCommand {
                        context: command.context.clone(),
                        project_id: command.request.project_id,
                        project_revision: command.request.revision,
                        approval_command_id: command.context.command_id,
                    }
                } else {
                    serde_json::from_value(work.payload["command"].clone())
                        .map_err(|_| AdmissionError::Refused("invalid_request"))?
                };
                if setup.context.server_engagement_id.as_str() != fleet {
                    return Err(AdmissionError::Refused("invalid_request"));
                }
                run_project_setup(reader, domain, setup).await
            }
            .await;
            return match result {
                Ok(result) => match adapter.complete(work.ticket, result).await {
                    Ok(()) => Outcome::Done,
                    Err(_) => Outcome::Later,
                },
                Err(AdmissionError::Refused(reason)) => match domain
                    .refuse_coordinator_command(fleet.into(), work.payload.clone(), reason.into())
                    .await
                {
                    Ok(receipt) => match adapter.complete(work.ticket, receipt).await {
                        Ok(()) => Outcome::Done,
                        Err(_) => Outcome::Later,
                    },
                    Err(_) => {
                        let _ = adapter.retry_later(work.ticket).await;
                        Outcome::Later
                    }
                },
                Err(reason) => {
                    eprintln!("palpo coordinator project: {reason}");
                    let _ = adapter.retry_later(work.ticket).await;
                    Outcome::Later
                }
            };
        }
        if let Some(id) = work.payload.get("requestId").and_then(Value::as_str) {
            probes.remember_request(id);
        }
        if work.payload.get("coordinatorApproval").is_some() {
            match domain
                .receive_coordinator_agent(fleet.into(), work.payload.clone())
                .await
            {
                Ok(row) if row["state"] == "refused" || row["state"] == "applied" => {
                    return match adapter.complete(work.ticket, row).await {
                        Ok(()) => Outcome::Done,
                        Err(_) => Outcome::Later,
                    };
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("palpo decision could not be recorded: {error}");
                    let _ = adapter.retry_later(work.ticket).await;
                    return Outcome::Later;
                }
            }
        }
        return match admit_request(reader, domain, fleet, &work.payload).await {
            Ok(engagement) => {
                if work.payload.get("coordinatorApproval").is_some() {
                    eprintln!(
                        "palpo request {engagement}: coordinator decision applied; provisioning queued"
                    );
                } else {
                    eprintln!(
                        "palpo request admitted as {engagement} (legacy console verdict pending)"
                    );
                }
                match adapter
                    .complete(work.ticket, json!({"engagementId": engagement}))
                    .await
                {
                    Ok(()) => Outcome::Done,
                    Err(_) => Outcome::Later,
                }
            }
            Err(AdmissionError::Refused(reason))
                if work.payload.get("coordinatorApproval").is_some() =>
            {
                let id = work.payload["coordinatorApproval"]["context"]["commandId"]
                    .as_str()
                    .unwrap_or_default();
                match domain
                    .refuse_coordinator_agent(id.into(), reason.into())
                    .await
                {
                    Ok(row) => match adapter.complete(work.ticket, row).await {
                        Ok(()) => Outcome::Done,
                        Err(_) => Outcome::Later,
                    },
                    Err(_) => {
                        let _ = adapter.retry_later(work.ticket).await;
                        Outcome::Later
                    }
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

/// The fleet's request statuses in the TS `fleetPublicEngagement` shape, observed
/// now. Rust `reserved` is TS's approved-and-fulfilling `active` with an open
/// fulfillment phase; `ready` needs an active, bound agent (TS: state active and
/// bound), which the provisioning slice establishes.
async fn refresh_statuses(domain: &DomainStore, probes: &Probes, fleet: &str, reader: &Reader) {
    use hagency_core::project::EngagementState as S;
    // Retain one bounded page until publication acknowledges it, then advance.
    // A large first fleet must not hide later fleets or its own 101st agent.
    if !probes
        .statuses
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty()
    {
        return;
    }
    let cursor = probes
        .status_cursor
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let Ok(engagements) = domain.fleet_engagements(fleet.to_owned(), cursor, 20).await else {
        return;
    };
    *probes
        .status_cursor
        .lock()
        .unwrap_or_else(|e| e.into_inner()) =
        engagements.last().map(|e| e.id.clone()).unwrap_or_default();
    let observed = now_iso();
    let mut out = Vec::new();
    for e in engagements {
        if !probes.palpo_request(&e.request_id) {
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
        let allocated =
            matches!(e.state, S::Reserved | S::Active | S::Revoked).then(|| json!(e.allocation()));
        let usage = domain
            .coordinator_agent_usage(e.id.clone())
            .await
            .unwrap_or(Value::Null);
        let mut lifecycle = domain
            .coordinator_agent_lifecycle(e.id.clone())
            .await
            .unwrap_or(Value::Null);
        let runtime_available = probes.runtime_availability(&e.id);
        if lifecycle.is_object() {
            lifecycle["runtimeAvailability"] = json!(runtime_available);
        }
        // The serving identity is the fleet-namespaced account the App Service
        // factory created for this engagement; `ready` is TS's rule (active and
        // bound) plus the observed fact the agent is joined in the target room.
        let server = reader
            .user
            .split_once(':')
            .map(|(_, s)| s)
            .unwrap_or_default();
        let agent = format!("@{fleet}_{}:{server}", e.id);
        let target = c["targetRoomId"].as_str().unwrap_or_default().to_owned();
        let joined = bound
            && runtime_available == "available"
            && lifecycle["matrixReady"] == true
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
                        agent.replace('~', "~0").replace('/', "~1")
                    ))
                    .is_some()
                });
        out.push(json!({
            "v": 1, "fleetId": fleet, "requestId": e.request_id, "engagementId": e.id, "state": state,
            "targetProjectId": c["targetProjectId"], "targetRoomId": c["targetRoomId"],
            "sourceRoomId": c["sourceRoomId"], "sourceEventId": c["sourceEventId"], "role": e.role,
            "agentDefinition": c["agentDefinition"], "requestedTokens": c["requestedTokens"],
            "allocatedTokens": allocated, "agentMxid": if bound { json!(agent) } else { Value::Null }, "bound": bound,
            "serving": resource.map(|r| json!({"framework": r.framework, "model": r.model,
                "reasoning": r.reasoning})),
            "fulfillment": phase.map(|p| json!({"phase": p, "incomplete": false})),
            "ready": joined, "lifecycle":lifecycle, "decidedAt": null, "endedAt": null, "observedAt": observed,
            "consumedTokens":usage["consumedTokens"],"usageObservedAtMs":usage["usageObservedAtMs"],
            "usageEvidence":usage["usageEvidence"],"usageComplete":usage["usageComplete"],"quotaPaused":usage["quotaPaused"],
        }));
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
        refresh_statuses(domain, probes, fleet, &reader).await;
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
        let work = work_once(adapter, probes, domain, &reader, fleet).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_palpo_private_observation_files_survive_beyond_secret_token_size() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("private");
        private::directory(&state).unwrap();
        let probes = Probes::new(&state);
        for i in 0..25 {
            probes.remember_request(&format!("request_{i:040}"));
            probes.record_event(json!({"sourceEventId":format!("event_{i}"),"challenge":"bound-challenge","receivedAt":"2026-10-04T00:00:00Z"}));
        }
        drop(probes);
        let probes = Probes::new(&state);
        assert!(probes.palpo_request("request_0000000000000000000000000000000000000000"));
        assert!(probes.palpo_request("request_0000000000000000000000000000000000000024"));
        assert!(probes.event("event_0").is_some());
        assert!(probes.event("event_24").is_some());
        let configuration = json!({"homeserver":"https://matrix.example.test","as_token":"x".repeat(1024),"sender_localpart":"representative"});
        private::replace(
            &state.join("palpo-appservice.json"),
            &serde_json::to_vec(&configuration).unwrap(),
        )
        .unwrap();
        assert!(Appservice::load(&state, "example.test").is_some());
    }

    #[test]
    fn native_palpo_status_pages_wait_for_their_exact_publication_receipt() {
        let root = tempfile::tempdir().unwrap();
        let probes = Probes::new(root.path());
        let pending =
            json!({"requestId":"one","state":"active","observedAt":"2026-10-04T01:02:03.000Z"});
        *probes.statuses.lock().unwrap() = vec![pending.clone()];
        *probes.status_cursor.lock().unwrap() = "after_one".into();
        let old =
            json!({"requestId":"one","state":"pending","observedAt":"2026-10-04T01:01:00.000Z"});
        probes.statuses_published(&[old]);
        assert_eq!(probes.statuses(), vec![pending.clone()]);
        probes.statuses_published(&[pending]);
        assert!(probes.statuses().is_empty());
        assert_eq!(*probes.status_cursor.lock().unwrap(), "after_one");
    }
}
