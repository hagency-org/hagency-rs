//! Claude Code permission requests as owner approvals (ADR-192 decision 4).
//! The Codex coordinator's store protocol (`control.rs`), step for step, on
//! the Claude control channel (ADR-156): one durable request per
//! `can_use_tool`, the owner's verdict frozen into exactly one prepared frame,
//! authorized, begun and rechecked before its first byte, and its acceptance
//! recorded. Task and always grants are matched by the store on the tool and
//! its exact input; Claude never receives a permission update.
use super::{
    ApprovalNotice,
    state::{ApprovalRun, Callbacks, wall_now},
};
use crate::{AuthoritySite, Failure, SettlementCause, operation::Deadline, usage::UsageRun};
use hagency_core::{
    approvals::{
        ApprovalChoice, ApprovalRpcId, ApprovalSummary, HostApprovalContext, HostApprovalRequest,
    },
    execution::CLAUDE_TOOL_METHOD,
    tasks::{RunnerCapability, TaskState},
};
use hagency_runtime::{
    claude::{
        EventKind, Message,
        session::{
            ControlUpdate, Error, PermissionDecision, PreparedApproval, PreparedUpdate,
            WriteProgress,
        },
    },
    owned::OwnedClaudeSession,
};
use hagency_store::{ApprovalResponseGrant, ApprovalResponseObservation, DomainStore};
use serde_json::{Value, json};
use std::{future::Future, sync::atomic::AtomicBool, time::Duration};
use tokio::time::Instant;

/// The dispatch's one prompt (ADR-155): the `turnId` of every request of a
/// session, as Codex names its turn.
pub(crate) const TURN: &str = "prompt";

/// One `can_use_tool` request, from its arrival to its recorded answer.
pub(crate) struct Entry {
    item: String,
    input: HostApprovalRequest,
    id: Option<String>,
    owner_deadline: Instant,
    response_deadline: Instant,
    owner_expires_at: u64,
    selected: Option<bool>,
    prepared: Option<PreparedApproval>,
    grant: Option<ApprovalResponseGrant>,
    write: Option<WriteProgress>,
    admitted: bool,
    /// The frame is handed to the transport and its write not yet accepted.
    pub(super) in_flight: bool,
    expired: bool,
    recorded: bool,
    /// Claude withdrew the request (`control_cancel_request`).
    resolved: bool,
    /// The tool use this request is for, whose result ends it.
    tool_use: Option<String>,
    /// That result was seen (`applied` recorded).
    applied: bool,
    /// The store's application of this request's answer, once authorized.
    application: Option<hagency_core::approvals::ApprovalApplication>,
}
impl Entry {
    pub(super) fn written(&self) -> bool {
        self.write.is_some()
    }
    /// ADR-046, as for a Codex resolution: before admission a withdrawal
    /// cancels the callback; once admitted or in flight the send path owns
    /// the frame and skips it.
    fn resolution_arrives(&mut self) -> Result<bool, Failure> {
        self.resolved = true;
        if self.write.is_none() && !self.admitted && !self.in_flight {
            Err(Failure::ApprovalCancelled)
        } else {
            Ok(false)
        }
    }
}
struct Sending {
    id: String,
    prepared: PreparedApproval,
    grant: ApprovalResponseGrant,
}

/// What the turn's own events decided (ADR-192 decision 5): the reply text of
/// a successful result, or the subtype of a failed one.
#[derive(Default)]
pub(crate) struct ClaudeOutcome {
    pub ended: bool,
    pub text: Option<String>,
    pub failure: String,
}
impl ClaudeOutcome {
    /// Usage first (ADR-157), then the result. True at the turn's result.
    pub(crate) async fn observe(
        &mut self,
        session: &OwnedClaudeSession,
        usage: &mut UsageRun,
        message: &Message,
        cancel: &AtomicBool,
        until: Instant,
    ) -> Result<bool, Failure> {
        if let Some(observation) = session.last_observation()
            && usage.observe(observation)
        {
            // Storage refusal closes capture only, as for Codex.
            let _ = crate::operation::bounded(usage.record_pending(), cancel, until).await?;
        }
        let Message::Event { kind, payload, .. } = message else {
            return Ok(false);
        };
        if *kind != EventKind::Result {
            return Ok(false);
        }
        self.ended = true;
        if payload["subtype"] == "success" && payload["is_error"] == false {
            self.text = payload["result"].as_str().map(str::to_owned);
        } else {
            self.failure = payload["subtype"]
                .as_str()
                .unwrap_or("error")
                .chars()
                .take(512)
                .collect();
        }
        Ok(true)
    }
}

