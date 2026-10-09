//! One disposable Octos session over a private `octos serve --stdio`
//! (ADR-193): the handshake, the dispatch's turn and every continuation turn
//! the kernel runs for its background work, until Octos reports the session
//! idle. No task, approval or cleanup authority.
mod control;
mod io;
mod observation;
use super::{Frame, Notification, Outcome, Payload, Usage};
pub use control::{
    ApprovalControlPolicy, ControlUpdate, PermissionDecision, PreparedApproval, PreparedUpdate,
};
pub use io::{Limits, StderrSnapshot, Termination, WriteProgress};
pub use observation::{
    MAX_OBSERVATIONS, Observation, ObservationKind, ObservationSource, UsageCoverage, UsageEvidence,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::Instant,
};

/// How long a session must stay quiet after its last terminal, with nothing
/// reported active, before it counts as idle. Octos reports a session's work
/// on a two-second tick, so a turn too short for one tick reports nothing;
/// two ticks and a margin.
pub const QUIET_MS: u64 = 5_000;
/// Notifications read while a request awaits its answer, kept for `next`.
const MAX_PENDING: usize = 256;
/// Turns one dispatch may run: its own and the kernel's continuations.
const MAX_TURNS: usize = 64;
/// Envelopes that move a reply or end a turn, remembered to drop redelivery.
const MAX_SEEN: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid Octos session limits")]
    Configuration,
    #[error("invalid Octos session state")]
    State,
    #[error("Octos session is closed")]
    Closed,
    #[error("Octos session operation was cancelled")]
    Cancelled,
    #[error("Octos session deadline exceeded")]
    Timeout,
    #[error("Octos session IO failed ({0})")]
    Io(&'static str),
    #[error("Octos session peer closed")]
    PeerEof,
    #[error("Octos session capacity exceeded")]
    Capacity,
    #[error("Octos session protocol failed: {0}")]
    Protocol(super::Error),
    #[error("Octos refused the request (code {0})")]
    Refused(i64),
    #[error("Octos session identity or answer mismatch")]
    Identity,
    #[error("Octos request is cancelled or already answered")]
    PermissionUnavailable,
    #[error("Octos session was closed by its host")]
    HostClosed,
}
impl From<super::Error> for Error {
    fn from(error: super::Error) -> Self {
        Self::Protocol(error)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    New,
    Ready,
    Open,
    Running,
    Idle,
    Closed,
}
/// The fixed permission profile of one dispatch (ADR-193 decision 3): never
/// `danger_full_access` and never approval policy `never`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permissions {
    /// `workspace_write`, network denied, approvals on request.
    WorkspaceWrite,
    /// `read_only`, network denied, approvals on request.
    ReadOnly,
}
/// What the dispatch's work came to when Octos went idle.
pub struct Idle {
    pub outcome: Outcome,
    pub error_code: Option<String>,
    /// The last turn's reply: its last persisted assistant text after its last
    /// tool start, as Octos's own client takes it.
    pub reply: Option<String>,
    /// The dispatch's turn and every continuation turn that ended.
    pub turns: u32,
}
/// What the host must act on while the session runs. Payloads may carry
/// private text and tool input; deliberately no Debug.
pub enum Event {
    /// A continuation turn the kernel started for the dispatch's work.
    TurnStarted {
        turn_id: String,
    },
    Approval {
        approval_id: String,
        turn_id: String,
        params: Value,
    },
    /// Octos settled an approval this host had not answered: cancelled,
    /// timed out or decided elsewhere.
    ApprovalSettled {
        approval_id: String,
    },
    ToolCall {
        params: Value,
    },
    /// One turn ended: the dispatch's own or a continuation.
    TurnEnded {
        turn_id: String,
        outcome: Outcome,
    },
    Idle(Idle),
}
#[derive(Default)]
struct Turn {
    reply: Option<String>,
    terminal: Option<(Outcome, Option<String>)>,
}

