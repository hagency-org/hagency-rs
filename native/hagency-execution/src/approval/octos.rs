//! Octos approvals as owner approvals (ADR-193 decision 4). The Claude
//! coordinator's store protocol (`claude.rs`), step for step, on the Octos
//! session: one durable request per `approval/requested`, the owner's verdict
//! frozen into exactly one prepared `approval/respond` with scope `request`,
//! authorized, begun and rechecked before its first byte, and its acceptance
//! recorded. Task and always grants are matched by the store on a shell
//! command's exact line and working directory; Octos never records a rule of
//! its own.
use super::{
    ApprovalNotice,
    state::{ApprovalRun, Callbacks, wall_now},
};
use crate::{AuthoritySite, Failure, SettlementCause, operation::Deadline, usage::UsageRun};
use hagency_core::{
    approvals::{
        ApprovalChoice, ApprovalRpcId, ApprovalSummary, HostApprovalContext, HostApprovalRequest,
    },
    execution::OCTOS_APPROVAL_METHOD,
    tasks::{RunnerCapability, TaskState},
};
use hagency_runtime::{
    octos::{
        Outcome,
        session::{
            ControlUpdate, Error, Event, PermissionDecision, PreparedApproval, PreparedUpdate,
            WriteProgress,
        },
    },
    owned::OwnedOctosSession,
};
use hagency_store::{ApprovalResponseGrant, ApprovalResponseObservation, DomainStore};
use serde_json::{Value, json};
use std::{future::Future, sync::atomic::AtomicBool, time::Duration};
use tokio::time::Instant;

/// One `approval/requested`, from its arrival to its recorded answer.
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
    /// Octos settled the approval itself: cancelled, timed out or decided
    /// elsewhere.
    resolved: bool,
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

/// What the session's own events decided (ADR-193): the last turn's reply
/// when Octos reports the session idle and completed, or its outcome and
/// error code otherwise.
#[derive(Default)]
pub(crate) struct OctosOutcome {
    pub ended: bool,
    pub text: Option<String>,
    pub failure: String,
}
impl OctosOutcome {
    /// Usage first: each terminal's, then the session's at idle (ADR-157).
    /// True once Octos reports the session idle.
    pub(crate) async fn observe(
        &mut self,
        session: &OwnedOctosSession,
        usage: &mut UsageRun,
        event: &Event,
        cancel: &AtomicBool,
        until: Instant,
    ) -> Result<bool, Failure> {
        if let Some(observation) = session.last_observation()
            && usage.observe(observation)
        {
            // Storage refusal closes capture only, as for Codex.
            let _ = crate::operation::bounded(usage.record_pending(), cancel, until).await?;
        }
        let Event::Idle(idle) = event else {
            return Ok(false);
        };
        self.ended = true;
        match (idle.outcome, &idle.reply) {
            (Outcome::Completed, Some(reply)) => self.text = Some(reply.clone()),
            (outcome, _) => {
                let code = idle.error_code.as_deref().unwrap_or_default();
                self.failure = format!("{outcome:?}: {code}").chars().take(512).collect();
            }
        }
        Ok(true)
    }
}

pub(crate) struct OctosDrive<'a> {
    pub domain: &'a DomainStore,
    pub cap: &'a RunnerCapability,
    /// The dispatch's lease fingerprint, renewed while a host tool runs.
    pub expected: &'a str,
    /// The dispatch's task helper, behind its host tools (decision 5).
    pub tools: &'a mut crate::octos_tools::HostTools,
    pub cancel: &'a AtomicBool,
    pub until: Instant,
    pub status: &'a mut Option<TaskState>,
    pub usage: &'a mut UsageRun,
    pub outcome: &'a mut OctosOutcome,
    /// Diagnostic only, as on the Codex drive.
    pub settlement_cause: &'a mut Option<SettlementCause>,
}
struct Pumped<T> {
    output: T,
    terminal: Result<bool, Failure>,
}

/// An Octos ID as a store identifier: itself when it already is one, else a
/// digest of it. Octos's session and approval IDs are opaque strings.
pub(crate) fn opaque(value: &str) -> String {
    if hagency_core::project::identifier(value, 256).is_ok() {
        return value.to_owned();
    }
    let digest = hagency_core::canonical::digest(&json!(value)).unwrap_or_default();
    format!("octos_{}", &digest[..digest.len().min(40)])
}