pub(crate) struct ClaudeDrive<'a> {
    pub domain: &'a DomainStore,
    pub cap: &'a RunnerCapability,
    pub cancel: &'a AtomicBool,
    pub until: Instant,
    pub status: &'a mut Option<TaskState>,
    pub usage: &'a mut UsageRun,
    pub outcome: &'a mut ClaudeOutcome,
    /// Diagnostic only, as on the Codex drive.
    pub settlement_cause: &'a mut Option<SettlementCause>,
}
struct Pumped<T> {
    output: T,
    terminal: Result<bool, Failure>,
}

/// A Claude ID as a store identifier: itself when it already is one, else a
/// digest of it. Claude's session and request IDs are opaque strings.
pub(crate) fn opaque(value: &str) -> String {
    if hagency_core::project::identifier(value, 256).is_ok() {
        return value.to_owned();
    }
    let digest = hagency_core::canonical::digest(&json!(value)).unwrap_or_default();
    format!("claude_{}", &digest[..digest.len().min(40)])
}

/// The send-path classifier of `control.rs`, on the Claude stream: a peer-gone
/// cause with no byte accepted is `PeerUnavailable`; any accepted byte, or a
/// host-side close, leaves the frame's fate unknown; anything else is a
/// protocol refusal.
fn send_failure(session: &OwnedClaudeSession, armed: bool, error: Error) -> Failure {
    let error = match (error, session.termination()) {
        (Error::State, Some(termination)) => termination.cause,
        (error, _) => error,
    };
    let zero_accepted = session
        .termination()
        .and_then(|termination| termination.unconfirmed_write.as_ref())
        .is_some_and(|write| write.accepted_bytes == 0);
    match error {
        Error::Io(_) | Error::PeerEof if zero_accepted || !armed => Failure::PeerUnavailable,
        Error::Io(_) | Error::PeerEof | Error::HostClosed | Error::Closed => {
            Failure::SettlementUnknown
        }
        _ => Failure::Protocol,
    }
}

fn matches(
    grant: &ApprovalResponseGrant,
    key: &str,
    entry: &Entry,
    context: &HostApprovalContext,
) -> Result<(), Failure> {
    let application = grant.application();
    if entry.id.as_deref() != Some(application.id.as_str())
        || application.connection_id != context.connection_id
        || application.upstream_id != ApprovalRpcId::String(key.to_owned())
        || application.thread_id != context.thread_id
        || application.turn_id != context.turn_id
        || application.item_id != entry.item
        || entry.selected != Some(application.allow)
    {
        return Err(Failure::LostAuthority {
            site: AuthoritySite::ApprovalApplication,
            cause: crate::AuthorityCause::State,
        });
    }
    Ok(())
}