/// Private payloads deliberately have no Debug/Serialize projection. The caller
/// owns these streams; only OwnedOctosSession separately owns a process.
pub struct SessionDriver<R, W, E> {
    wire: io::Wire<R, W, E>,
    phase: Phase,
    next_id: u64,
    session_id: Option<String>,
    dispatch_turn: Option<String>,
    /// The dispatch's turn and each turn Octos announced on the session. A
    /// background child stream has its own identity and is not a turn.
    turns: BTreeMap<String, Turn>,
    last_ended: Option<String>,
    seen: BTreeSet<(String, u64)>,
    /// Octos's last report of the session's whole job.
    active: bool,
    reported_idle: bool,
    /// When every known turn had ended; reset by any new turn.
    quiet_from: Option<Instant>,
    /// Notifications read while a request awaited its answer, with the time
    /// each was received.
    pending: VecDeque<(Notification, Instant)>,
    observations: observation::State,
    control: control::State,
    source: Arc<()>,
}
impl<R, W, E> SessionDriver<R, W, E> {
    pub fn new(stdout: R, stdin: W, stderr: E, limits: Limits) -> Result<Self, Error> {
        Ok(Self {
            wire: io::Wire::new(stdout, stdin, stderr, limits)?,
            phase: Phase::New,
            next_id: 0,
            session_id: None,
            dispatch_turn: None,
            turns: BTreeMap::new(),
            last_ended: None,
            seen: BTreeSet::new(),
            active: false,
            reported_idle: false,
            quiet_from: None,
            pending: VecDeque::new(),
            observations: observation::State::default(),
            control: control::State::default(),
            source: Arc::new(()),
        })
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    pub fn dispatch_turn(&self) -> Option<&str> {
        self.dispatch_turn.as_deref()
    }
    pub fn termination(&self) -> Option<&Termination> {
        self.wire.termination()
    }
    pub fn write_progress(&self) -> Option<WriteProgress> {
        self.wire.write_progress()
    }
    pub fn stderr_snapshot(&self) -> StderrSnapshot {
        self.wire.stderr_snapshot()
    }
    pub fn close(&mut self) {
        self.fail(Error::HostClosed);
    }
    fn fail(&mut self, error: Error) {
        self.phase = Phase::Closed;
        self.observations.retire();
        self.pending.clear();
        self.control.clear();
        self.wire.close(error);
    }
    fn ours(&self, session: &str) -> bool {
        self.session_id.as_deref() == Some(session)
    }
    /// The dispatch's turn and every announced turn have ended, and Octos
    /// reports nothing still working.
    fn settled(&self) -> bool {
        self.dispatch_turn
            .as_ref()
            .and_then(|turn| self.turns.get(turn))
            .is_some_and(|turn| turn.terminal.is_some())
            && self.turns.values().all(|turn| turn.terminal.is_some())
            && !self.active
    }
    /// Settled, and either Octos said so or the session stayed quiet for
    /// longer than its report interval.
    fn idle_due(&self) -> bool {
        self.settled()
            && (self.reported_idle
                || self
                    .quiet_from
                    .is_some_and(|at| Instant::now() >= at + Duration::from_millis(QUIET_MS)))
    }
    /// A notification that starts work again, read while the totals were.
    fn restarts(&self, notification: &Notification) -> bool {
        match notification {
            Notification::TurnStarted { session_id, .. } => self.ours(session_id),
            Notification::Orchestration { session_id, active } => *active && self.ours(session_id),
            _ => false,
        }
    }
    fn first_sight(&mut self, thread: String, seq: u64) -> Result<bool, Error> {
        if self.seen.contains(&(thread.clone(), seq)) {
            return Ok(false);
        }
        if self.seen.len() >= MAX_SEEN {
            return Err(Error::Capacity);
        }
        self.seen.insert((thread, seq));
        Ok(true)
    }
    /// The state one notification changes, and the event the host must see.
    fn apply(
        &mut self,
        notification: Notification,
        received: Instant,
    ) -> Result<Option<Event>, Error> {
        Ok(match notification {
            Notification::TurnStarted {
                session_id,
                turn_id,
            } if self.ours(&session_id) => {
                if self.turns.contains_key(&turn_id) {
                    // The dispatch's own turn, announced after `turn/start`.
                    return Ok(None);
                }
                if self.turns.len() >= MAX_TURNS {
                    return Err(Error::Capacity);
                }
                self.turns.insert(turn_id.clone(), Turn::default());
                self.quiet_from = None;
                self.reported_idle = false;
                Some(Event::TurnStarted { turn_id })
            }
            Notification::Envelope(envelope)
                if self.ours(&envelope.session_id)
                    && self.turns.contains_key(&envelope.turn_id) =>
            {
                if matches!(envelope.payload, Payload::Other)
                    || !self.first_sight(envelope.thread_id, envelope.seq)?
                {
                    return Ok(None);
                }
                let turn_id = envelope.turn_id;
                let turn = self.turns.get_mut(&turn_id).ok_or(Error::State)?;
                if turn.terminal.is_some() {
                    // A terminal is a hard barrier: nothing after it moves the
                    // turn, and a second terminal cannot overwrite the first.
                    return Ok(None);
                }
                match envelope.payload {
                    Payload::AssistantPersisted { text } => {
                        turn.reply = Some(text);
                        None
                    }
                    Payload::ToolStart => {
                        turn.reply = None;
                        None
                    }
                    Payload::Terminal {
                        outcome,
                        error_code,
                        usage,
                    } => {
                        turn.terminal = Some((outcome, error_code));
                        self.observations.turn_ended(usage);
                        self.last_ended = Some(turn_id.clone());
                        if self.turns.values().all(|turn| turn.terminal.is_some()) {
                            self.quiet_from = Some(Instant::now());
                        }
                        Some(Event::TurnEnded { turn_id, outcome })
                    }
                    Payload::Other => None,
                }
            }
            Notification::Orchestration { session_id, active } if self.ours(&session_id) => {
                self.active = active;
                self.reported_idle = !active;
                None
            }
            Notification::ApprovalRequested {
                session_id,
                approval_id,
                turn_id,
                params,
            } if self.ours(&session_id) => {
                self.control.admit(&approval_id, received, &self.wire)?;
                Some(Event::Approval {
                    approval_id,
                    turn_id,
                    params,
                })
            }
            Notification::ApprovalSettled {
                session_id,
                approval_id,
            } if self.ours(&session_id) => self
                .control
                .settled(&approval_id, self.wire.writing_prepared())
                .then_some(Event::ApprovalSettled { approval_id }),
            Notification::ToolCall { params } => Some(Event::ToolCall { params }),
            // Another session's or a child stream's frames, and every other
            // notification: OUP's additive rule.
            _ => None,
        })
    }
    fn idle(&mut self) -> Result<Idle, Error> {
        let last = self.last_ended.as_ref().ok_or(Error::State)?;
        let turn = self.turns.get(last).ok_or(Error::State)?;
        let (outcome, error_code) = turn.terminal.clone().ok_or(Error::State)?;
        let reply = turn.reply.clone();
        let turns = u32::try_from(self.turns.len()).map_err(|_| Error::Capacity)?;
        self.phase = Phase::Idle;
        Ok(Idle {
            outcome,
            error_code,
            reply,
            turns,
        })
    }
    fn observe(&mut self, event: Event) -> Result<Event, Error> {
        let session = self.session_id.clone().ok_or(Error::State)?;
        self.control.observed();
        self.observations.accept(&event, &session)?;
        Ok(event)
    }
    /// One frame read outside a request: Octos's answer to an approval this
    /// host sent, or a notification and the event it makes, observed.
    fn receive(&mut self, received: io::Received) -> Result<Option<Event>, Error> {
        match received.message {
            Frame::Response { id, .. } => {
                if !self.control.answered(&id) {
                    return Err(Error::Identity);
                }
                Ok(None)
            }
            Frame::Notification(notification) => match self.apply(notification, received.at)? {
                Some(event) => self.observe(event).map(Some),
                None => Ok(None),
            },
        }
    }
}
impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, E: AsyncRead + Unpin> SessionDriver<R, W, E> {
    /// One request and its answer. Notifications that arrive first are kept,
    /// in order, for `next`.
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, Error> {
        self.next_id = self.next_id.checked_add(1).ok_or(Error::Capacity)?;
        let id = format!("hagency-{}", self.next_id);
        let until = self.wire.event_deadline();
        self.wire
            .send(super::request(&id, method, params)?, until)
            .await?;
        loop {
            match self.wire.next(until).await?.message {
                Frame::Response {
                    id: answered,
                    outcome,
                } => {
                    if answered == id {
                        return outcome.map_err(|error| Error::Refused(error.code));
                    }
                    // Octos's answer to an approval this host sent earlier.
                    if !self.control.answered(&answered) {
                        return Err(Error::Identity);
                    }
                }
                Frame::Notification(notification) => {
                    if self.pending.len() >= MAX_PENDING {
                        return Err(Error::Capacity);
                    }
                    self.pending.push_back((notification, Instant::now()));
                }
            }
        }
    }
    /// `client_hello` with Hagency's fixed features; the server must speak
    /// `octos-ui/v1alpha1`.
    pub async fn hello(&mut self) -> Result<(), Error> {
        let operation = Operation::new(self, Phase::New)?;
        let result = operation.driver.hello_inner().await;
        operation.finish(result)
    }
    async fn hello_inner(&mut self) -> Result<(), Error> {
        let answer = self
            .call(
                "client_hello",
                json!({"transport":"stdio","client":"hagency",
                    "supported_features":super::FEATURES}),
            )
            .await?;
        if answer["type"] != "server_hello"
            || answer["capabilities"]["version"]["protocol"] != super::PROTOCOL
        {
            return Err(Error::Protocol(super::Error::Unsupported));
        }
        self.phase = Phase::Ready;
        Ok(())
    }
    /// The permission profile, then a fresh session bound to `cwd`. Octos must
    /// bind exactly that workspace and profile.
    pub async fn open(
        &mut self,
        session_id: &str,
        profile_id: &str,
        cwd: &str,
        permissions: Permissions,
    ) -> Result<(), Error> {
        let operation = Operation::new(self, Phase::Ready)?;
        let result = operation
            .driver
            .open_inner(session_id, profile_id, cwd, permissions)
            .await;
        operation.finish(result)
    }
    async fn open_inner(
        &mut self,
        session_id: &str,
        profile_id: &str,
        cwd: &str,
        permissions: Permissions,
    ) -> Result<(), Error> {
        if !super::session_key(session_id, profile_id) || !cwd.starts_with('/') {
            return Err(Error::Protocol(super::Error::Input));
        }
        let mode = match permissions {
            Permissions::WorkspaceWrite => "workspace_write",
            Permissions::ReadOnly => "read_only",
        };
        // Before `session/open`, so the session's runtime is built with it.
        let applied = self
            .call(
                "permission/profile/set",
                json!({"session_id":session_id,
                    "update":{"mode":mode,"network":"deny","approval_policy":"on-request"}}),
            )
            .await?;
        if applied["current"]["mode"] != mode || applied["current"]["network"] != "deny" {
            return Err(Error::Identity);
        }
        let answer = self
            .call(
                "session/open",
                json!({"session_id":session_id,"profile_id":profile_id,"cwd":cwd}),
            )
            .await?;
        let opened = &answer["opened"];
        if opened["session_id"] != session_id
            || opened["workspace_root"] != cwd
            || opened
                .get("active_profile_id")
                .is_some_and(|active| active != profile_id)
        {
            return Err(Error::Identity);
        }
        self.session_id = Some(session_id.to_owned());
        self.phase = Phase::Open;
        Ok(())
    }
    /// The dispatch's one turn: accepted, not executed.
    pub async fn start_turn(&mut self, turn_id: &str, text: &str) -> Result<(), Error> {
        let operation = Operation::new(self, Phase::Open)?;
        let result = operation.driver.start_inner(turn_id, text).await;
        operation.finish(result)
    }
    async fn start_inner(&mut self, turn_id: &str, text: &str) -> Result<(), Error> {
        if !super::uuid(turn_id) || text.is_empty() || text.len() > super::MAX_TEXT_BYTES {
            return Err(Error::Protocol(super::Error::Input));
        }
        let session_id = self.session_id.clone().ok_or(Error::State)?;
        self.turns.insert(turn_id.to_owned(), Turn::default());
        self.dispatch_turn = Some(turn_id.to_owned());
        let answer = self
            .call(
                "turn/start",
                json!({"session_id":session_id,"turn_id":turn_id,
                    "input":[{"kind":"text","text":text}]}),
            )
            .await?;
        if answer["accepted"] != true {
            return Err(Error::Identity);
        }
        self.phase = Phase::Running;
        Ok(())
    }
    /// The next event the host must see, ending with `Idle`. Every return
    /// carries exactly one observation (`last_observation`).
    pub async fn next(&mut self) -> Result<Event, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = match operation
            .driver
            .next_inner::<std::future::Pending<()>>(None)
            .await
        {
            Ok(ControlUpdate::Event(event)) => Ok(event),
            Ok(ControlUpdate::Control(())) => Err(Error::State),
            Err(error) => Err(error),
        };
        operation.finish(result)
    }
    /// The next event, or the host's own future when one is given and finishes
    /// first. The quiet wait, the owner bounds and the wire's deadlines bound
    /// every read.
    async fn next_inner<F: Future + ?Sized>(
        &mut self,
        mut control: Option<Pin<&mut F>>,
    ) -> Result<ControlUpdate<F::Output>, Error> {
        loop {
            if let Some((notification, received)) = self.pending.pop_front() {
                if let Some(event) = self.apply(notification, received)? {
                    return self.observe(event).map(ControlUpdate::Event);
                }
                continue;
            }
            if self.idle_due() {
                if let Some(event) = self.finish_idle().await? {
                    return self.observe(event).map(ControlUpdate::Event);
                }
                continue;
            }
            let wait = self.control.deadline(&self.wire)?;
            let quiet = self
                .quiet_from
                .filter(|_| self.settled())
                .map(|at| at + Duration::from_millis(QUIET_MS));
            let until = quiet.map_or(wait, |at| at.min(wait));
            let read = match control.as_mut() {
                Some(control) => match self.wire.next_or_control(control.as_mut(), until).await {
                    Ok(io::Controlled::Message(received)) => Ok(received),
                    Ok(io::Controlled::Control(output)) => {
                        return Ok(ControlUpdate::Control(output));
                    }
                    Err(error) => Err(error),
                },
                None => self.wire.next(until).await,
            };
            match read {
                Ok(received) => {
                    if let Some(event) = self.receive(received)? {
                        return Ok(ControlUpdate::Event(event));
                    }
                }
                // Only the quiet wait may end there; the wire's own event and
                // lifetime deadlines stay failures.
                Err(Error::Timeout)
                    if quiet.is_some_and(|at| at < wait && at < self.wire.lifetime())
                        && self.idle_due() => {}
                Err(error) => return Err(error),
            }
        }
    }
    /// The session's own totals close the usage record: they also cover its
    /// sub-agents. A refused or empty read keeps the turns' own sums. Work
    /// that began while the totals were read is not idle.
    async fn finish_idle(&mut self) -> Result<Option<Event>, Error> {
        let session_id = self.session_id.clone().ok_or(Error::State)?;
        let totals = match self
            .call("session/status/read", json!({"session_id":session_id}))
            .await
        {
            Ok(answer) => session_totals(&answer["usage"]),
            Err(Error::Refused(_)) => None,
            Err(error) => return Err(error),
        };
        if self.pending.iter().any(|(n, _)| self.restarts(n)) {
            return Ok(None);
        }
        self.observations.session_totals(totals);
        Ok(Some(Event::Idle(self.idle()?)))
    }
}
/// `session/status/read` usage. An empty object (no recorded run, or no
/// ledger) is unknown, never zero.
fn session_totals(usage: &Value) -> Option<Usage> {
    let counter = |key: &str| {
        usage
            .get(key)
            .and_then(Value::as_u64)
            .filter(|n| *n <= 9_007_199_254_740_991)
    };
    Some(Usage {
        input: counter("input_tokens")?,
        output: counter("output_tokens")?,
        reasoning: 0,
        cache_read: counter("cached_input_tokens")?,
        cache_write: counter("cache_write_input_tokens")?,
    })
}
struct Operation<'a, R, W, E> {
    driver: &'a mut SessionDriver<R, W, E>,
    finished: bool,
}
impl<'a, R, W, E> Operation<'a, R, W, E> {
    fn new(driver: &'a mut SessionDriver<R, W, E>, expected: Phase) -> Result<Self, Error> {
        if driver.phase == Phase::Closed {
            return Err(Error::Closed);
        }
        if driver.phase != expected {
            driver.fail(Error::State);
            return Err(Error::State);
        }
        Ok(Self {
            driver,
            finished: false,
        })
    }
    fn finish<T>(mut self, result: Result<T, Error>) -> Result<T, Error> {
        if let Err(error) = result {
            self.driver.fail(error);
        }
        self.finished = true;
        result
    }
}
impl<R, W, E> Drop for Operation<'_, R, W, E> {
    fn drop(&mut self) {
        if !self.finished {
            self.driver.fail(Error::Cancelled);
        }
    }
}