/// The send-path classifier of `control.rs`, on the Octos session: a peer-gone
/// cause with no byte accepted is `PeerUnavailable`; any accepted byte, or a
/// host-side close, leaves the frame's fate unknown; anything else is a
/// protocol refusal.
fn send_failure(session: &OwnedOctosSession, armed: bool, error: Error) -> Failure {
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

/// The owner request for one `approval/requested`: the tool, its title and
/// body as Octos shows them, the Octos turn it arrived in, and for a shell
/// command its exact line and working directory, the scope a task or always
/// grant matches.
fn request_params(
    context: &HostApprovalContext,
    item: &str,
    turn_id: &str,
    params: &Value,
) -> Result<Value, Failure> {
    let text = |key: &str| params[key].as_str().ok_or(Failure::Protocol);
    let mut request = json!({"threadId":context.thread_id,"turnId":context.turn_id,
        "itemId":item,"toolName":text("tool_name")?,"octosTurnId":turn_id,
        "title":text("title")?,"body":text("body")?});
    let details = &params["typed_details"];
    if details["kind"] == "command" {
        for (from, to) in [("command_line", "command"), ("cwd", "cwd")] {
            if let Some(value) = details["command"][from].as_str() {
                request[to] = json!(value);
            }
        }
    }
    Ok(request)
}

impl Callbacks {
    fn retain_octos(
        &mut self,
        session: &OwnedOctosSession,
        approval_id: &str,
        turn_id: &str,
        params: &Value,
        until: Instant,
    ) -> Result<String, Failure> {
        if self.octos.len() >= 16 || self.octos.contains_key(approval_id) {
            return Err(Failure::ApprovalCapacity);
        }
        let owner_deadline = session
            .approval_deadline(approval_id)
            .map_err(|_| Failure::Protocol)?;
        let response_deadline =
            owner_deadline + Duration::from_millis(self.host.octos_policy().response_reserve_ms);
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
        let item = opaque(approval_id);
        let request = HostApprovalRequest {
            context_id: context.id.clone(),
            upstream_id: ApprovalRpcId::String(approval_id.to_owned()),
            item_id: item.clone(),
            method: OCTOS_APPROVAL_METHOD.into(),
            params: request_params(context, &item, turn_id, params)?,
            expires_at,
        };
        if self.parked.is_none() {
            self.parked = Some(self.host.reserve_parked()?);
        }
        self.octos.insert(
            approval_id.to_owned(),
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
            },
        );
        // The reservation and callback are retained before the request.
        self.parked
            .as_mut()
            .ok_or(Failure::ApprovalCapacity)?
            .possible();
        Ok(approval_id.to_owned())
    }
    fn octos_acknowledged(&mut self, key: &str, summary: ApprovalSummary) -> Result<(), Failure> {
        let entry = self.octos.get_mut(key).ok_or(Failure::Protocol)?;
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
    fn release_octos_written(&mut self) {
        if !self.octos.is_empty() && self.octos.values().all(|e| e.recorded) {
            if let Some(parked) = &mut self.parked {
                parked.release();
            }
            self.parked = None;
        }
    }
    /// Octos reported the session idle, against the approvals still open
    /// (the Codex turn-end rule): an unanswered approval is a cancellation; a
    /// frame whose bytes left without a recorded acceptance has an unknown
    /// fate; a frame that never left, or an answered approval, ends the drive
    /// quietly.
    fn octos_session_end(&self, session: &OwnedOctosSession) -> Result<bool, Failure> {
        if self
            .octos
            .values()
            .any(|e| e.write.is_none() && !e.in_flight)
        {
            return Err(Failure::ApprovalCancelled);
        }
        if self
            .octos
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

impl OctosDrive<'_> {
    async fn event(
        &mut self,
        callbacks: &mut Callbacks,
        session: &mut OwnedOctosSession,
        event: Event,
    ) -> Result<bool, Failure> {
        // Retain the callback or its withdrawal before any await.
        let (request, mut terminal) = match &event {
            Event::Approval {
                approval_id,
                turn_id,
                params,
            } => (
                Some(callbacks.retain_octos(session, approval_id, turn_id, params, self.until)?),
                Ok(false),
            ),
            Event::ApprovalSettled { approval_id } => (
                None,
                callbacks
                    .octos
                    .get_mut(approval_id)
                    .ok_or(Failure::Protocol)?
                    .resolution_arrives(),
            ),
            Event::ToolCall { params } => {
                crate::operation::answer_octos_tool(
                    self.tools,
                    session,
                    params,
                    self.domain,
                    self.cap,
                    self.expected,
                    self.cancel,
                    self.until,
                    self.status,
                )
                .await?;
                (None, Ok(false))
            }
            Event::TurnStarted { .. } | Event::TurnEnded { .. } | Event::Idle(_) => {
                (None, Ok(false))
            }
        };
        if self
            .outcome
            .observe(session, self.usage, &event, self.cancel, self.until)
            .await?
        {
            terminal = callbacks.octos_session_end(session);
        }
        if let Some(key) = request {
            let input = callbacks
                .octos
                .get(&key)
                .ok_or(Failure::Protocol)?
                .input
                .clone();
            let summary = self
                .domain
                .request_owner_approval(self.cap.clone(), input)
                .await
                .map_err(|error| Failure::lost(AuthoritySite::ApprovalRequest, &error))?;
            callbacks.octos_acknowledged(&key, summary)?;
        }
        terminal
    }
    /// The Codex pump on the Octos session: the store future and the session
    /// read run together; an event is handled in order and the SAME store
    /// future is kept. A terminal event stops the child, and the store call
    /// still finishes, since stopping cannot cancel a commit.
    async fn pump<F: Future>(
        &mut self,
        callbacks: &mut Callbacks,
        session: &mut OwnedOctosSession,
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
                Ok(Ok(ControlUpdate::Event(event))) => self.event(callbacks, session, event).await,
                Ok(Err(error)) => {
                    let armed = callbacks
                        .octos
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
    /// Bind the approval context once Octos accepted the dispatch's turn,
    /// then open the session's control: approvals reach the owner from here
    /// on, and an unanswered one is denied at its owner bound.
    pub(crate) async fn bind_octos(
        &mut self,
        domain: &DomainStore,
        cap: &RunnerCapability,
        expected: &str,
        context: HostApprovalContext,
        deadline: Deadline,
        session: &mut OwnedOctosSession,
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
            .enable_approval_control(self.callbacks.host.octos_policy())
            .map_err(|_| Failure::Admission)?;
        session
            .enable_owner_wait_expiry()
            .map_err(|_| Failure::Admission)
    }

    /// The rest of the dispatch, from its accepted turn to Octos's idle:
    /// every event, every owner decision and every answer.
    pub(crate) async fn drive_octos(
        &mut self,
        mut drive: OctosDrive<'_>,
        session: &mut OwnedOctosSession,
    ) -> Result<(), Failure> {
        let (domain, cap, cancel, until) = (drive.domain, drive.cap, drive.cancel, drive.until);
        let mut sending: Option<Sending> = None;
        loop {
            // The owner-wait expiry (the ADR046 amendment): the host takes an
            // unanswered approval over and records the deny before any byte;
            // the loop below then answers it from that persisted choice.
            for key in self.callbacks.octos.keys().cloned().collect::<Vec<_>>() {
                let entry = self
                    .callbacks
                    .octos
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
            // bound; past it only the host's own deny on an expired approval.
            let keys: Vec<_> = self.callbacks.octos.keys().cloned().collect();
            for key in &keys {
                let entry = self.callbacks.octos.get_mut(key).ok_or(Failure::Protocol)?;
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
                let entry = self.callbacks.octos.get(key).ok_or(Failure::Protocol)?;
                matches(&grant, key, entry, context)?;
                self.callbacks
                    .octos
                    .get_mut(key)
                    .ok_or(Failure::Protocol)?
                    .grant = Some(grant);
                if authorized.terminal? {
                    return Ok(());
                }
            }
            // Every open approval answered: begin them together (the store's
            // barrier), so no answer leaves while another waits for its owner.
            let ready = !self.callbacks.octos.is_empty()
                && self.callbacks.octos.values().all(|e| {
                    e.write.is_some() || e.admitted || (e.prepared.is_some() && e.grant.is_some())
                });
            if ready {
                let deadline = self
                    .callbacks
                    .octos
                    .values()
                    .filter(|e| e.write.is_none())
                    .map(|e| e.response_deadline)
                    .min()
                    .unwrap_or(until)
                    .min(until);
                let mut ids = Vec::new();
                let mut grants = Vec::new();
                for (key, entry) in &mut self.callbacks.octos {
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
                            .octos
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
            // A new approval repairs the barrier first; an older admitted
            // frame stays retained meanwhile.
            if self
                .callbacks
                .octos
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
                    .octos
                    .iter_mut()
                    .find(|(_, e)| e.admitted && e.write.is_none() && !e.in_flight)
            {
                // Committed to the transport from here: set before the recheck
                // pump, which may deliver this approval's own withdrawal.
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
                .octos
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
            // Octos settled this approval before its first byte: the frame is
            // dropped unsent and the entry is never selected again.
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
                PreparedUpdate::Event(event) => {
                    // The frame is suspended mid-write: handle the event,
                    // recheck, then continue this same frame.
                    let ended = drive.event(&mut self.callbacks, session, event).await?;
                    if ended {
                        return Err(Failure::ApprovalCancelled);
                    }
                    sending = Some(frame);
                }
                PreparedUpdate::WriteAccepted(write) => {
                    let entry = self
                        .callbacks
                        .octos
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
                        .octos
                        .get_mut(&frame.id)
                        .ok_or(Failure::Protocol)?;
                    entry.prepared = Some(frame.prepared);
                    entry.grant = Some(frame.grant);
                    entry.recorded = true;
                    self.callbacks.release_octos_written();
                    if written.terminal? {
                        return Ok(());
                    }
                }
            }
        }
    }
}