impl Callbacks {
    fn retain_claude(
        &mut self,
        session: &OwnedClaudeSession,
        request_id: &str,
        tool_name: &str,
        tool_use_id: Option<&str>,
        input: &Value,
        until: Instant,
    ) -> Result<String, Failure> {
        if self.claude.len() >= 16 || self.claude.contains_key(request_id) {
            return Err(Failure::ApprovalCapacity);
        }
        let owner_deadline = session
            .approval_deadline(request_id)
            .map_err(|_| Failure::Protocol)?;
        let response_deadline =
            owner_deadline + Duration::from_millis(self.host.claude_policy().response_reserve_ms);
        if response_deadline > until || owner_deadline <= Instant::now() {
            return Err(Failure::Deadline);
        }
        let sampled = Instant::now();
        let wall = wall_now()?;
        let millis = |deadline: Instant| {
            u64::try_from(deadline.saturating_duration_since(sampled).as_millis())
                .ok()
                .and_then(|ms| wall.checked_add(ms))
                .ok_or(Failure::Deadline)
        };
        let owner_expires_at = millis(owner_deadline)?;
        let expires_at = millis(response_deadline)?;
        let context = self.context.as_ref().ok_or(Failure::Admission)?;
        let item = opaque(request_id);
        let mut params = json!({"threadId":context.thread_id,"turnId":context.turn_id,
            "itemId":item,"toolName":tool_name,"input":input});
        if let Some(id) = tool_use_id {
            params["toolUseId"] = json!(id);
        }
        let request = HostApprovalRequest {
            context_id: context.id.clone(),
            upstream_id: ApprovalRpcId::String(request_id.to_owned()),
            item_id: item.clone(),
            method: CLAUDE_TOOL_METHOD.into(),
            params,
            expires_at,
        };
        if self.parked.is_none() {
            self.parked = Some(self.host.reserve_parked()?);
        }
        self.claude.insert(
            request_id.to_owned(),
            Entry {
                item,
                input: request,
                id: None,
                owner_deadline,
                response_deadline,
                owner_expires_at,
                selected: None,
                prepared: None,
                grant: None,
                write: None,
                admitted: false,
                in_flight: false,
                expired: false,
                recorded: false,
                resolved: false,
                tool_use: tool_use_id.map(str::to_owned),
                applied: false,
                application: None,
            },
        );
        // The reservation and callback are retained before the request.
        self.parked
            .as_mut()
            .ok_or(Failure::ApprovalCapacity)?
            .possible();
        Ok(request_id.to_owned())
    }
    fn claude_acknowledged(&mut self, key: &str, summary: ApprovalSummary) -> Result<(), Failure> {
        let entry = self.claude.get_mut(key).ok_or(Failure::Protocol)?;
        entry.id = Some(summary.id.clone());
        // A matching task or always grant decided it already: no card.
        if summary.choice.is_some() {
            return Ok(());
        }
        self.notices
            .as_ref()
            .ok_or(Failure::ApprovalCapacity)?
            .try_send(ApprovalNotice {
                request_id: summary.id,
                owner_expires_at: entry.owner_expires_at,
            })
            .map_err(|_| Failure::ApprovalCapacity)
    }
    fn release_claude_written(&mut self) {
        if !self.claude.is_empty() && self.claude.values().all(|e| e.recorded) {
            if let Some(parked) = &mut self.parked {
                parked.release();
            }
            self.parked = None;
        }
    }
    /// The turn's result, against the requests still open (the Codex
    /// turn-end rule): an unanswered request is a cancellation; a frame whose
    /// bytes left without a recorded acceptance has an unknown fate; a frame
    /// that never left, or an answered request, ends the drive quietly.
    fn claude_turn_end(&self, session: &OwnedClaudeSession) -> Result<bool, Failure> {
        if self
            .claude
            .values()
            .any(|e| e.write.is_none() && !e.in_flight)
        {
            return Err(Failure::ApprovalCancelled);
        }
        if self
            .claude
            .values()
            .any(|e| e.in_flight && e.write.is_none() && !e.resolved)
            && session
                .write_progress()
                .is_some_and(|write| write.accepted_bytes > 0)
        {
            return Err(Failure::SettlementUnknown);
        }
        Ok(true)
    }
}

impl ClaudeDrive<'_> {
    async fn message(
        &mut self,
        callbacks: &mut Callbacks,
        session: &mut OwnedClaudeSession,
        message: Message,
    ) -> Result<bool, Failure> {
        // Retain the callback or its withdrawal before any await.
        let (request, mut terminal) = match &message {
            Message::Permission {
                request_id,
                tool_name,
                tool_use_id,
                input,
            } => (
                Some(callbacks.retain_claude(
                    session,
                    request_id,
                    tool_name,
                    tool_use_id.as_deref(),
                    input,
                    self.until,
                )?),
                Ok(false),
            ),
            Message::ControlCancel { request_id } => (
                None,
                callbacks
                    .claude
                    .get_mut(request_id)
                    .ok_or(Failure::Protocol)?
                    .resolution_arrives(),
            ),
            Message::ControlResponse { .. } => return Err(Failure::Protocol),
            Message::Event { .. } => {
                let activity = crate::operation::claude_activity(&message);
                // A tool result for an answered request is its decision
                // taking effect.
                for event in &activity {
                    let hagency_store::ActivityEvent::ToolEnd { event_id, .. } = event else {
                        continue;
                    };
                    for entry in callbacks.claude.values_mut() {
                        if let (false, Some(application), Some(tool_use)) =
                            (entry.applied, &entry.application, &entry.tool_use)
                            && (entry.write.is_some() || entry.in_flight)
                            && crate::operation::activity_id(tool_use) == *event_id
                        {
                            entry.applied = true;
                            super::observe_applied(self.domain, application, "claude tool result")
                                .await;
                        }
                    }
                }
                crate::operation::record_activity(self.domain, self.cap, activity).await;
                (None, Ok(false))
            }
        };
        if self
            .outcome
            .observe(session, self.usage, &message, self.cancel, self.until)
            .await?
        {
            terminal = callbacks.claude_turn_end(session);
        }
        if let Some(key) = request {
            let input = callbacks
                .claude
                .get(&key)
                .ok_or(Failure::Protocol)?
                .input
                .clone();
            let summary = self
                .domain
                .request_owner_approval(self.cap.clone(), input)
                .await
                .map_err(|error| Failure::lost(AuthoritySite::ApprovalRequest, &error))?;
            callbacks.claude_acknowledged(&key, summary)?;
        }
        terminal
    }
    /// The Codex pump on the Claude stream: the store future and the session
    /// read run together; a message is handled in order and the SAME store
    /// future is kept. A terminal message stops the child, and the store call
    /// still finishes, since stopping cannot cancel a commit.
    async fn pump<F: Future>(
        &mut self,
        callbacks: &mut Callbacks,
        session: &mut OwnedClaudeSession,
        future: F,
    ) -> Pumped<F::Output> {
        tokio::pin!(future);
        loop {
            let step = crate::operation::bounded(
                session.next_or_control(future.as_mut()),
                self.cancel,
                self.until,
            )
            .await;
            let terminal = match step {
                Ok(Ok(ControlUpdate::Control(output))) => {
                    return Pumped {
                        output,
                        terminal: Ok(false),
                    };
                }
                Ok(Ok(ControlUpdate::Message(message))) => {
                    self.message(callbacks, session, message).await
                }
                Ok(Err(error)) => {
                    let armed = callbacks
                        .claude
                        .values()
                        .any(|e| e.prepared.is_some() || e.in_flight || e.write.is_some());
                    Err(send_failure(session, armed, error))
                }
                Err(failure) => Err(failure),
            };
            if !matches!(terminal, Ok(false)) {
                session.stop();
                return Pumped {
                    output: future.await,
                    terminal,
                };
            }
        }
    }
}

