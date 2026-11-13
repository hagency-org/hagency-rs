//! Private authenticated journal DTOs. Only the owned SDK constructs proofs;
//! deserialization occurs solely after authenticated journal decryption.
use crate::{Error, wire};
use hagency_core::{
    canonical,
    ingress::*,
    messages::InboundMessage,
    replies::{ReplyRoute, RoomPrivacy},
};
use matrix_sdk_base::sync::SyncResponse;
use matrix_sdk_common::deserialized_responses::{
    AlgorithmInfo, TimelineEventKind, VerificationLevel, VerificationState,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
pub(crate) mod disposition;
use disposition::{Decision, Disposition, Rejection, Source};

pub(crate) const MAX_TIMELINE: usize = 100;
/// The sync filter allows MAX_TIMELINE events per room, so the batch-wide
/// bound covers every observed room rather than one room's worth. Each room is
/// still bounded by MAX_TIMELINE on its own.
pub(crate) const MAX_TOTAL_TIMELINE: usize = MAX_TIMELINE * MAX_TARGETS;
pub(crate) const MAX_TARGETS: usize = 64;
/// A bounded retention of raw `m.room.encrypted` envelopes whose room key had
/// not arrived yet (TS `bridge-matrix.js:6646` `pendingEncryptedEventStore`).
/// Capacity refuses rather than evicting: an unresolved custody is never
/// silently dropped (TS throws and the durable sync token does not advance).
/// One room may legitimately hold a full timeline of such envelopes and a
/// batch spans several rooms; a bound below that would refuse the same batch
/// on every pass and stall intake. The durable list stays well below the SDK
/// journal bound (a few KiB per envelope against MAX_JOURNAL_BYTES).
pub(crate) const MAX_PENDING_UNDECRYPTABLE: usize = MAX_TIMELINE * 10;
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Prepared,
    Applying,
    Derived,
    Quarantined,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Batch {
    pub token: String,
    pub digest: String,
    pub sdk_identity: String,
    pub targets: Vec<ReplyRoute>,
    pub raw: Value,
    pub phase: Phase,
    pub reason: Option<String>,
    pub events: Vec<Event>,
    /// ADR-095: pre-project provisioning events, admitted by discriminator
    /// before target resolution. They carry no `ReplyRoute` (the reception
    /// room is pre-project), are acknowledged by reprocessing (provision is
    /// idempotent on `request_id`), and are never disposition rows.
    #[serde(default)]
    pub pre_project: Vec<PreProjectEvent>,
    /// ADR-065 reverted to TS (board #10): raw undecryptable envelopes retained
    /// for a later sync. Never a disposition row, never archived, bounded.
    #[serde(default)]
    pub pending: Vec<PendingEnvelope>,
    pub acknowledgements: Vec<Acknowledgement>,
    pub filtered: usize,
    #[serde(default)]
    pub dispositions: Option<Vec<Disposition>>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Event {
    pub route: ReplyRoute,
    input: Message,
    pub mentions: BTreeSet<String>,
    proof: Proof,
    #[serde(default)]
    pub(crate) attachment: Option<crate::attachments::Manifest>,
}
/// A provisioning request admitted by discriminator before target resolution
/// (ADR-095). It has no route and no scope; the request body's `requestId`
/// is the idempotency key, and `verify_request` binds its room, sender,
/// powers and binding against the store-recorded registration.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct PreProjectEvent {
    input: Message,
    proof: Proof,
}
impl PreProjectEvent {
    pub(crate) fn observation(&self) -> InboundMessage {
        self.input.observation()
    }
}
/// TS `lib/fleet-protocol.js:4,16-34`: a request carried as the custom Matrix
/// event type `com.hagency.engagement.request.v1` has the camelCase fleet
/// fields as its `content` (no `msgtype`, no `body`). `provision()` reads the
/// console-shape `body`, so this rewrites the camelCase `content` into the
/// body the msgtype carrier carries. The fields `provision()` derives itself
/// (`fleetId`, `v`, `authVersion`, `sourceRoomId`, `ownerMxid`, `ownerDmRoomId`,
/// `sourceEventId`) are omitted here exactly as the msgtype body omits them.
fn custom_request_body(content: &serde_json::Map<String, Value>) -> Result<String, Rejection> {
    let get = |key: &str| content.get(key).ok_or(Rejection::Malformed);
    let requester = get("requesterMxid")?.as_str().ok_or(Rejection::Malformed)?;
    let definition = get("agentDefinition")?
        .as_object()
        .ok_or(Rejection::Malformed)?;
    let name = definition.get("name").ok_or(Rejection::Malformed)?.clone();
    let resource = definition
        .get("resourceId")
        .ok_or(Rejection::Malformed)?
        .clone();
    let body = serde_json::json!({
        "requestId": get("requestId")?,
        "requester": requester,
        "project": get("targetProjectId")?,
        "projectRoomId": get("targetRoomId")?,
        "role": get("role")?,
        "requestedTokens": get("requestedTokens")?,
        "ratePerDay": content.get("ratePerDay").cloned().unwrap_or(Value::Null),
        "agent": name,
        "context": { "agentDefinition": { "resourceId": resource } },
    });
    Ok(body.to_string())
}
/// A derived timeline candidate: either a routed event or a pre-project
/// provisioning request admitted by discriminator before target resolution.
enum Candidate {
    Target(Box<Event>),
    PreProject(Box<PreProjectEvent>),
}
/// A raw `m.room.encrypted` envelope retained because its room key had not
/// arrived (TS `bridge-matrix.js:6646-6658`). Unlike a terminal tombstone, the
/// source stays recoverable: a later sync retries decryption and, when the key
/// arrives, the message becomes input exactly once.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct PendingEnvelope {
    pub room: String,
    pub raw: Value,
}
impl PendingEnvelope {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.room.is_empty() || !self.raw.is_object() {
            return Err(Error::Storage);
        }
        Ok(())
    }
}
/// A retained envelope whose room key has since arrived, decrypted by the
/// owned SDK and handed to `derive_with_history` (board #10). It has no raw
/// counterpart in this sync, so it becomes a candidate with the next index and
/// is admitted exactly once by the ordinary handoff.
pub(crate) struct Recovered {
    pub room: String,
    pub original: Value,
    pub value: Value,
    pub kind: TimelineEventKind,
}
#[derive(Clone, Serialize, Deserialize)]
struct Message {
    server_name: String,
    room_id: String,
    event_id: String,
    sender_mxid: String,
    thread_root: Option<String>,
    body: String,
    kind: String,
    origin_ts: u64,
}
impl Message {
    fn observation(&self) -> InboundMessage {
        InboundMessage {
            server_name: self.server_name.clone(),
            room_id: self.room_id.clone(),
            event_id: self.event_id.clone(),
            sender_mxid: self.sender_mxid.clone(),
            thread_root: self.thread_root.clone(),
            body: self.body.clone(),
            kind: self.kind.clone(),
            origin_ts: self.origin_ts,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Proof {
    Plain,
    Verified {
        sender: String,
        device: String,
        session: String,
    },
}
impl Event {
    pub(crate) fn attachment_observation(
        &self,
    ) -> Result<Option<hagency_core::attachments::MatrixAttachmentObservation>, Error> {
        self.attachment
            .as_ref()
            .map(|m| m.observation(self.observation()))
            .transpose()
    }
    pub(crate) fn observation(&self) -> MatrixEventObservation {
        MatrixEventObservation {
            scope: MatrixIngressScope::from(&self.route),
            event: self.input.observation(),
            mentions: self.mentions.clone(),
            encrypted: matches!(self.proof, Proof::Verified { .. }),
        }
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Acknowledgement {
    pub sequence: u64,
    pub session_id: String,
    pub wake: bool,
}
impl From<&MatrixIngressReceipt> for Acknowledgement {
    fn from(r: &MatrixIngressReceipt) -> Self {
        Self {
            sequence: r.sequence,
            session_id: r.session_id.clone(),
            wake: r.wake,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Receipt {
    pub token: String,
    pub digest: String,
    pub target_digest: String,
    pub acknowledgements: Vec<Acknowledgement>,
    pub filtered: usize,
    #[serde(default)]
    pub dispositions: Option<Vec<Disposition>>,
}
impl Batch {
    pub(crate) fn refuse_stale_session(
        &mut self,
        proofs: &[hagency_store::StaleMatrixSessionReceipt],
    ) -> Result<usize, Error> {
        if self.phase != Phase::Quarantined
            || self.reason.as_deref() != Some("domain refused the frozen event scope or content")
            || self.acknowledgements.len() >= self.events.len()
            || proofs.len() != self.events.len() - self.acknowledgements.len()
        {
            return Err(Error::Unsupported);
        }
        for (event, proof) in self.events[self.acknowledgements.len()..]
            .iter()
            .zip(proofs)
        {
            if event.attachment.is_some() || !proof.matches(&event.observation()) {
                return Err(Error::Generation);
            }
        }
        let dispositions = self.dispositions.as_mut().ok_or(Error::Unsupported)?;
        disposition::validate(dispositions, self.events.len(), self.filtered)?;
        let count = proofs.len();
        for value in dispositions {
            if matches!(value.decision, Decision::Candidate { index } if index >= self.acknowledgements.len())
            {
                value.decision = Decision::Rejected {
                    reason: Rejection::StaleSession,
                };
            }
        }
        self.events.truncate(self.acknowledgements.len());
        self.phase = Phase::Derived;
        self.reason = Some("stale session inputs explicitly refused".into());
        self.validate_restored(
            &self.sdk_identity,
            &self.targets[0].sender_mxid,
            &self.targets[0].device_id,
        )?;
        Ok(count)
    }
    pub(crate) fn new(
        raw: Value,
        targets: Vec<ReplyRoute>,
        identity: String,
    ) -> Result<Self, Error> {
        let token = raw
            .get("next_batch")
            .and_then(Value::as_str)
            .ok_or(Error::Wire)?
            .to_owned();
        let digest = canonical::transport_digest(&raw).map_err(|_| Error::Wire)?;
        if targets.is_empty() || targets.len() > MAX_TARGETS {
            return Err(Error::Capacity);
        }
        Ok(Self {
            token,
            digest,
            sdk_identity: identity,
            targets,
            raw,
            phase: Phase::Prepared,
            reason: None,
            events: vec![],
            pre_project: vec![],
            pending: vec![],
            acknowledgements: vec![],
            filtered: 0,
            dispositions: Some(vec![]),
        })
    }
    #[cfg(test)]
    pub(crate) fn derive(&mut self, sync: SyncResponse, history: &[Receipt]) -> Result<(), Error> {
        self.derive_with_history(sync, history, &[], &[])
    }
    pub(crate) fn derive_with_history(
        &mut self,
        sync: SyncResponse,
        history: &[Receipt],
        archived: &[Disposition],
        recovered: &[Recovered],
    ) -> Result<(), Error> {
        if sync
            .rooms
            .left
            .values()
            .any(|room| !room.timeline.events.is_empty())
        {
            return Err(Error::Unsupported);
        }
        let raw = disposition::raw_events(&self.raw)?;
        let mut returned = vec![];
        for (room, update) in sync.rooms.joined {
            if update.timeline.limited {
                return Err(Error::Unsupported);
            }
            for timeline in update.timeline.events {
                returned.push((room.to_string(), timeline));
            }
        }
        if raw.len() != returned.len() {
            return Err(Error::Conflict);
        }
        let mut events = vec![];
        let mut dispositions = vec![];
        let mut pending = vec![];
        let mut filtered = 0;
        let mut seen = BTreeSet::new();
        for ((room, original), (actual_room, timeline)) in raw.iter().zip(returned) {
            if room != &actual_room {
                return Err(Error::Conflict);
            }
            let value = wire::json(timeline.raw().json().get().as_bytes())?;
            match &timeline.kind {
                TimelineEventKind::Decrypted(_)
                    if ["event_id", "sender", "origin_server_ts"]
                        .iter()
                        .any(|field| original.get(field) != value.get(field))
                        || value.get("room_id").and_then(Value::as_str) != Some(room.as_str()) =>
                {
                    return Err(Error::Conflict);
                }
                TimelineEventKind::Decrypted(_) => {}
                _ if canonical::transport_digest(original).map_err(|_| Error::Wire)?
                    != canonical::transport_digest(&value).map_err(|_| Error::Wire)? =>
                {
                    return Err(Error::Conflict);
                }
                _ => {}
            }
            if let Some(id) = original.get("event_id").and_then(Value::as_str)
                && ruma::EventId::parse(id).is_ok()
                && !seen.insert((room.clone(), id.to_owned()))
            {
                return Err(Error::Conflict);
            }
            let source = Source::new(room, original)?;
            let decision = if let Some(prior) = Disposition::prior_values(&source, archived.iter())
                .or_else(|| Disposition::prior(&source, history))
            {
                prior
            } else if let TimelineEventKind::UnableToDecrypt { utd_info, .. } = &timeline.kind {
                // Board #10 (TS `bridge-matrix.js:6646` `onFailedRoomDecryption`):
                // a failure to decrypt because the room KEY has not arrived yet
                // is NOT terminal — retain the raw envelope so a later sync can
                // recover it, and never write the immutable tombstone that would
                // stop that. Only `is_missing_room_key()` qualifies; a permanent
                // trust refusal (untrusted/forged sender, malformed) stays an
                // immediate rejection exactly as before. One row per raw event
                // is still exact (validate_restored's raw.len() == values.len()).
                // Never silent: an undecryptable message either waits for its
                // key or is refused, and the log says which and why, so a
                // message lost in the first instant after enrollment can be
                // traced (live 2026-10-02: one owner DM was never admitted).
                let event = original
                    .get("event_id")
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                if !utd_info.reason.is_missing_room_key() {
                    eprintln!(
                        "intake: refused undecryptable {event} in {room}: {:?}",
                        utd_info.reason
                    );
                    Decision::Rejected {
                        reason: Rejection::CryptoIneligible,
                    }
                } else {
                    eprintln!("intake: waiting for the room key of {event} in {room}");
                    if pending.len() >= MAX_PENDING_UNDECRYPTABLE {
                        return Err(Error::Capacity);
                    }
                    pending.push(PendingEnvelope {
                        room: room.clone(),
                        raw: original.clone(),
                    });
                    Decision::Deferred
                }
            } else if timeline.raw().deserialize().is_err() {
                Decision::Rejected {
                    reason: Rejection::Malformed,
                }
            } else {
                match self.event(room, original, &value, &timeline.kind) {
                    Ok(Some(Candidate::Target(event))) => {
                        let index = events.len();
                        events.push(*event);
                        Decision::Candidate { index }
                    }
                    // ADR-095: a pre-project provisioning request is not a
                    // disposition row. Its admission is idempotent on
                    // `requestId`, so a re-delivered source simply re-derives
                    // and replays the prior admission.
                    Ok(Some(Candidate::PreProject(request))) => {
                        self.pre_project.push(*request);
                        continue;
                    }
                    Ok(None) => Decision::NotTarget,
                    Err(reason) => Decision::Rejected { reason },
                }
            };
            if matches!(decision, Decision::NotTarget) {
                filtered += 1;
            }
            // Board #98: every refusal at ingress leaves a trace. A follow-up
            // posted in a thread whose session is not in this plan matched no
            // target and was dropped with no reason anywhere — that silence is
            // exactly how the live drop hid for 120 s. Logged, never stored:
            // the disposition row already carries the durable decision. An
            // unrelated room event is NOT ours and stays unlogged (that is the
            // ordinary `NotTarget` path); only a threaded event — the shape
            // that can only ever be ours — is loud when it matches no target.
            match &decision {
                Decision::NotTarget if event_thread(&value).is_some() => eprintln!(
                    "[ingress] dropped {room} {}: no target for thread_root {:?} ({} target(s))",
                    value.get("event_id").and_then(Value::as_str).unwrap_or("?"),
                    event_thread(&value),
                    self.targets.len(),
                ),
                Decision::Rejected { reason } => eprintln!(
                    "[ingress] refused {room} {}: {reason:?}",
                    value.get("event_id").and_then(Value::as_str).unwrap_or("?"),
                ),
                Decision::Candidate { .. } | Decision::Deferred | Decision::NotTarget => {}
            }
            dispositions.push(Disposition::new(
                source,
                serde_json::to_value(&timeline.kind).map_err(|_| Error::Storage)?,
                decision,
            )?);
        }
        // Board #10: events recovered from the durable pending store (the room
        // key arrived on a later sync) are appended as ordinary candidates.
        // They are not in `raw`, so they hold no prior source row; their
        // indices continue after the raw candidates and the handoff admits
        // them exactly once (idempotent on the domain receipt).
        for entry in recovered {
            if let Ok(Some(Candidate::Target(event))) =
                self.event(&entry.room, &entry.original, &entry.value, &entry.kind)
            {
                let index = events.len();
                events.push(*event);
                dispositions.push(Disposition::new(
                    Source::new(&entry.room, &entry.original)?,
                    serde_json::to_value(&entry.kind).map_err(|_| Error::Storage)?,
                    Decision::Candidate { index },
                )?);
            }
        }
        // Candidate content plus the complete private disposition ledger is bounded.
        if serde_json::to_vec(&(&events, &self.pre_project, &dispositions, &pending))
            .map_err(|_| Error::Storage)?
            .len()
            // The raw sync response is already bounded by `limits.bytes`
            // (at most 1 MiB); candidates add decrypted content and the ledger.
            // Stay well below the SDK journal bound (MAX_JOURNAL_BYTES).
            > 4 * 1024 * 1024
        {
            return Err(Error::Capacity);
        }
        self.events = events;
        self.filtered = filtered;
        self.dispositions = Some(dispositions);
        self.pending = pending;
        self.phase = Phase::Derived;
        Ok(())
    }
    fn event(
        &self,
        room: &str,
        original: &Value,
        value: &Value,
        kind: &TimelineEventKind,
    ) -> Result<Option<Candidate>, Rejection> {
        use Rejection::{CryptoIneligible, Malformed, PlaintextEncrypted, Unsupported};
        let string = |field: &str| value.get(field).and_then(Value::as_str).ok_or(Malformed);
        let id = string("event_id")?;
        if value
            .get("room_id")
            .is_some_and(|v| v.as_str() != Some(room))
        {
            return Err(Malformed);
        }
        let proof = match kind {
            TimelineEventKind::UnableToDecrypt { .. } => return Err(CryptoIneligible),
            TimelineEventKind::Decrypted(d) => {
                let info = &d.encryption_info;
                // The owner's unverified devices are accepted (TS parity; see
                // `sdk::trust_requirement`). An unknown or insecurely sourced
                // device and a mismatched sender stay refused.
                if !matches!(
                    info.verification_state,
                    VerificationState::Verified
                        | VerificationState::Unverified(
                            VerificationLevel::UnverifiedIdentity
                                | VerificationLevel::VerificationViolation
                                | VerificationLevel::UnsignedDevice
                        )
                ) || info.sender.as_str() != string("sender")?
                    || info.forwarder.is_some()
                {
                    return Err(CryptoIneligible);
                }
                let device = info
                    .sender_device
                    .as_ref()
                    .ok_or(CryptoIneligible)?
                    .to_string();
                let session = match &info.algorithm_info {
                    AlgorithmInfo::MegolmV1AesSha2 {
                        session_id: Some(id),
                        ..
                    } => id.clone(),
                    _ => return Err(CryptoIneligible),
                };
                Proof::Verified {
                    sender: info.sender.to_string(),
                    device,
                    session,
                }
            }
            TimelineEventKind::PlainText { .. } => Proof::Plain,
        };
        // TS `lib/fleet-protocol.js:4`: a request may also be carried as the
        // custom event type `com.hagency.engagement.request.v1` (top-level
        // `type`), whose `content` is the camelCase fleet fields. Everything
        // else that is not a room message is not ours (Ok(None), dropped).
        let event_type = string("type")?;
        let custom_request = event_type == "com.hagency.engagement.request.v1";
        if event_type != "m.room.message" && !custom_request {
            return Ok(None);
        }
        let content = value
            .get("content")
            .and_then(Value::as_object)
            .ok_or(Malformed)?;
        let relation = content.get("m.relates_to");
        if relation.is_some_and(|v| !v.is_object()) {
            return Err(Malformed);
        }
        // A custom-type request carries the camelCase fleet fields as its
        // `content`; `provision()` reads the console-shape `body`, so the two
        // carriers converge here into the same pre-project message the msgtype
        // carrier produces (same kind, same body shape, same fail-closed
        // verification in `provision`).
        let (msgtype, body) = if custom_request {
            (
                "com.hagency.engagement.request.v1",
                custom_request_body(content)?,
            )
        } else {
            (
                content
                    .get("msgtype")
                    .and_then(Value::as_str)
                    .ok_or(Malformed)?,
                content
                    .get("body")
                    .and_then(Value::as_str)
                    .ok_or(Malformed)?
                    .to_owned(),
            )
        };
        // ADR-095: the provisioning discriminator is admitted before target
        // resolution. The reception room is pre-project, so no ReplyRoute can
        // name it; the event becomes a pre-project candidate instead and
        // `provision()` verifies it against the store-recorded registration
        // (which refuses any other room fail-closed). The provider's decision
        // rides the same lane: the approval event names the request it
        // approves, and the handoff rebuilds the verified request from the
        // admitted engagement's stored evidence before calling `approve`.
        if matches!(
            msgtype,
            "com.hagency.engagement.request.v1" | "com.hagency.engagement.approval.v1"
        ) {
            let input = Message {
                server_name: string("sender")?
                    .split_once(':')
                    .map(|(_, s)| s.to_owned())
                    .ok_or(Malformed)?,
                room_id: room.into(),
                event_id: id.into(),
                sender_mxid: string("sender")?.into(),
                thread_root: None,
                body,
                kind: msgtype.into(),
                origin_ts: value
                    .get("origin_server_ts")
                    .and_then(Value::as_u64)
                    .ok_or(Malformed)?,
            };
            input.observation().validate().map_err(|_| Malformed)?;
            return Ok(Some(Candidate::PreProject(Box::new(PreProjectEvent {
                input,
                proof,
            }))));
        }
        let thread = match relation.and_then(|v| v.get("rel_type")) {
            // Board #112 (TS `parseInboundTextMessage`, bridge-matrix.js:3294-3297):
            // "Ignore edit events: they should not create a new hagency message."
            // The retained bridge SKIPPED an edit — it never refused it. Native
            // refused every non-thread relation as `Unsupported`, so an agent's
            // own ⏳→✅ activity edits were re-delivered by every sync and logged
            // as `[ingress] refused …: Unsupported` over and over. Ignoring them
            // is the TS behaviour; a refusal TS did not have is not (RULES §1).
            Some(Value::String(t)) if t == "m.replace" => return Ok(None),
            Some(Value::String(t)) if t == "m.thread" => Some(
                relation
                    .and_then(|v| v.get("event_id"))
                    .and_then(Value::as_str)
                    .ok_or(Malformed)?
                    .to_owned(),
            ),
            Some(Value::String(_)) => return Err(Unsupported),
            Some(_) => return Err(Malformed),
            None => None,
        };
        let mut candidates = self
            .targets
            .iter()
            .filter(|t| t.room_id == room && t.thread_root == thread)
            .collect::<Vec<_>>();
        if candidates.is_empty() && thread.is_none() {
            candidates = self
                .targets
                .iter()
                .filter(|t| t.room_id == room && t.thread_root.as_deref() == Some(id))
                .collect();
        }
        // Board #112: a mentioned agent must never silently lose a message. TS
        // routes a thread follow-up to the task bound to its root, and when NO
        // binding applies it is an ordinary new message to the mentioned agent,
        // answered IN the thread (`backend-v2.js:2352-2366`). Native's ordinary
        // owner mention runs through the ROOM session (`select_agent`), which
        // carries no thread binding, so a follow-up in the thread of a question
        // that session answered at the top level matched nothing and was dropped
        // (live: `no target for thread_root Some("$uPwN…")`). Fall back to the
        // room-scoped target: the event is admitted and woken, and the answer
        // carries the SOURCE message's own thread root, exactly as TS
        // `resolveGroupReplyRelation` (`bridge-matrix.js:3318-3393`) takes it
        // from `delivery.threadRootEventId` rather than from the session.
        //
        // This arm is NOT a licence to read somebody else's thread: TS requires
        // that the message ADDRESS this agent before the binding-less follow-up
        // is taken (`backend-v2.js:2368-2377`: a human whose `msg.mentions`
        // includes the agent). The predicate below is exactly the room target's
        // own wake rule, so the fallback admits nothing the room session would
        // not already have woken on; an unaddressed thread message stays
        // `NotTarget` (filtered), as it was before.
        if candidates.is_empty() && thread.is_some() {
            let sender = string("sender")?;
            candidates = self
                .targets
                .iter()
                .filter(|t| t.room_id == room && t.thread_root.is_none())
                .filter(|t| match &t.privacy {
                    RoomPrivacy::Direct { human_mxid } => sender == human_mxid,
                    RoomPrivacy::Group {} => {
                        content
                            .get("m.mentions")
                            .and_then(|m| m.get("user_ids"))
                            .and_then(Value::as_array)
                            .is_some_and(|ids| {
                                ids.iter()
                                    .any(|id| id.as_str() == Some(t.sender_mxid.as_str()))
                            })
                            || address_mentions(content, &t.server_name).contains(&t.sender_mxid)
                    }
                })
                .collect();
        }
        if candidates.len() > 1 {
            return Err(Malformed);
        }
        let Some(target) = candidates.first() else {
            return Ok(None);
        };
        if target.encrypted && matches!(proof, Proof::Plain) {
            return Err(PlaintextEncrypted);
        }
        let kind = content
            .get("msgtype")
            .and_then(Value::as_str)
            .ok_or(Malformed)?;
        if !matches!(
            kind,
            "m.text"
                | "m.notice"
                | "m.emote"
                | "m.file"
                | "m.image"
                | "com.hagency.engagement.request.v1"
        ) {
            return Err(Unsupported);
        }
        let mut mentions = BTreeSet::new();
        if let Some(mentioned) = content.get("m.mentions") {
            let mentioned = mentioned.as_object().ok_or(Malformed)?;
            if let Some(ids) = mentioned.get("user_ids") {
                let ids = ids.as_array().ok_or(Malformed)?;
                if ids.len() > 64 {
                    return Err(Malformed);
                }
                for id in ids {
                    mentions.insert(id.as_str().ok_or(Malformed)?.to_owned());
                }
            }
        }
        // TS:bridge-matrix.js:3133-3174 — `m.mentions` is only the FIRST place a
        // mention may live. A client that sets none still addresses a member with
        // an HTML pill in `formatted_body`, or in plain text with `@name`; both
        // name a LOCALPART and the room supplies the server. ADR-054 narrowed this
        // to `m.mentions` alone, so a client that omits it could not wake an agent
        // at all; TS never narrowed it.
        if mentions.is_empty() {
            mentions = address_mentions(content, &target.server_name);
        }
        let attachment = if matches!(kind, "m.file" | "m.image") {
            match (&proof, target.encrypted) {
                (
                    Proof::Verified {
                        device, session, ..
                    },
                    true,
                ) => Some(
                    crate::attachments::Manifest::new(
                        &self.sdk_identity,
                        target,
                        original,
                        &value["content"],
                        device,
                        session,
                    )
                    .map_err(|_| Malformed)?,
                ),
                // TS parity (bridge-matrix.js:6799-6831): a plaintext room's
                // m.file/m.image is archived like any other message — the room's
                // lack of encryption is not a refusal. The TS receiver accepts
                // `content.file?.url || content.url` (lib/matrix-file.js:38);
                // the manifest carries content.url with no crypto device/session.
                (Proof::Plain, false) => Some(
                    crate::attachments::Manifest::new(
                        &self.sdk_identity,
                        target,
                        original,
                        &value["content"],
                        "",
                        "",
                    )
                    .map_err(|_| Malformed)?,
                ),
                // An encrypted event against a target recorded plaintext is a
                // state desync, not TS behaviour; keep the original refusal.
                (Proof::Verified { .. }, false) => return Err(Unsupported),
                // Unreachable in practice: a plain proof against an encrypted
                // target is already refused as PlaintextEncrypted above.
                (Proof::Plain, true) => return Err(CryptoIneligible),
            }
        } else {
            None
        };
        let event = Event {
            attachment,
            route: (*target).clone(),
            mentions,
            proof,
            input: Message {
                server_name: target.server_name.clone(),
                room_id: room.into(),
                event_id: id.into(),
                sender_mxid: string("sender")?.into(),
                thread_root: thread,
                body: content
                    .get("body")
                    .and_then(Value::as_str)
                    .ok_or(Malformed)?
                    .into(),
                kind: kind.into(),
                origin_ts: value
                    .get("origin_server_ts")
                    .and_then(Value::as_u64)
                    .ok_or(Malformed)?,
            },
        };
        event.observation().validate().map_err(|_| Malformed)?;
        Ok(Some(Candidate::Target(Box::new(event))))
    }
    pub(crate) fn rejected(&self) -> usize {
        disposition::rejected(self.dispositions.as_deref())
    }
    pub(crate) fn validate_restored(
        &self,
        identity: &str,
        user: &str,
        device: &str,
    ) -> Result<(), Error> {
        if self.sdk_identity != identity
            || self.targets.is_empty()
            || self.targets.len() > MAX_TARGETS
            || self
                .events
                .len()
                .checked_add(self.pre_project.len())
                .and_then(|n| n.checked_add(self.filtered))
                .is_none_or(|n| n > MAX_TOTAL_TIMELINE)
            || self.acknowledgements.len() > self.events.len()
            || self.raw.get("next_batch").and_then(Value::as_str) != Some(self.token.as_str())
            || canonical::transport_digest(&self.raw).map_err(|_| Error::Storage)? != self.digest
        {
            return Err(Error::Storage);
        }
        if matches!(self.phase, Phase::Prepared | Phase::Applying)
            && (!self.events.is_empty()
                || !self.pre_project.is_empty()
                || !self.pending.is_empty()
                || !self.acknowledgements.is_empty()
                || self.filtered != 0
                || self.dispositions.as_ref().is_some_and(|v| !v.is_empty()))
        {
            return Err(Error::Storage);
        }
        // Board #10: retained raw envelopes awaiting a room key are bounded and
        // must belong to a target room. They are never a candidate or a
        // terminal source, so they carry no disposition row.
        if self.pending.len() > MAX_PENDING_UNDECRYPTABLE {
            return Err(Error::Storage);
        }
        for envelope in &self.pending {
            envelope.validate()?;
            if !self.targets.iter().any(|t| t.room_id == envelope.room) {
                return Err(Error::Storage);
            }
        }
        if let Some(values) = &self.dispositions {
            disposition::validate(values, self.events.len(), self.filtered)?;
            if self.phase == Phase::Derived || !values.is_empty() {
                // A pre-project provisioning request (an agent request in the
                // reception room) is derived into `pre_project`, not a
                // disposition row, so its raw event has no row to match here.
                let raw = disposition::raw_events(&self.raw)
                    .map_err(|_| Error::Storage)?
                    .into_iter()
                    .filter(|(room, event)| {
                        !self.pre_project.iter().any(|p| {
                            p.input.room_id == *room
                                && event.get("event_id").and_then(Value::as_str)
                                    == Some(p.input.event_id.as_str())
                        })
                    })
                    .collect::<Vec<_>>();
                // Board #10: recovered candidates (from the retained pending
                // store) are appended after the raw rows, so the ledger may be
                // longer than the raw timeline. The raw rows must still match
                // exactly; every extra row must be a candidate.
                if values.len() < raw.len() {
                    return Err(Error::Storage);
                }
                for value in &values[raw.len()..] {
                    if !matches!(value.decision, Decision::Candidate { .. }) {
                        return Err(Error::Storage);
                    }
                }
                for ((room, event), value) in raw.iter().zip(values) {
                    if !value.matches(room, event).map_err(|_| Error::Storage)? {
                        return Err(Error::Storage);
                    }
                    if let Decision::Candidate { index } = value.decision {
                        let candidate = self.events.get(index).ok_or(Error::Storage)?;
                        if candidate
                            .attachment
                            .as_ref()
                            .is_some_and(|manifest| !manifest.matches_original(event))
                        {
                            return Err(Error::Storage);
                        }
                        if candidate.input.room_id != *room
                            || event.get("event_id").and_then(Value::as_str)
                                != Some(candidate.input.event_id.as_str())
                            || event.get("sender").and_then(Value::as_str)
                                != Some(candidate.input.sender_mxid.as_str())
                            || event.get("origin_server_ts").and_then(Value::as_u64)
                                != Some(candidate.input.origin_ts)
                        {
                            return Err(Error::Storage);
                        }
                        if matches!(candidate.proof, Proof::Plain) {
                            let plain = self
                                .event(
                                    room,
                                    event,
                                    event,
                                    &TimelineEventKind::PlainText {
                                        event: serde_json::from_value(event.clone())
                                            .map_err(|_| Error::Storage)?,
                                    },
                                )
                                .map_err(|_| Error::Storage)?
                                .ok_or(Error::Storage)?;
                            let Candidate::Target(plain) = plain else {
                                return Err(Error::Storage);
                            };
                            if serde_json::to_value(&plain).map_err(|_| Error::Storage)?
                                != serde_json::to_value(candidate).map_err(|_| Error::Storage)?
                            {
                                return Err(Error::Storage);
                            }
                        }
                    }
                }
            }
        }
        let mut targets = BTreeSet::new();
        for target in &self.targets {
            MatrixIngressScope::from(target)
                .validate()
                .map_err(|_| Error::Storage)?;
            if target.sender_mxid != user
                || target.device_id != device
                || !targets.insert((&target.room_id, &target.thread_root))
            {
                return Err(Error::Storage);
            }
        }
        for event in &self.events {
            event.observation().validate().map_err(|_| Error::Storage)?;
            if matches!(event.input.kind.as_str(), "m.file" | "m.image")
                != event.attachment.is_some()
            {
                return Err(Error::Storage);
            }
            if let Some(manifest) = &event.attachment {
                manifest.validate(identity, user, device)?;
                if manifest.route != event.route {
                    return Err(Error::Storage);
                }
                event.attachment_observation()?;
            }
            if !self.targets.contains(&event.route) || event.input.room_id != event.route.room_id {
                return Err(Error::Storage);
            }
            match &event.proof {
                Proof::Plain if event.route.encrypted => return Err(Error::Storage),
                Proof::Verified {
                    sender,
                    device,
                    session,
                } if sender != &event.input.sender_mxid
                    || device.is_empty()
                    || session.is_empty() =>
                {
                    return Err(Error::Storage);
                }
                _ => {}
            }
        }
        for (event, ack) in self.events.iter().zip(&self.acknowledgements) {
            if ack.sequence == 0 || ack.session_id != event.route.session_id {
                return Err(Error::Storage);
            }
        }
        Ok(())
    }
    pub(crate) fn receipt(&self) -> Result<Receipt, Error> {
        Ok(Receipt {
            token: self.token.clone(),
            digest: self.digest.clone(),
            target_digest: canonical::transport_digest(
                &serde_json::to_value(&self.targets).map_err(|_| Error::Storage)?,
            )
            .map_err(|_| Error::Storage)?,
            acknowledgements: self.acknowledgements.clone(),
            filtered: self.filtered,
            dispositions: self.dispositions.clone(),
        })
    }
}

impl Receipt {
    pub(crate) fn validate_dispositions(&self) -> Result<(), Error> {
        if let Some(values) = &self.dispositions {
            disposition::validate(values, self.acknowledgements.len(), self.filtered)?;
        }
        Ok(())
    }
    pub(crate) fn lacks_filtered_history(&self) -> bool {
        self.filtered > 0 && self.dispositions.is_none()
    }
}

/// TS:bridge-matrix.js:3150-3169, steps 2 and 3 of `parseMentions`: when a client
/// set no `m.mentions`, the address is carried by an HTML pill in
/// `formatted_body` or by plain `@name` text in the body. Both name a LOCALPART
/// and the room supplies the server, so the localpart plus the route's
/// `server_name` is exactly the MXID an `m.mentions` list would have carried.
/// TS never fell back past a non-empty pill list, so neither does this.
/// The thread root an inbound wire event names, if any. Used only by the
/// ingress trace (board #98): a threaded event that matches no target is
/// dropped, and that drop must be audible.
fn event_thread(value: &Value) -> Option<&str> {
    let relation = value.get("content")?.get("m.relates_to")?;
    (relation.get("rel_type").and_then(Value::as_str) == Some("m.thread"))
        .then(|| relation.get("event_id").and_then(Value::as_str))
        .flatten()
}
fn address_mentions(content: &serde_json::Map<String, Value>, server: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    if let Some(formatted) = content.get("formatted_body").and_then(Value::as_str) {
        for localpart in pill_localparts(formatted) {
            if let Some(mxid) = mention_mxid(&localpart, server) {
                found.insert(mxid);
            }
            if found.len() == MENTION_CAP {
                return found;
            }
        }
    }
    if !found.is_empty() {
        return found;
    }
    if let Some(body) = content.get("body").and_then(Value::as_str) {
        for localpart in plain_localparts(body) {
            if let Some(mxid) = mention_mxid(&localpart, server) {
                found.insert(mxid);
            }
            if found.len() == MENTION_CAP {
                return found;
            }
        }
    }
    found
}

/// The same 64-entry ceiling `m.mentions.user_ids` is held to.
const MENTION_CAP: usize = 64;
/// TS's `[a-z0-9_-]` character class, matched case-insensitively.
fn mention_localpart_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}
/// A mention names a localpart; the room's own server completes the MXID. An
/// entry that does not form a valid scoped user is dropped rather than failing
/// the whole event, exactly as `agentNameFromUserId` returns null for one.
fn mention_mxid(localpart: &str, server: &str) -> Option<String> {
    if localpart.is_empty() || localpart.len() > 128 {
        return None;
    }
    let mxid = format!("@{localpart}:{server}");
    hagency_core::replies::matrix_user(&mxid, server).ok()?;
    Some(mxid)
}
/// Localparts named by HTML pill hrefs, in order: TS's
/// `matrix\.to/#/@(?:prefix)?([a-z0-9_-]+):` — the trailing colon is required, so
/// a bare `matrix.to/#/@name` is not a pill.
fn pill_localparts(formatted: &str) -> Vec<String> {
    const NEEDLE: &str = "matrix.to/#/@";
    let mut out = Vec::new();
    let mut rest = formatted;
    while let Some(at) = rest.find(NEEDLE) {
        let after = &rest[at + NEEDLE.len()..];
        let end = after
            .find(|c: char| !mention_localpart_char(c))
            .unwrap_or(after.len());
        let localpart = &after[..end];
        if !localpart.is_empty() && after[end..].starts_with(':') {
            out.push(localpart.to_owned());
        }
        rest = &after[end.max(1)..];
    }
    out
}
/// Localparts named by plain `@name` text, in order: TS's global
/// `@(?:prefix)?([a-z0-9_-]+)`.
fn plain_localparts(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = body.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if c != '@' {
            continue;
        }
        let mut localpart = String::new();
        while let Some(&(_, next)) = chars.peek() {
            if mention_localpart_char(next) {
                localpart.push(next);
                chars.next();
            } else {
                break;
            }
        }
        if !localpart.is_empty() {
            out.push(localpart);
        }
    }
    out
}

#[cfg(test)]
mod mention_fallback_tests {
    use super::*;

    fn content(value: serde_json::Value) -> serde_json::Map<String, Value> {
        value.as_object().unwrap().clone()
    }
    fn mentioned(value: serde_json::Value, server: &str) -> Vec<String> {
        address_mentions(&content(value), server)
            .into_iter()
            .collect()
    }

    /// TS:bridge-matrix.js:3133-3174. `m.mentions` is empty here, so the address
    /// has to be recovered from the pill or from plain `@name` text; both name a
    /// LOCALPART and the room completes the MXID. This is the TS-visible outcome
    /// (which member an empty `m.mentions` still addresses), not an invariant.
    #[test]
    fn native_matrix_mention_fallback_reads_pills_and_plain_text() {
        let server = "example.test";
        assert_eq!(
            mentioned(
                serde_json::json!({"formatted_body":
                    "<a href=\"https://matrix.to/#/@worker:example.test\">worker</a> please"}),
                server
            ),
            vec!["@worker:example.test".to_string()],
            "an HTML pill addresses the localpart"
        );
        assert_eq!(
            mentioned(serde_json::json!({"body": "@worker please answer"}), server),
            vec!["@worker:example.test".to_string()],
            "plain @name text addresses the localpart"
        );
        assert!(
            mentioned(serde_json::json!({"body": "nobody is addressed"}), server).is_empty(),
            "an unaddressed message wakes nobody"
        );
        // A pill without the trailing server (`matrix.to/#/@worker`) is not a
        // pill; TS then reads the plain-text body, which does name the agent.
        assert_eq!(
            mentioned(
                serde_json::json!({"formatted_body":
                    "<a href=\"https://matrix.to/#/@worker\">worker</a>", "body": "@worker"}),
                server
            ),
            vec!["@worker:example.test".to_string()],
            "a non-pill href falls through to the plain-text pass"
        );
    }
}

#[cfg(test)]
mod restore_tests {
    use super::*;

    /// A Derived batch whose raw timeline carries an agent request in the
    /// reception room keeps that request in `pre_project`, with no disposition
    /// row. Restoring the journal must accept it: the live coordinator refused
    /// its own SDK store with `Storage` after every clean restart that
    /// followed a request (2026-09-30).
    #[test]
    fn native_matrix_pre_project_batch_is_restorable() {
        let room = "!reception:example.test";
        let event = serde_json::json!({
            "type": "m.room.message", "event_id": "$request", "sender": "@owner:example.test",
            "origin_server_ts": 1_000u64, "room_id": room,
            "content": {"msgtype": "com.hagency.engagement.request.v1", "body": "{}"}
        });
        let raw = serde_json::json!({
            "next_batch": "s1",
            "rooms": {"join": {room: {"timeline": {"limited": false, "events": [event]}}}}
        });
        let route = ReplyRoute {
            session_id: "coordinator".into(),
            session_generation: 1,
            engagement_id: "en_coordinator".into(),
            fleet_id: "hf_fleet".into(),
            project_id: "project".into(),
            registration_generation: 1,
            server_name: "example.test".into(),
            room_id: "!dm:example.test".into(),
            room_generation: 1,
            sender_mxid: "@coordinator:example.test".into(),
            device_id: "DEVICE".into(),
            transport_generation: 1,
            owner_mxid: "@owner:example.test".into(),
            privacy: RoomPrivacy::Direct {
                human_mxid: "@owner:example.test".into(),
            },
            encrypted: true,
            thread_root: None,
        };
        let mut batch = Batch::new(raw, vec![route], "identity".into()).unwrap();
        batch.phase = Phase::Derived;
        batch.dispositions = Some(vec![]);
        batch.pre_project.push(PreProjectEvent {
            input: Message {
                server_name: "example.test".into(),
                room_id: room.into(),
                event_id: "$request".into(),
                sender_mxid: "@owner:example.test".into(),
                thread_root: None,
                body: "{}".into(),
                kind: "com.hagency.engagement.request.v1".into(),
                origin_ts: 1_000,
            },
            proof: Proof::Plain,
        });
        assert!(
            batch
                .validate_restored("identity", "@coordinator:example.test", "DEVICE")
                .is_ok(),
            "a request recorded as pre_project has no disposition row to match"
        );
        // An ordinary raw event with no row is still refused.
        batch.pre_project.clear();
        assert!(matches!(
            batch.validate_restored("identity", "@coordinator:example.test", "DEVICE"),
            Err(Error::Storage)
        ));
    }
}