impl ApprovalRun {
    /// Bind the approval context once `system/init` named the session, then
    /// open the session's control: requests reach the owner from here on, and
    /// an unanswered one is denied at its owner bound.
    pub(crate) async fn bind_claude(
        &mut self,
        domain: &DomainStore,
        cap: &RunnerCapability,
        expected: &str,
        context: HostApprovalContext,
        deadline: Deadline,
        session: &mut OwnedClaudeSession,
    ) -> Result<(), Failure> {
        let Deadline { until, expires_at } = deadline;
        self.callbacks.context = Some(context.clone());
        self.scope = Some(
            domain
                .bind_owned_approval_context(
                    cap.clone(),
                    expected.into(),
                    context,
                    until.into_std(),
                    expires_at,
                )
                .await
                .map_err(|error| Failure::lost(AuthoritySite::ApprovalBind, &error))?,
        );
        session
            .enable_approval_control(self.callbacks.host.claude_policy())
            .map_err(|_| Failure::Admission)?;
        session
            .enable_owner_wait_expiry()
            .map_err(|_| Failure::Admission)
    }

    /// The rest of the turn, from `system/init` to Claude's result: every
    /// message, every owner decision and every answer.
    pub(crate) async fn drive_claude(
        &mut self,
        mut drive: ClaudeDrive<'_>,
        session: &mut OwnedClaudeSession,
    ) -> Result<(), Failure> {
        let (domain, cap, cancel, until) = (drive.domain, drive.cap, drive.cancel, drive.until);
        let mut sending: Option<Sending> = None;
        loop {
            // The owner-wait expiry (the ADR046 amendment): the host takes an
            // unanswered request over and records the deny before any byte;
            // the loop below then answers it from that persisted choice.
            for key in self.callbacks.claude.keys().cloned().collect::<Vec<_>>() {
                let entry = self
                    .callbacks
                    .claude
                    .get_mut(&key)
                    .ok_or(Failure::Protocol)?;
                if entry.expired
                    || entry.resolved
                    || entry.selected.is_some()
                    || entry.prepared.is_some()
                    || entry.write.is_some()
                    || entry.admitted
                    || entry.in_flight
                    || Instant::now() < entry.owner_deadline
                {
                    continue;
                }
                let Some(id) = entry.id.clone() else {
                    continue;
                };
                let owner_expires_at = entry.owner_expires_at;
                session
                    .expire_approval(&key)
                    .map_err(|_| Failure::Deadline)?;
                entry.expired = true;
                let denied = drive
                    .pump(
                        &mut self.callbacks,
                        session,
                        domain.deny_for_owner_wait_expiry(id, owner_expires_at),
                    )
                    .await;
                // `State`: the owner decided first; read below like any other.
                match denied.output {
                    Ok(_) | Err(hagency_store::Error::State) => {}
                    Err(error) => {
                        return Err(Failure::lost(AuthoritySite::ApprovalExpiry, &error));
                    }
                }
                if denied.terminal? {
                    return Ok(());
                }
            }
            let scope = self.scope.as_ref().ok_or(Failure::Admission)?;
            let observed = drive
                .pump(
                    &mut self.callbacks,
                    session,
                    domain.maintain_owned_approval(scope),
                )
                .await;
            let current = observed
                .output
                .map_err(|error| Failure::lost(AuthoritySite::ApprovalMaintain, &error))?;
            *drive.status = Some(current.task.status);
            if observed.terminal? {
                return Ok(());
            }
            // Freeze a frame only from a persisted choice, before the owner
            // bound; past it only the host's own deny on an expired request.
            let keys: Vec<_> = self.callbacks.claude.keys().cloned().collect();
            for key in &keys {
                let entry = self
                    .callbacks
                    .claude
                    .get_mut(key)
                    .ok_or(Failure::Protocol)?;
                if entry.resolved
                    || entry.write.is_some()
                    || entry.prepared.is_some()
                    || entry.admitted
                {
                    continue;
                }
                let Some(choice) = current
                    .approvals
                    .iter()
                    .find(|v| Some(v.id.as_str()) == entry.id.as_deref())
                    .and_then(|summary| summary.choice)
                else {
                    continue;
                };
                let allow = choice != ApprovalChoice::Deny;
                if (allow || !entry.expired) && Instant::now() >= entry.owner_deadline {
                    return Err(Failure::Deadline);
                }
                let decision = if allow {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Deny
                };
                entry.prepared = Some(
                    session
                        .prepare_approval(key, decision)
                        .map_err(|_| Failure::Protocol)?,
                );
                entry.selected = Some(allow);
                let id = entry.id.clone().ok_or(Failure::Protocol)?;
                let authorized = drive
                    .pump(
                        &mut self.callbacks,
                        session,
                        domain.authorize_approval_response(cap.clone(), id),
                    )
                    .await;
                let grant = authorized
                    .output
                    .map_err(|error| Failure::lost(AuthoritySite::ApprovalResponse, &error))?;
                let context = self.callbacks.context.as_ref().ok_or(Failure::Admission)?;
                let entry = self.callbacks.claude.get(key).ok_or(Failure::Protocol)?;
                matches(&grant, key, entry, context)?;
                let entry = self
                    .callbacks
                    .claude
                    .get_mut(key)
                    .ok_or(Failure::Protocol)?;
                // Kept for the application observation: the grant itself
                // travels with the frame while it is in flight.
                entry.application = Some(grant.application().clone());
                entry.grant = Some(grant);
                if authorized.terminal? {
                    return Ok(());
                }
            }
            // Every open request answered: begin them together (the store's
            // barrier), so no answer leaves while another waits for its owner.
            let ready = !self.callbacks.claude.is_empty()
                && self.callbacks.claude.values().all(|e| {
                    e.write.is_some() || e.admitted || (e.prepared.is_some() && e.grant.is_some())
                });
            if ready {
                let deadline = self
                    .callbacks
                    .claude
                    .values()
                    .filter(|e| e.write.is_none())
                    .map(|e| e.response_deadline)
                    .min()
                    .unwrap_or(until)
                    .min(until);
                let mut ids = Vec::new();
                let mut grants = Vec::new();
                for (key, entry) in &mut self.callbacks.claude {
                    if entry.write.is_none() && !entry.admitted {
                        ids.push(key.clone());
                        grants.push(entry.grant.take().ok_or(Failure::Protocol)?);
                    }
                }
                if !grants.is_empty() {
                    let begun = drive
                        .pump(
                            &mut self.callbacks,
                            session,
                            domain.begin_approval_responses(
                                cap.clone(),
                                &mut grants,
                                deadline.into_std(),
                            ),
                        )
                        .await;
                    begun
                        .output
                        .map_err(|error| Failure::lost(AuthoritySite::ApprovalBegin, &error))?;
                    for (key, grant) in ids.into_iter().zip(grants) {
                        let entry = self
                            .callbacks
                            .claude
                            .get_mut(&key)
                            .ok_or(Failure::Protocol)?;
                        entry.admitted = true;
                        entry.grant = Some(grant);
                    }
                    if begun.terminal? {
                        return Ok(());
                    }
                }
            }
            // A new request repairs the barrier first; an older admitted frame
            // stays retained meanwhile.
            if self
                .callbacks
                .claude
                .values()
                .any(|e| e.write.is_none() && !e.admitted)
            {
                let wake = drive
                    .pump(
                        &mut self.callbacks,
                        session,
                        tokio::time::sleep(Duration::from_millis(100)),
                    )
                    .await;
                if wake.terminal? {
                    return Ok(());
                }
                continue;
            }
            if sending.is_none()
                && let Some((key, entry)) = self
                    .callbacks
                    .claude
                    .iter_mut()
                    .find(|(_, e)| e.admitted && e.write.is_none() && !e.in_flight)
            {
                // Committed to the transport from here: set before the recheck
                // pump, which may deliver this request's own withdrawal.
                entry.in_flight = true;
                sending = Some(Sending {
                    id: key.clone(),
                    prepared: entry.prepared.take().ok_or(Failure::Protocol)?,
                    grant: entry.grant.take().ok_or(Failure::Protocol)?,
                });
            }
            let Some(mut frame) = sending.take() else {
                let wake = drive
                    .pump(
                        &mut self.callbacks,
                        session,
                        tokio::time::sleep(Duration::from_millis(100)),
                    )
                    .await;
                if wake.terminal? {
                    return Ok(());
                }
                continue;
            };
            let checked = drive
                .pump(
                    &mut self.callbacks,
                    session,
                    domain.check_approval_response(cap.clone(), &frame.grant),
                )
                .await;
            let new_barrier = self
                .callbacks
                .claude
                .values()
                .any(|e| e.write.is_none() && !e.admitted);
            if new_barrier
                && matches!(
                    checked.output,
                    Ok(()) | Err(hagency_store::Error::RunnerAuthority)
                )
            {
                checked.terminal?;
                sending = Some(frame);
                continue;
            }
            checked
                .output
                .map_err(|error| Failure::lost(AuthoritySite::ApprovalCheck, &error))?;
            if checked.terminal? {
                return Ok(());
            }
            // Claude withdrew this request before its first byte: the frame
            // is dropped unsent and the entry is never selected again.
            if !session.prepared_admissible(&frame.prepared) {
                continue;
            }
            let step = crate::operation::bounded(
                session.send_prepared_approval(&mut frame.prepared),
                cancel,
                until,
            )
            .await?
            .map_err(|error| send_failure(session, true, error))?;
            match step {
                PreparedUpdate::Message(message) => {
                    // The frame is suspended mid-write: handle the message,
                    // recheck, then continue this same frame.
                    let ended = drive.message(&mut self.callbacks, session, message).await?;
                    if ended {
                        return Err(Failure::ApprovalCancelled);
                    }
                    sending = Some(frame);
                }
                PreparedUpdate::WriteAccepted(write) => {
                    let entry = self
                        .callbacks
                        .claude
                        .get_mut(&frame.id)
                        .ok_or(Failure::Protocol)?;
                    entry.write = Some(write);
                    entry.in_flight = false;
                    let written = drive
                        .pump(
                            &mut self.callbacks,
                            session,
                            domain.observe_approval_response(
                                &mut frame.grant,
                                ApprovalResponseObservation::WriteAccepted,
                            ),
                        )
                        .await;
                    // The frame is accepted; only its record may have failed.
                    // One ordered read decides (ADR-053); nothing is re-sent.
                    if written.output.is_err() {
                        let id = frame.grant.application().id.clone();
                        match crate::operation::bounded(
                            domain.approval_response_summary(id),
                            cancel,
                            until,
                        )
                        .await?
                        {
                            Ok(summary) if summary.write_accepted => {}
                            Ok(_) => {
                                *drive.settlement_cause =
                                    Some(SettlementCause::AcceptanceUnrecorded);
                                return Err(Failure::SettlementUnknown);
                            }
                            Err(read_error) => {
                                *drive.settlement_cause = Some(SettlementCause::of(&read_error));
                                return Err(Failure::SettlementUnknown);
                            }
                        }
                    }
                    let entry = self
                        .callbacks
                        .claude
                        .get_mut(&frame.id)
                        .ok_or(Failure::Protocol)?;
                    entry.prepared = Some(frame.prepared);
                    entry.grant = Some(frame.grant);
                    entry.recorded = true;
                    self.callbacks.release_claude_written();
                    if written.terminal? {
                        return Ok(());
                    }
                }
            }
        }
    }
}
