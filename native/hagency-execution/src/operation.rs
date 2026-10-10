use crate::registration::{Gate, RegistrationSlot, WorkspaceRegistration};
use crate::usage::{UsageFailure, UsageRun, UsageStatus};
use crate::workspace::{Binding, Handoff};
use crate::{Host, Limits, SharedHost, StartedWorkspace};
use hagency_core::tasks::{RunnerCapability, RunnerCommand, Task, TaskState};
use hagency_runtime::{
    claude::{EventKind, Message},
    codex::session::{self, Outcome, Update},
    owned::{Cleanup, OwnedClaudeSession, OwnedOctosSession, OwnedSession, StartError},
};
use hagency_store::{DomainStore, OwnedFailure, OwnedObservation};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};
use tokio::{
    sync::{oneshot, watch},
    time::{Instant, MissedTickBehavior, interval},
};

/// ADR-183 decision D: the turn has no bridge-side bound. The only lifetime a
/// running turn keeps is the runtime transport's own protocol ceiling
/// (`hagency-runtime` refuses a longer one and checks it on every read); the
/// host grants the transport that ceiling instead of the budget, and hands
/// the same instant to every turn-phase and post-turn wait, so that only
/// cancellation (SIGTERM, the owner's stop) and the runtime's own ceiling end
/// them. Not a budget: the budget is notify-only once the turn has started.
pub(crate) const TURN_CEILING_MS: u64 = hagency_runtime::codex::MAX_REQUEST_MS;

/// The check whose refusal produced a lost authority. Fixed labels only
/// (ADR-175); diagnostic, never authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthoritySite {
    LeaseRenew,
    DispatchCheck,
    AccountCheck,
    LocalCodexCheck,
    LocalClaudeCheck,
    LocalOctosCheck,
    TaskMcpBind,
    WarmRoot,
    WarmScope,
    WarmProvision,
    WarmQualify,
    WarmReady,
    WarmActivate,
    WarmDispatch,
    CommandChannel,
    ApprovalBind,
    ApprovalExpiry,
    ApprovalMaintain,
    ApprovalRequest,
    ApprovalResponse,
    ApprovalBegin,
    ApprovalCheck,
    ApprovalApplication,
    FactoryAccount,
    HostClaim,
}
impl AuthoritySite {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LeaseRenew => "lease_renew",
            Self::DispatchCheck => "dispatch_check",
            Self::AccountCheck => "account_check",
            Self::LocalCodexCheck => "local_codex_check",
            Self::LocalClaudeCheck => "local_claude_check",
            Self::LocalOctosCheck => "local_octos_check",
            Self::TaskMcpBind => "task_mcp_bind",
            Self::WarmRoot => "warm_root",
            Self::WarmScope => "warm_scope",
            Self::WarmProvision => "warm_provision",
            Self::WarmQualify => "warm_qualify",
            Self::WarmReady => "warm_ready",
            Self::WarmActivate => "warm_activate",
            Self::WarmDispatch => "warm_dispatch",
            Self::CommandChannel => "command_channel",
            Self::ApprovalBind => "approval_bind",
            Self::ApprovalExpiry => "approval_expiry",
            Self::ApprovalMaintain => "approval_maintain",
            Self::ApprovalRequest => "approval_request",
            Self::ApprovalResponse => "approval_response",
            Self::ApprovalBegin => "approval_begin",
            Self::ApprovalCheck => "approval_check",
            Self::ApprovalApplication => "approval_application",
            Self::FactoryAccount => "factory_account",
            Self::HostClaim => "host_claim",
        }
    }
}
/// The store's own word for the refusal, or the host's for a physical check:
/// revoked, timed out and busy are different facts and were one word before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityCause {
    Revoked,
    Generation,
    Quarantined,
    State,
    Busy,
    Unavailable,
    TimedOut,
    NotFound,
    Conflict,
    Locked,
    Io,
    Other,
}
impl AuthorityCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Revoked => "revoked",
            Self::Generation => "generation",
            Self::Quarantined => "quarantined",
            Self::State => "state",
            Self::Busy => "busy",
            Self::Unavailable => "unavailable",
            Self::TimedOut => "timed_out",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Locked => "locked",
            Self::Io => "io",
            Self::Other => "other",
        }
    }
}
impl From<&hagency_store::Error> for AuthorityCause {
    fn from(error: &hagency_store::Error) -> Self {
        use hagency_store::Error::*;
        match error {
            RunnerAuthority | LocalAuthority => Self::Revoked,
            Generation => Self::Generation,
            Quarantined => Self::Quarantined,
            State => Self::State,
            Busy => Self::Busy,
            Unavailable | PlatformUnavailable => Self::Unavailable,
            OutcomeUnknown => Self::TimedOut,
            NotFound => Self::NotFound,
            Conflict => Self::Conflict,
            Locked => Self::Locked,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    #[error("host dispatch admission refused")]
    Admission,
    #[error("host operation cancelled")]
    Cancelled,
    #[error("durable start response unknown; no child launched")]
    StartUnknown,
    #[error("usage source binding failed or its response is unknown; no child launched")]
    UsageBinding,
    #[error("owned native child startup failed")]
    SpawnFailed,
    /// Which check failed and what the store (or the host) said: kept beside
    /// the unchanged verdict so the reason is no longer discarded (ADR-181).
    #[error("dispatch authority expired, changed or was revoked ({site:?}: {cause:?})")]
    LostAuthority {
        site: AuthoritySite,
        cause: AuthorityCause,
    },
    #[error("native runner protocol failed")]
    Protocol,
    #[error("owned approval application is unavailable")]
    UnsupportedApproval,
    #[error("owned approval capacity exhausted")]
    ApprovalCapacity,
    #[error("original approval callback was cancelled before response admission")]
    ApprovalCancelled,
    #[error("host operation deadline expired")]
    Deadline,
    #[error("whole-tree cleanup remains unproven")]
    CleanupUnknown,
    #[error("domain settlement outcome unknown")]
    SettlementUnknown,
    #[error("native runner peer vanished before the approval frame's first byte")]
    PeerUnavailable,
    #[error("host worker failed")]
    Worker,
    #[error(
        "no native runner exists for framework {framework}; the dispatch is refused before any spawn"
    )]
    UnsupportedRunner { framework: String },
}
impl Failure {
    /// A lost authority named by the check and the store's own refusal.
    pub(crate) fn lost(site: AuthoritySite, error: &hagency_store::Error) -> Self {
        Self::LostAuthority {
            site,
            cause: AuthorityCause::from(error),
        }
    }
    /// A lost authority from a physical or channel check with no store word.
    pub(crate) fn lost_io(site: AuthoritySite) -> Self {
        Self::LostAuthority {
            site,
            cause: AuthorityCause::Io,
        }
    }
    fn observation(&self) -> OwnedFailure {
        match self {
            Self::Admission | Self::UsageBinding => OwnedFailure::Admission,
            Self::Cancelled => OwnedFailure::Cancelled,
            Self::StartUnknown => OwnedFailure::StartUnknown,
            Self::SpawnFailed => OwnedFailure::SpawnFailed,
            Self::LostAuthority { .. } => OwnedFailure::LostAuthority,
            Self::Protocol | Self::Worker => OwnedFailure::Protocol,
            Self::UnsupportedApproval | Self::ApprovalCapacity | Self::ApprovalCancelled => {
                OwnedFailure::UnsupportedApproval
            }
            Self::Deadline => OwnedFailure::Deadline,
            Self::CleanupUnknown => OwnedFailure::CleanupUnknown,
            Self::SettlementUnknown => OwnedFailure::SettlementUnknown,
            Self::UnsupportedRunner { framework } => OwnedFailure::UnsupportedRunner {
                framework: framework.clone(),
            },
            Self::PeerUnavailable => OwnedFailure::PeerUnavailable,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    NotStarted,
    Completed,
    Failed,
    Interrupted,
    Unsupported,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settlement {
    Pending,
    Completed,
    CanonicalReplyReady,
    Negative(OwnedObservation),
    Unknown,
}

/// Which store refusal produced `Failure::SettlementUnknown`. A fixed
/// discriminant, never the store's own error text: this carries no path,
/// payload or capability material, and cannot become execution authority,
/// retry, reply or lease input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettlementCause {
    /// The command never entered the writer queue (`Busy`).
    QueueBusy,
    /// The writer is gone (`Unavailable`).
    QueueUnavailable,
    /// The writer reply did not arrive inside its bound (`OutcomeUnknown`).
    ReplyTimedOut,
    /// The writer refused this scope, fence, route or deadline.
    RunnerAuthority,
    /// The writer refused its own current state.
    State,
    /// An unrelated dispatch or lease still holds the scope.
    Quarantined,
    /// Durable store capacity, schema or IO refusal.
    Storage,
    /// The acceptance write was refused or abandoned and the ordered reconcile
    /// read found no accepted row. Set directly by the approval acceptance pump,
    /// never derived from a store error: it names a *missing record*, not a
    /// refused call. Carries no path, payload or capability material, and cannot
    /// become execution authority, retry, reply or lease input.
    AcceptanceUnrecorded,
}
impl SettlementCause {
    pub(crate) fn of(error: &hagency_store::Error) -> Self {
        match error {
            hagency_store::Error::Busy => Self::QueueBusy,
            hagency_store::Error::Unavailable => Self::QueueUnavailable,
            hagency_store::Error::OutcomeUnknown => Self::ReplyTimedOut,
            hagency_store::Error::RunnerAuthority => Self::RunnerAuthority,
            hagency_store::Error::State => Self::State,
            hagency_store::Error::Quarantined => Self::Quarantined,
            _ => Self::Storage,
        }
    }
    /// Fixed trace label. Never store text: the discriminant is the whole datum.
    #[cfg(any(test, feature = "test-diagnostics"))]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::QueueBusy => "queue-busy",
            Self::QueueUnavailable => "queue-unavailable",
            Self::ReplyTimedOut => "reply-timed-out",
            Self::RunnerAuthority => "runner-authority",
            Self::State => "state",
            Self::Quarantined => "quarantined",
            Self::Storage => "storage",
            Self::AcceptanceUnrecorded => "acceptance-unrecorded",
        }
    }
}
/// One bounded fresh-clock read of the canonical task status. Observation
/// only: never execution, release, retry or reply authority.
async fn observed_canonical_status(
    domain: &DomainStore,
    cap: &RunnerCapability,
    scope: &hagency_store::OwnedDispatchScope,
) -> Option<TaskState> {
    domain
        .runner_command(
            cap.clone(),
            RunnerCommand::Task {
                id: scope.task().id.clone(),
            },
        )
        .await
        .ok()
        .and_then(|value| serde_json::from_value::<Task>(value).ok())
        .filter(|task| task.id == scope.task().id && task.session_id == scope.task().session_id)
        .map(|task| task.status)
}

/// Record the cause and preserve the existing terminal verdict exactly.
///
/// The **first** cause on a path is the root cause and is never replaced. The
/// reconcile sets `AcceptanceUnrecorded` when its ordered read conclusively
/// found no accepted row; a later unrelated refusal (`observe_owned_completion`,
/// `publish_owned_completion`, `complete_owned_dispatch`) would otherwise
/// overwrite that verdict with a refusal-derived marker and hide the record
/// that actually went missing. So this only fills an unset marker.
fn settlement_failure(report: &mut Report, error: &hagency_store::Error) -> Failure {
    if report.settlement_cause.is_none() {
        report.settlement_cause = Some(SettlementCause::of(error));
    }
    Failure::SettlementUnknown
}

/// H5: which drive outcomes still observe completion custody on the failure
/// path. The excluded causes never reach settlement, so a store refusal there
/// must not pin a `settlement_cause` onto them — the same first-cause rule
/// brief 14 installed, now guarding the marker, not just its overwrite.
/// `PeerUnavailable` joins the exclusions: a named peer-gone refusal never
/// carries a settlement cause. `SettlementUnknown` is excluded by the
/// precedence rule (ADR-046): a conclusive negative reconcile surfaces
/// unchanged and never consults custody. Generic over the drive's success
/// payload (the early completion path drives `()`, the later one the
/// `Report`).
fn observes_completion<T>(drive: &Result<T, Failure>) -> bool {
    !matches!(
        drive,
        Err(Failure::Cancelled
            | Failure::Deadline
            | Failure::UnsupportedApproval
            | Failure::PeerUnavailable
            | Failure::SettlementUnknown)
    )
}

/// Fixed diagnostics from the original owned runtime, never execution authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStage {
    Initialize,
    ThreadStart,
    TurnStart,
    Update,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeWriteObservation {
    pub accepted_bytes: usize,
    pub total_bytes: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeObservation {
    pub stage: RuntimeStage,
    pub server_request: Option<&'static str>,
    /// The refused server request's own method name, bounded and printable.
    /// `server_request` above is a fixed vocabulary label and reads `"unknown"`
    /// for a method native does not know — exactly the case board #87 must
    /// NAME so a live failure is actionable. Diagnostic only (ADR-181).
    pub server_request_method: Option<String>,
    pub refused_notification: Option<&'static str>,
    pub session_error: Option<session::Error>,
    pub transport_cause: Option<hagency_runtime::codex::transport::Error>,
    pub pending_requests: Option<usize>,
    pub pending_server_requests: Option<usize>,
    pub write: Option<RuntimeWriteObservation>,
}
impl RuntimeObservation {
    pub(crate) fn capture(stage: RuntimeStage, runner: &OwnedSession) -> Self {
        let termination = runner.transport_termination();
        Self {
            stage,
            server_request: runner.last_server_request(),
            server_request_method: runner.last_server_request_method().map(str::to_owned),
            refused_notification: runner.refused_notification(),
            session_error: match runner.protocol_outcome() {
                Some(Outcome::Unknown { reason }) => Some(*reason),
                _ => None,
            },
            transport_cause: termination.map(|t| t.cause),
            pending_requests: termination.map(|t| t.pending_requests),
            pending_server_requests: termination.map(|t| t.pending_server_requests),
            write: termination
                .and_then(|t| t.unconfirmed_write.as_ref())
                .map(|w| RuntimeWriteObservation {
                    accepted_bytes: w.accepted_bytes,
                    total_bytes: w.total_bytes,
                }),
        }
    }
}

/// ADR-183 decision D: the operation budget elapsed while the turn was still
/// running. Recorded as the attempt's `over_budget` event, notified once to
/// the thread and shown by the host as the fixed status word `over_budget`;
/// nothing acts on it. `elapsed_ms` is the attempt's age when the budget
/// elapsed, then its whole age once the turn has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct OverBudget {
    pub budget_ms: u64,
    pub elapsed_ms: u64,
}
/// The host's live view of the budget while the operation runs: `None` until
/// the budget elapses during the turn, then the fact. Taken before `wait`
/// pins the operation, like the approval requests; it grants nothing.
pub struct BudgetWatch(watch::Receiver<Option<OverBudget>>);
impl BudgetWatch {
    pub fn current(&self) -> Option<OverBudget> {
        *self.0.borrow()
    }
    /// Resolves once the budget has elapsed during the turn (at once if it
    /// already has); `None` when the operation ended without that.
    pub async fn exceeded(&mut self) -> Option<OverBudget> {
        loop {
            if let Some(over) = *self.0.borrow_and_update() {
                return Some(over);
            }
            if self.0.changed().await.is_err() {
                return *self.0.borrow();
            }
        }
    }
}

/// Private host result: no Serialize/Debug or automatic console/Matrix projection.
/// An unresolved owner remains retained here; retrying stop never clears a lease.
pub struct Report {
    pub protocol: Protocol,
    pub cleanup: Cleanup,
    /// ADR-183 decision D: set when the budget elapsed during the turn; the
    /// host projects it as `over_budget` with the elapsed time.
    pub over_budget: Option<OverBudget>,
    pub canonical_status: Option<TaskState>,
    pub settlement: Settlement,
    /// Diagnostic only: which store refusal was observed first. Set once,
    /// never used for authority, retry, reply or lease decisions.
    pub settlement_cause: Option<SettlementCause>,
    pub failure: Option<Failure>,
    /// The leader's exit identity (`code:N` / `signal:N`) as the guardian
    /// reported it, and the bounded tails of the runtime's and the guardian's
    /// stderr, captured at stop for the attempt's record (ADR-181). Evidence
    /// only: nothing reads them to decide anything.
    pub exit_identity: Option<String>,
    pub stderr_tail: String,
    pub guardian_stderr_tail: String,
    /// The provider's own reason for ending the turn, bounded, for the
    /// attempt's record (board #110). TS surfaces the same words
    /// (`router/src/runner.ts:793`); without them a usage-limit refusal reached
    /// the operator as a bare `protocol` fault with no reason at all. Evidence
    /// only — it decides no outcome, and it never carries a path or capability.
    pub turn_failure: String,
    /// Fixed original startup diagnostics, never authority or child-stop proof.
    startup_error: Option<StartError>,
    runtime_observation: Option<RuntimeObservation>,
    runtime_stage: RuntimeStage,
    pub text: Option<String>,
    pub(crate) owner: Option<OwnedSession>,
    /// ADR-192: this dispatch's Claude session when its host runs Claude, under
    /// the same custody rule as `owner`: kept until a stop proves release.
    pub(crate) claude: Option<OwnedClaudeSession>,
    /// ADR-193: this dispatch's `octos serve --stdio` when its host runs Octos,
    /// under the same custody rule. Boxed: a report crosses nested host futures
    /// by value, and an inline session adds its 34 KB to every dispatch's.
    pub(crate) octos: Option<Box<OwnedOctosSession>>,
    pub(crate) warm: Option<crate::warm::Binding>,
    factory: Option<crate::warm::Binding>,
    /// Custody for a child whose spawn was abandoned at the deadline (ADR-053
    /// amendment): the detached blocking thread try-sends the spawn result here;
    /// the teardown adopts any late OwnedSession before capture, or it drops on
    /// the thread — SupervisedProcess::Drop's socket EOF triggers the guardian's
    /// process-group kill. Never blocks: a gone receiver is the backstop's trigger.
    late_child: Option<oneshot::Receiver<Result<OwnedSession, StartError>>>,
    approvals: Option<crate::approval::ApprovalRun>,
    pub(crate) live: Option<crate::approval::Reservation>,
    reconciliation: Option<(DomainStore, RunnerCapability, OwnedFailure)>,
    usage: Option<UsageRun>,
    // After owner in field order: actual cleanup drops before retained roots.
    workspace: Option<Arc<Binding>>,
    stopped_scope: Option<hagency_store::OwnedDispatchScope>,
    stop_inspection: crate::inspection::Inspection,
    pub(crate) account: Option<hagency_store::ManagedLaunch>,
    local_codex: Option<Arc<crate::LocalBinding>>,
    handoff: Handoff,
    registration: Option<Gate>,
}
impl Report {
    pub(crate) fn new(handoff: Handoff) -> Self {
        Self {
            protocol: Protocol::NotStarted,
            cleanup: Cleanup::Pending,
            over_budget: None,
            exit_identity: None,
            stderr_tail: String::new(),
            guardian_stderr_tail: String::new(),
            turn_failure: String::new(),
            canonical_status: None,
            settlement: Settlement::Pending,
            settlement_cause: None,
            failure: None,
            startup_error: None,
            runtime_observation: None,
            runtime_stage: RuntimeStage::Initialize,
            text: None,
            owner: None,
            claude: None,
            octos: None,
            warm: None,
            factory: None,
            late_child: None,
            approvals: None,
            live: None,
            reconciliation: None,
            usage: None,
            workspace: None,
            stopped_scope: None,
            stop_inspection: crate::inspection::Inspection::unavailable(),
            account: None,
            local_codex: None,
            handoff,
            registration: None,
        }
    }
    /// Immutable original observation survives retry_stop discarding a stopped
    /// owner. No descriptor, process ID, payload or private stderr is exposed.
    pub fn runtime_observation(&self) -> Option<&RuntimeObservation> {
        self.runtime_observation.as_ref()
    }
    pub fn startup_error(&self) -> Option<StartError> {
        self.startup_error
    }
    pub fn stop_inspection_status(&self) -> crate::StopInspectionStatus {
        self.stop_inspection.status()
    }
    /// Re-record only the original frozen observation after receipt loss. This
    /// cannot scan a new root or mint evidence from caller-modified diagnostics.
    pub async fn retry_stop_inspection(&mut self) -> crate::StopInspectionStatus {
        self.stop_inspection.retry().await
    }
    #[cfg(test)]
    pub(crate) fn approval_custody(&self) -> (usize, usize, usize, usize) {
        self.approvals
            .as_ref()
            .map_or((0, 0, 0, 0), crate::approval::ApprovalRun::custody)
    }
    pub fn usage_status(&self) -> UsageStatus {
        self.usage
            .as_ref()
            .map_or_else(UsageStatus::default, UsageRun::status)
    }
    /// Explicit retry of one retained historical observation. Never restarts
    /// capture, execution, cleanup, canonical completion or message delivery.
    pub async fn retry_usage(&mut self) -> Result<UsageStatus, UsageFailure> {
        match &mut self.usage {
            Some(usage) => usage.record_pending().await,
            None => Ok(UsageStatus::default()),
        }
    }
    /// A bounded negative-only retry after a lost database response. Cancellation
    /// leaves the same retained receipt/capability for a subsequent explicit retry.
    /// This never retries successful settlement or clears a dirty lease.
    pub async fn retry_reconcile(&mut self) -> Settlement {
        self.retry_stop();
        if let Some((domain, cap, failure)) = &self.reconciliation
            && let Ok(value) = domain
                .observe_owned_failure(cap.clone(), failure.clone())
                .await
        {
            self.settlement = Settlement::Negative(value);
            self.reconciliation = None;
        }
        self.settlement
    }

    /// Local physical custody only, never task/grant/lease settlement authority.
    /// Pending without an owner is the existing pre-child result. Unknown spawn
    /// or incomplete stop observations remain retained even without a returned owner.
    pub fn retains_process_custody(&self) -> bool {
        self.owner.is_some()
            || self.claude.is_some()
            || self.octos.is_some()
            || match self.cleanup {
                Cleanup::Pending => false,
                Cleanup::Observed(_) => !stopped(self.cleanup),
                Cleanup::Unknown { .. } => true,
            }
    }

    pub fn retry_stop(&mut self) -> Cleanup {
        if let Some(owner) = &mut self.owner {
            self.cleanup = owner.stop();
        } else if let Some(claude) = &mut self.claude {
            self.cleanup = claude.stop();
        } else if let Some(octos) = &mut self.octos {
            self.cleanup = octos.stop();
        } else if self.cleanup == Cleanup::Pending
            && self.late_child.is_none()
            && let Some(live) = &mut self.live
        {
            live.release();
        }
        // F3 (macOS-reachable): the deferred approval entries are released as
        // soon as the *leader* stopped on every OS — their fate is settled by
        // the turn-end rule the moment the child is gone — even where the
        // supervisor cannot prove `whole_tree_stopped` (macOS). The live
        // reservation and the retained owner still wait on the full
        // `whole_tree_stopped` proof: custody/settlement must not claim "no
        // detached child remains" on a leader-only stop.
        if leader_stopped(self.cleanup)
            && let Some(approvals) = &mut self.approvals
        {
            approvals.stopped();
        }
        if stopped(self.cleanup) {
            if let Some(live) = &mut self.live {
                live.release();
            }
            self.owner.take();
            self.claude.take();
            self.octos.take();
        }
        self.cleanup
    }
}
/// The release predicate (F3, macOS-reachable): the owned child's *leader* is
/// gone, so the run is physically over even where the supervisor cannot prove
/// the whole detached tree is gone (`whole_tree_stopped` is Linux-only; the
/// macOS supervisor refuses the stronger report and the non-Linux scopes force
/// it `false`). The deferred approval entries are released on this — not on the
/// stricter `whole_tree_stopped` — because their fate is settled by the
/// turn-end rule the moment the leader stops, on every OS. `signals_accepted`
/// is deliberately NOT a conjunct: macOS can report it false for a leader that
/// already exited before the stop was signalled (EPERM on a zombie-only group,
/// `signalled = group_accepted && child_accepted` in `unix.rs`), and the child's
/// exit — not signal delivery — is the physical fact that settles these
/// never-transmitted frames. This must stay out of the custody/settlement
/// paths, which still need the full `whole_tree_stopped` proof to claim "no
/// detached child remains".
fn leader_stopped(cleanup: Cleanup) -> bool {
    matches!(cleanup, Cleanup::Observed(report) if report.scope.leader_exited)
}
fn stopped(cleanup: Cleanup) -> bool {
    matches!(cleanup, Cleanup::Observed(report) if report.scope.whole_tree_stopped && report.scope.leader_exited && report.scope.signals_accepted)
}

/// One host operation, one retained OS worker, one result slot. No scheduler or
/// detached cleanup. Dropping a waiting future requests cancellation; the worker
/// still stops and reconciles. Dropping this handle joins it synchronously, with
/// the operation-budget-plus-30 s allowance documented by `Limits`. It must
/// not be dropped on a latency-sensitive HTTP/UI worker. There is no forced
/// thread termination or claim that an unknown child/guardian outcome is clean.
pub struct Operation {
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    result: oneshot::Receiver<Box<Report>>,
    workspace: Handoff,
    registration: RegistrationSlot,
    approvals: Option<crate::ApprovalRequests>,
    continuation: Arc<Continuation>,
    budget: watch::Receiver<Option<OverBudget>>,
}
/// Private original result custody. Neither Report mutation nor a caller's
/// cleanup/status metadata can populate this slot or acknowledge its worker.
pub(crate) struct Continuation {
    binding: Mutex<Option<crate::warm::Binding>>,
    acknowledged: AtomicBool,
    cancel: Arc<AtomicBool>,
}
impl Continuation {
    pub(crate) fn binding(&self) -> Result<crate::warm::Binding, Failure> {
        if !self.acknowledged.load(Ordering::Acquire) {
            return Err(Failure::Admission);
        }
        self.binding
            .lock()
            .map_err(|_| Failure::Worker)?
            .clone()
            .ok_or(Failure::Admission)
    }
    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    fn record(&self, report: &Report) {
        let usage = report.usage_status();
        if report.failure.is_none()
            && stopped(report.cleanup)
            && matches!(
                report.settlement,
                Settlement::Completed | Settlement::CanonicalReplyReady
            )
            && report.owner.is_none()
            && report.late_child.is_none()
            && usage.bound
            && usage.closed
            && !usage.pending
            && !usage.rejected
            && usage.failure.is_none()
            && let Ok(mut binding) = self.binding.lock()
        {
            *binding = report.factory.as_ref().or(report.warm.as_ref()).cloned();
        }
    }
}
pub(crate) type OwnedWork = Box<dyn FnOnce(&tokio::runtime::Runtime, Option<Box<Report>>) + Send>;
impl Operation {
    pub fn start(
        domain: DomainStore,
        capability: RunnerCapability,
        host: Host,
        limits: Limits,
    ) -> Result<Self, Failure> {
        Self::start_mode(domain, capability, host.into_shared(), limits, false)
    }
    /// Require the host to register the exact Started workspace before any child.
    pub fn start_requiring_workspace(
        domain: DomainStore,
        capability: RunnerCapability,
        host: Host,
        limits: Limits,
    ) -> Result<Self, Failure> {
        Self::start_mode(domain, capability, host.into_shared(), limits, true)
    }
    /// Require workspace registration while retaining the same originally
    /// admitted host configuration for another sequential operation.
    pub fn start_requiring_workspace_shared(
        domain: DomainStore,
        capability: RunnerCapability,
        host: SharedHost,
        limits: Limits,
    ) -> Result<Self, Failure> {
        Self::start_mode(domain, capability, host, limits, true)
    }
    fn start_mode(
        domain: DomainStore,
        capability: RunnerCapability,
        host: SharedHost,
        limits: Limits,
        required: bool,
    ) -> Result<Self, Failure> {
        let (mut operation, work) =
            Self::prepare(domain, capability, host, limits, required, false)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Failure::Worker)?;
        let worker = std::thread::Builder::new()
            .name("hagency-owned-dispatch".into())
            .spawn(move || work(&runtime, None))
            .map_err(|_| Failure::Worker)?;
        operation.worker = Some(worker);
        Ok(operation)
    }
    pub(crate) fn prepare(
        domain: DomainStore,
        capability: RunnerCapability,
        host: SharedHost,
        limits: Limits,
        required: bool,
        reuse_live: bool,
    ) -> Result<(Self, OwnedWork), Failure> {
        Self::prepare_bound(domain, capability, host, limits, required, reuse_live, None)
    }
    pub(crate) fn start_factory_followup(
        domain: DomainStore,
        capability: RunnerCapability,
        host: SharedHost,
        limits: Limits,
        binding: crate::warm::Binding,
    ) -> Result<Self, Failure> {
        let (mut operation, work) =
            Self::prepare_bound(domain, capability, host, limits, true, false, Some(binding))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Failure::Worker)?;
        operation.worker = Some(
            std::thread::Builder::new()
                .name("hagency-owned-dispatch".into())
                .spawn(move || work(&runtime, None))
                .map_err(|_| Failure::Worker)?,
        );
        Ok(operation)
    }
    fn prepare_bound(
        domain: DomainStore,
        capability: RunnerCapability,
        host: SharedHost,
        limits: Limits,
        required: bool,
        reuse_live: bool,
        factory: Option<crate::warm::Binding>,
    ) -> Result<(Self, OwnedWork), Failure> {
        if !limits.validate() {
            return Err(Failure::Admission);
        }
        let started = Instant::now();
        let until = started + Duration::from_millis(limits.operation_ms);
        let (budget_signal, budget) = watch::channel(None);
        let clock = Budget {
            started,
            until,
            budget_ms: limits.operation_ms,
            signal: budget_signal,
        };
        let (live, approval_run, approval_requests) = if let Some(policy) = &host.0.approvals {
            if !policy.fits(limits) {
                return Err(Failure::Admission);
            }
            let live = if reuse_live {
                None
            } else {
                Some(policy.reserve_live()?)
            };
            let (send, receive) = crate::approval::notices();
            (
                live,
                Some(crate::approval::ApprovalRun::new(policy.clone(), send)),
                Some(receive),
            )
        } else {
            (None, None, None)
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let continuation = Arc::new(Continuation {
            binding: Mutex::new(None),
            acknowledged: AtomicBool::new(false),
            cancel: cancel.clone(),
        });
        let original_completion = continuation.clone();
        let signal = cancel.clone();
        let (reply, result) = oneshot::channel();
        let workspace = Arc::new(Mutex::new(None));
        let handoff = workspace.clone();
        let (gate, registration) = Gate::new();
        let work: OwnedWork = Box::new(move |runtime, warm| {
            let mut report = warm.unwrap_or_else(|| Box::new(Report::new(handoff.clone())));
            report.handoff = handoff;
            report.registration = required.then_some(gate);
            if report.live.is_none() {
                report.live = live;
            }
            report.approvals = approval_run;
            report.factory = factory;
            #[cfg(test)]
            if let Some(run) = &mut report.approvals {
                run.set_fault(host.0.approval_fault, host.0.approval_gate.clone());
            }
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if reuse_live
                    && (report.warm.is_none()
                        || report.owner.is_none()
                        || (host.0.approvals.is_some() && report.live.is_none()))
                {
                    return Err(Failure::Worker);
                }
                runtime.block_on(execute(
                    &domain,
                    &capability,
                    host.0,
                    limits,
                    &signal,
                    &clock,
                    &mut report,
                ))
            }))
            .unwrap_or(Err(Failure::Worker));
            if let Some(usage) = &mut report.usage {
                usage.close();
            }
            if let Err(failure) = outcome {
                // execute retains any returned owner; stop before negative domain
                // observation too. This can never authorize lease release.
                // ADOPTION (ADR-053 amendment): a spawn abandoned at the
                // deadline may still complete on its detached thread — the
                // late child arrives through SpawnCustody. Adopt it BEFORE
                // capture/stop and BEFORE the fence is written, so the
                // existing teardown (observe_stop's guardian stop/reap;
                // on a miss, SupervisedProcess::Drop's socket EOF triggers
                // the guardian's process-group kill) is what reaps it. The
                // receive is try-only — bounded by the thread having
                // already signalled — so finalization never waits.
                if report.runtime_observation.is_none()
                    && report.owner.is_none()
                    && let Some(mut late) = report.late_child.take()
                    && let Ok(Ok(session)) = late.try_recv()
                {
                    report.owner = Some(session);
                }
                if report.runtime_observation.is_none()
                    && let Some(owner) = &report.owner
                {
                    report.runtime_observation =
                        Some(RuntimeObservation::capture(report.runtime_stage, owner));
                }
                report.retry_stop();
                // The payload-carrying refusal (ADR-142) forces one owned
                // observation here; project it before the move into `failure`.
                let observation = failure.observation();
                report.failure = Some(failure);
                report.reconciliation =
                    Some((domain.clone(), capability.clone(), observation.clone()));
                report.settlement = match runtime
                    .block_on(domain.observe_owned_failure(capability.clone(), observation))
                {
                    Ok(value) => {
                        report.reconciliation = None;
                        Settlement::Negative(value)
                    }
                    Err(_) => Settlement::Unknown,
                };
                if stopped(report.cleanup)
                    && report.late_child.is_none()
                    && report.settlement == Settlement::Negative(OwnedObservation::Fenced)
                    && let (Some(scope), Some(workspace)) =
                        (&report.stopped_scope, &report.workspace)
                {
                    report.stop_inspection =
                        runtime.block_on(crate::inspection::Inspection::capture(
                            &domain,
                            &capability,
                            scope,
                            workspace,
                        ));
                }
            }
            if let Some(approvals) = &mut report.approvals {
                approvals.finish_notices();
            }
            original_completion.record(&report);
            // If the host dropped its handle, sending returns ownership and its
            // Drop still runs here before this retained worker exits.
            let _ = reply.send(report);
        });
        Ok((
            Self {
                cancel,
                worker: None,
                result,
                workspace,
                registration,
                approvals: approval_requests,
                continuation,
                budget,
            },
            work,
        ))
    }
    pub(crate) fn continuation(&self) -> Arc<Continuation> {
        self.continuation.clone()
    }
    /// The live budget view (ADR-183 decision D), for the host to project
    /// `over_budget` and post the thread notice while the turn still runs.
    pub fn budget_watch(&self) -> BudgetWatch {
        BudgetWatch(self.budget.clone())
    }
    pub(crate) fn adopt_worker(&mut self, worker: JoinHandle<()>) {
        self.worker = Some(worker);
    }
    /// Nonblocking, one-shot handoff. None means not ready, already taken, or
    /// unavailable. A late value remains sealed but refuses retired access.
    pub fn take_workspace_binding(&mut self) -> Option<StartedWorkspace> {
        self.workspace.try_lock().ok()?.take()
    }
    /// One bounded host handoff; unavailable in the ordinary start mode.
    pub fn take_workspace_registration(&mut self) -> Option<WorkspaceRegistration> {
        self.registration.try_lock().ok()?.take()
    }
    pub fn take_approval_requests(&mut self) -> Option<crate::ApprovalRequests> {
        self.approvals.take()
    }
    pub fn is_finished(&self) -> bool {
        self.worker
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub async fn wait(&mut self) -> Result<Report, Failure> {
        self.wait_boxed().await.map(|report| *report)
    }
    /// Keep the single retained result indirect across nested host futures.
    /// This changes storage location only; cancellation and ownership are identical.
    pub async fn wait_boxed(&mut self) -> Result<Box<Report>, Failure> {
        let mut guard = WaitGuard {
            cancel: &self.cancel,
            done: false,
        };
        let result = (&mut self.result).await.map_err(|_| Failure::Worker);
        guard.done = true;
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| Failure::Worker)?;
        }
        if result.is_ok() {
            self.continuation
                .acknowledged
                .store(true, Ordering::Release);
        }
        result
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        self.cancel();
        self.result.close(); // Failed send drops any retained owner on the worker.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
struct WaitGuard<'a> {
    cancel: &'a AtomicBool,
    done: bool,
}
impl Drop for WaitGuard<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.cancel.store(true, Ordering::Release);
        }
    }
}

/// `Failure::Deadline` is the operation budget's own verdict, and since
/// ADR-183 decision D it belongs to the phases before the turn only: store
/// admission and start, the workspace handoff, the spawn handshake (ADR-053
/// amendment), initialize, the task MCP bind, thread/start and turn/start,
/// plus the warm runtime's initialize and idle budgets. Every wait during and
/// after the turn is bounded by `TURN_CEILING_MS` instead, so a `Deadline`
/// there is the runtime's own lifetime, never the budget.
fn checkpoint(cancel: &AtomicBool, until: Instant) -> Result<(), Failure> {
    if cancel.load(Ordering::Acquire) {
        Err(Failure::Cancelled)
    } else if Instant::now() >= until {
        Err(Failure::Deadline)
    } else {
        Ok(())
    }
}
/// The attempt's clock (ADR-183 decision D). `until` is the operation
/// budget: it bounds every wait that is not the turn. Once the turn has
/// started the budget is notify-only: `watch` lets it elapse under the turn,
/// records the fact once and keeps polling the turn until Codex ends it or
/// the host is cancelled.
pub(crate) struct Budget {
    started: Instant,
    until: Instant,
    budget_ms: u64,
    signal: watch::Sender<Option<OverBudget>>,
}
impl Budget {
    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
    /// Drive the turn. If the budget elapses first: the attempt event
    /// `over_budget`, the one thread notice (queued in the store, posted by
    /// the driver's notice delivery), the live signal for the host, a WARN
    /// with the same labels — and then the turn is awaited exactly as before.
    /// The records are best effort and run beside the turn, never instead of
    /// polling it; nothing here stops, cancels or signals the child.
    async fn watch<F: Future>(
        &self,
        future: F,
        domain: &DomainStore,
        cap: &RunnerCapability,
    ) -> (F::Output, Option<OverBudget>) {
        tokio::pin!(future);
        tokio::select! {
            biased;
            result = &mut future => return (result, None),
            _ = tokio::time::sleep_until(self.until) => {}
        }
        let mut over = OverBudget {
            budget_ms: self.budget_ms,
            elapsed_ms: self.elapsed_ms(),
        };
        tracing::warn!(
            dispatch_id = %cap.dispatch_id,
            fence = cap.fence,
            budget_ms = over.budget_ms,
            elapsed_ms = over.elapsed_ms,
            "operation budget elapsed; the turn continues until Codex ends it or a human stops it"
        );
        let _ = self.signal.send(Some(over));
        let record = async {
            let notice = match domain
                .queue_over_budget_notice(cap.dispatch_id.clone(), over.elapsed_ms, now_ms())
                .await
            {
                Ok(outcome) => outcome.as_str(),
                Err(error) => {
                    tracing::warn!(
                        dispatch_id = %cap.dispatch_id,
                        error = ?error,
                        "over-budget notice not queued"
                    );
                    "refused"
                }
            };
            note(
                domain,
                cap,
                hagency_store::AttemptPhase::OverBudget,
                serde_json::json!({
                    "budget_ms": over.budget_ms,
                    "elapsed_ms": over.elapsed_ms,
                    "notice": notice,
                }),
            )
            .await;
        };
        let (result, ()) = tokio::join!(&mut future, record);
        over.elapsed_ms = self.elapsed_ms();
        (result, Some(over))
    }
}
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or_default()
}
/// The runner's own item events projected into the activity vocabulary
/// (TS `codexActivity`, `router/src/runner-activity.ts:4-18`). Only the item
/// types the retained mapping names produce an event; everything else is
/// ignored, exactly as `return null` does there. The dedupe id is the
/// sha256 hex of the item id, so a replayed `item/completed` can never count
/// twice (`store` `dispatch_activity_events`).
pub(crate) fn runner_activity(update: &Update) -> Option<hagency_store::ActivityEvent> {
    let Update::Item { id, kind, phase } = update else {
        return None;
    };
    let kind = match kind.as_str() {
        "commandExecution" => "command",
        "fileChange" => "files",
        "webSearch" => "search",
        "mcpToolCall" | "dynamicToolCall" => "tool",
        "collabAgentToolCall" => "delegate",
        _ => return None,
    };
    let event_id = format!("{:x}", Sha256::digest(id.as_bytes()));
    Some(match phase {
        session::ItemPhase::Active => hagency_store::ActivityEvent::ToolStart {
            kind: kind.into(),
            event_id,
        },
        session::ItemPhase::Complete => hagency_store::ActivityEvent::ToolEnd {
            kind: kind.into(),
            event_id,
        },
    })
}
/// Record one drained update's runner activity. **Both** update-draining paths
/// call this: the plain loop below, and the approval drive that production
/// actually takes. The recording used to live inline in the plain loop only,
/// so with approvals bound (which is every live dispatch, `config.rs:1051`)
/// the loop was never reached and `dispatch_activity_events` stayed empty —
/// the notice's counters read `工具调用 0 次` forever (board #114). Wired here
/// once, no drain path can forget it.
pub(crate) async fn record_runner_activity(
    domain: &DomainStore,
    cap: &RunnerCapability,
    update: &Update,
) {
    let Some(event) = runner_activity(update) else {
        return;
    };
    // Observation-class, exactly like `note()`: a refused write is counted
    // nowhere and never changes the turn.
    if let Err(error) = domain
        .record_activity_event(cap.dispatch_id.clone(), event, now_ms())
        .await
    {
        tracing::warn!(dispatch_id = %cap.dispatch_id, error = ?error,
            "runner activity not recorded");
    }
}
/// One phase of the attempt, kept with it (ADR-181). Best effort: the store
/// records it in its own savepoint, a refused record changes nothing here,
/// and the same fixed labels go to the service log.
pub(crate) async fn note(
    domain: &DomainStore,
    cap: &RunnerCapability,
    phase: hagency_store::AttemptPhase,
    detail: serde_json::Value,
) {
    tracing::info!(
        dispatch_id = %cap.dispatch_id,
        fence = cap.fence,
        phase = phase.as_str(),
        detail = %detail,
        "owned attempt phase"
    );
    let event = hagency_store::AttemptEvent {
        dispatch_id: cap.dispatch_id.clone(),
        fence: cap.fence,
        phase,
        detail,
    };
    if let Err(error) = domain.record_attempt_event(event, now_ms()).await {
        tracing::warn!(
            dispatch_id = %cap.dispatch_id,
            phase = phase.as_str(),
            error = ?error,
            "owned attempt phase not recorded"
        );
    }
}
pub(crate) async fn bounded<F: Future>(
    future: F,
    cancel: &AtomicBool,
    until: Instant,
) -> Result<F::Output, Failure> {
    tokio::pin!(future);
    let mut tick = interval(Duration::from_millis(20));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        checkpoint(cancel, until)?;
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(until) => return Err(Failure::Deadline),
            _ = tick.tick() => {},
            result = &mut future => return Ok(result),
        }
    }
}
async fn watched<F: Future<Output = Result<T, session::Error>>, T>(
    future: F,
    domain: &DomainStore,
    cap: &RunnerCapability,
    expected: &str,
    cancel: &AtomicBool,
    until: Instant,
    status: &mut Option<TaskState>,
) -> Result<T, Failure> {
    let refused = |error| {
        if error == session::Error::UnsupportedRequest {
            Failure::UnsupportedApproval
        } else {
            Failure::Protocol
        }
    };
    watched_with(
        future, refused, domain, cap, expected, cancel, until, status,
    )
    .await
}
/// `watched` for any runtime: the lease is renewed every 100 ms while the
/// runtime future runs, and its own refusal maps through `refused`.
#[allow(clippy::too_many_arguments)]
async fn watched_with<F: Future<Output = Result<T, E>>, T, E>(
    future: F,
    refused: impl Fn(E) -> Failure,
    domain: &DomainStore,
    cap: &RunnerCapability,
    expected: &str,
    cancel: &AtomicBool,
    until: Instant,
    status: &mut Option<TaskState>,
) -> Result<T, Failure> {
    tokio::pin!(future); // Held across checks; a cancelled future is never repolled.
    let mut tick = interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        checkpoint(cancel, until)?;
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(until) => return Err(Failure::Deadline),
            _ = tick.tick() => {
                let current = bounded(domain.renew_owned_dispatch(cap.clone(), expected.into(), 5_000), cancel, until).await?
                    .map_err(|error| Failure::lost(AuthoritySite::LeaseRenew, &error))?;
                *status = Some(current.status);
            },
            result = &mut future => return result.map_err(&refused),
        }
    }
}
pub(crate) struct Deadline {
    pub until: Instant,
    pub expires_at: u64,
}
pub(crate) struct NativeStartup {
    pub launch: hagency_platform::Launch,
    pub settings: hagency_runtime::codex::session::Settings,
    pub io_limits: hagency_runtime::codex::transport::Limits,
}
pub(crate) async fn spawn_prepared(
    host: Arc<Host>,
    startup: NativeStartup,
    limits: Limits,
    cancel: &Arc<AtomicBool>,
    until: Instant,
    report: &mut Report,
) -> Result<(), Failure> {
    let NativeStartup {
        launch,
        settings,
        io_limits,
    } = startup;
    // ADR-053 amendment: the operation budget bounds the spawn itself.
    // The blocking guardian handshake (supervisor/unix.rs spawn_inner:
    // Prepare -> Prepared -> Start -> Started against its own watch)
    // runs on a blocking thread and is awaited through the SAME
    // bounded() that races sleep_until(until) — no new bound, no new
    // literal. The JoinHandle's result is the channel, so the abandoned
    // thread can never leak the session past the operation.
    let (custody, mut late_child) = oneshot::channel();
    let guardian = host.guardian.clone();
    let response_ms = limits.response_ms;
    // Test double only (ADR-053 amendment): delay the whole spawn past the
    // granted budget — the handshake cannot complete within `until`, and a
    // REAL late child still appears afterwards, exercising the custody
    // handoff and the Drop backstop against a live process. The duration
    // is a multiple of the granted operation budget (the same value the
    // host publishes as HAGENCY_OPERATION_BUDGET_MS) — no bare time
    // literal. Unconditional like the Host flag it reads: a cfg(test)
    // consumption is invisible to integration tests, and the selector would
    // race a real, unstalled spawn. No production caller sets the flag.
    let stall = host
        .guardian_prepare_stall
        .then(|| Duration::from_millis(limits.operation_ms.saturating_mul(2)))
        .filter(|stall| {
            // The double must LOSE the race deterministically: a stall within
            // the budget would not exercise the expiry path at all.
            stall.as_millis() > u128::from(limits.operation_ms)
        });
    let spawn = tokio::task::spawn_blocking(move || {
        if let Some(stall) = stall {
            std::thread::sleep(stall);
        }
        // try-send semantics: a gone receiver IS the Drop backstop's
        // trigger — the OwnedSession drops here, stopping the process
        // group through SupervisedProcess::Drop's socket EOF (the
        // guardian's own group kill, supervisor/unix.rs). The send can
        // therefore never block the store writer queue.
        let _ = custody.send(OwnedSession::spawn(
            &guardian,
            &launch,
            settings,
            io_limits,
            response_ms,
        ));
    });
    match bounded(spawn, cancel, until).await {
        // bounded() yields the JoinHandle's own result: Ok(Ok(())) is the
        // thread having finished inside the budget (the channel carries
        // the spawn outcome); a JoinError joins SpawnFailed's existing
        // classification — no child exists to hand over.
        Ok(Ok(())) => {
            // The thread finished before the deadline; its result is in
            // the channel (try_recv cannot be pending once the handle
            // resolved).
            match late_child.try_recv() {
                Ok(Ok(session)) => report.owner = Some(session),
                Ok(Err(error)) => {
                    report.startup_error = Some(error);
                    if let StartError::Uncertain { cleanup, .. } = error {
                        report.cleanup = cleanup;
                        if stopped(cleanup)
                            && let Some(live) = &mut report.live
                        {
                            live.release();
                        }
                    } else if let Some(live) = &mut report.live {
                        live.release();
                    }
                    return Err(Failure::SpawnFailed);
                }
                Err(_) => return Err(Failure::SpawnFailed),
            }
        }
        // A JoinError (the blocking thread panicked) has no child to hand
        // over: SpawnFailed with the release semantics of the non-Uncertain
        // start errors above.
        Ok(Err(_)) => {
            if let Some(live) = &mut report.live {
                live.release();
            }
            return Err(Failure::SpawnFailed);
        }
        // The deadline fired while the handshake was still in flight. A
        // spawn abandoned at the deadline is an UNCERTAIN start —
        // not-started is unprovable through a timed-out handshake (only
        // the pre-fork Settings validation and the platform's explicit
        // Unsupported are provably not-started, and both are already
        // classified above) — so the word is SpawnFailed with the
        // synthesized Cleanup::Unknown{TimedOut} the ADR names, never a
        // plain Deadline. The JoinHandle dropped with the await (the
        // blocking thread detaches by design); any late OwnedSession
        // reaches the teardown through late_child — adopted before
        // capture, or dropped on the thread where the Drop backstop
        // stops and reaps it. The store outcome is exactly today's
        // Uncertain path: fenced, quarantined, dirty workspace.
        Err(Failure::Deadline) => {
            report.cleanup = Cleanup::Unknown {
                kind: std::io::ErrorKind::TimedOut,
            };
            // A possible late child is not observed full cleanup. Keep the
            // original live slot held until an actual stop proves release.
            report.late_child = Some(late_child);
            return Err(Failure::SpawnFailed);
        }
        // Cancelled mid-spawn: same custody handoff for the late child;
        // the cancellation verdict itself is unchanged.
        Err(failure) => {
            report.late_child = Some(late_child);
            return Err(failure);
        }
    }
    #[cfg(test)]
    if host.approval_fault == Some(crate::approval::Fault::SpawnPanic) {
        panic!("actual owned spawn unwind");
    }
    Ok(())
}

/// The Claude counterpart of `spawn_prepared` (ADR-192): the guardian
/// handshake runs on a blocking thread raced against the same deadline
/// (ADR-053). A session that arrives after the deadline is dropped on that
/// thread, where its Drop stops it through the guardian; the report keeps the
/// uncertain cleanup, so the dispatch is fenced as for an abandoned Codex spawn.
async fn spawn_claude(
    host: &Host,
    launch: hagency_platform::Launch,
    limits: Limits,
    cancel: &Arc<AtomicBool>,
    until: Instant,
    report: &mut Report,
) -> Result<(), Failure> {
    let guardian = host.guardian.clone();
    // ADR-183 decision D, as for the Codex transport: the session's event wait
    // and lifetime are the runtime ceiling, never the budget.
    let session_limits = hagency_runtime::claude::session::Limits {
        write_timeout_ms: limits.response_ms,
        event_wait_ms: TURN_CEILING_MS,
        lifetime_ms: TURN_CEILING_MS,
    };
    let spawn = tokio::task::spawn_blocking(move || {
        OwnedClaudeSession::spawn(&guardian, &launch, session_limits)
    });
    match bounded(spawn, cancel, until).await {
        Ok(Ok(Ok(session))) => {
            report.claude = Some(session);
            Ok(())
        }
        Ok(Ok(Err(error))) => {
            report.startup_error = Some(error);
            if let StartError::Uncertain { cleanup, .. } = error {
                report.cleanup = cleanup;
                if stopped(cleanup)
                    && let Some(live) = &mut report.live
                {
                    live.release();
                }
            } else if let Some(live) = &mut report.live {
                live.release();
            }
            Err(Failure::SpawnFailed)
        }
        Ok(Err(_)) => {
            if let Some(live) = &mut report.live {
                live.release();
            }
            Err(Failure::SpawnFailed)
        }
        Err(Failure::Deadline) => {
            report.cleanup = Cleanup::Unknown {
                kind: std::io::ErrorKind::TimedOut,
            };
            Err(Failure::SpawnFailed)
        }
        Err(failure) => Err(failure),
    }
}
/// The Octos counterpart of `spawn_claude` (ADR-193), under the same
/// deadline and abandoned-spawn rules.
async fn spawn_octos(
    host: &Host,
    launch: hagency_platform::Launch,
    limits: Limits,
    cancel: &Arc<AtomicBool>,
    until: Instant,
    report: &mut Report,
) -> Result<(), Failure> {
    let guardian = host.guardian.clone();
    // ADR-183 decision D: the session's event wait and lifetime are the
    // runtime ceiling, never the budget.
    let session_limits = hagency_runtime::octos::session::Limits {
        write_timeout_ms: limits.response_ms,
        event_wait_ms: TURN_CEILING_MS,
        lifetime_ms: TURN_CEILING_MS,
    };
    let spawn = tokio::task::spawn_blocking(move || {
        OwnedOctosSession::spawn(&guardian, &launch, session_limits)
    });
    match bounded(spawn, cancel, until).await {
        Ok(Ok(Ok(session))) => {
            report.octos = Some(Box::new(session));
            Ok(())
        }
        Ok(Ok(Err(error))) => {
            report.startup_error = Some(error);
            if let StartError::Uncertain { cleanup, .. } = error {
                report.cleanup = cleanup;
                if stopped(cleanup)
                    && let Some(live) = &mut report.live
                {
                    live.release();
                }
            } else if let Some(live) = &mut report.live {
                live.release();
            }
            Err(Failure::SpawnFailed)
        }
        Ok(Err(_)) => {
            if let Some(live) = &mut report.live {
                live.release();
            }
            Err(Failure::SpawnFailed)
        }
        Err(Failure::Deadline) => {
            report.cleanup = Cleanup::Unknown {
                kind: std::io::ErrorKind::TimedOut,
            };
            Err(Failure::SpawnFailed)
        }
        Err(failure) => Err(failure),
    }
}
/// What one Octos dispatch borrows from `execute` (ADR-193).
struct OctosTurn<'a> {
    domain: &'a DomainStore,
    cap: &'a RunnerCapability,
    host: &'a Arc<Host>,
    limits: Limits,
    budget: &'a Budget,
    expected: &'a str,
    cancel: &'a Arc<AtomicBool>,
    until: Instant,
    ceiling: Instant,
    /// The approval context's workspace facts, when the host has approvals.
    approval: Option<TurnApproval>,
}
/// The prepared session of one Octos dispatch (ADR-193).
struct OctosSession {
    prompt: String,
    profile: String,
    session: String,
    turn: String,
    workspace: String,
    tools: crate::octos_tools::Spec,
}
/// ADR-193 decision 5: answer one Octos host tool call through the dispatch's
/// task helper, then give Octos the result. The lease is renewed meanwhile,
/// as it is while Codex's own helper runs. A tool's failure is the model's to
/// read; only a broken session refuses the dispatch.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn answer_octos_tool(
    tools: &mut crate::octos_tools::HostTools,
    session: &mut OwnedOctosSession,
    params: &serde_json::Value,
    domain: &DomainStore,
    cap: &RunnerCapability,
    expected: &str,
    cancel: &AtomicBool,
    until: Instant,
    status: &mut Option<TaskState>,
) -> Result<(), Failure> {
    let call = session
        .host_tool_call(params)
        .map_err(|_| Failure::Protocol)?;
    let result = watched_with(
        async { Ok::<_, ()>(tools.call(&call.name, call.args).await) },
        |_| Failure::Protocol,
        domain,
        cap,
        expected,
        cancel,
        until,
        status,
    )
    .await?;
    watched_with(
        session.host_tool_result(&call.call_id, result),
        |_| Failure::Protocol,
        domain,
        cap,
        expected,
        cancel,
        until,
        status,
    )
    .await?;
    Ok(())
}
/// One Octos dispatch on a fresh guardian-owned `octos serve --stdio`
/// (ADR-193), between the shared admission in `execute` and the shared
/// `settle`: hello, the fixed permission profile, a fresh session on the
/// workspace and one turn, then every event until Octos reports the session
/// idle, its sub-agents' work and continuation turns included. The reply is
/// the last turn's. With approvals, the accepted turn binds the approval
/// context and the Octos coordinator drives the rest, so every approval
/// becomes an owner card (decision 4). Without them an approval refuses the
/// dispatch, as for Codex. The child is stopped before this returns.
async fn run_octos_turn(
    turn: OctosTurn<'_>,
    launch: hagency_platform::Launch,
    prepared: OctosSession,
    report: &mut Report,
) -> Result<(), Failure> {
    use hagency_runtime::octos::session::{Event, Permissions};
    let OctosTurn {
        domain,
        cap,
        host,
        limits,
        budget,
        expected,
        cancel,
        until,
        ceiling,
        mut approval,
    } = turn;
    // The task helper first: without its tools the dispatch does not start.
    let (mut tools, registered) = bounded(
        crate::octos_tools::HostTools::start(&prepared.tools),
        cancel,
        until,
    )
    .await?
    .map_err(|_| Failure::Admission)?;
    note(
        domain,
        cap,
        hagency_store::AttemptPhase::SpawnStarted,
        serde_json::json!({}),
    )
    .await;
    spawn_octos(host, launch, limits, cancel, until, report).await?;
    let session = report.octos.as_mut().ok_or(Failure::SpawnFailed)?;
    note(
        domain,
        cap,
        hagency_store::AttemptPhase::SpawnDone,
        serde_json::json!({"pid": session.id(), "warm": false}),
    )
    .await;
    let refused = |_: hagency_runtime::octos::session::Error| Failure::Protocol;
    let local = report.local_codex.clone();
    let mut outcome = crate::approval::octos::OctosOutcome::default();
    let status = &mut report.canonical_status;
    let usage = report.usage.as_mut().ok_or(Failure::UsageBinding)?;
    let mut approvals = report.approvals.as_mut();
    let settlement_cause = &mut report.settlement_cause;
    let drive = async {
        watched_with(
            session.hello(),
            refused,
            domain,
            cap,
            expected,
            cancel,
            until,
            status,
        )
        .await?;
        note(
            domain,
            cap,
            hagency_store::AttemptPhase::Initialized,
            serde_json::json!({}),
        )
        .await;
        // Every owned dispatch holds its exclusive workspace lease.
        watched_with(
            session.open(
                &prepared.session,
                &prepared.profile,
                &prepared.workspace,
                Permissions::WorkspaceWrite,
            ),
            refused,
            domain,
            cap,
            expected,
            cancel,
            until,
            status,
        )
        .await?;
        watched_with(
            session.register_tools(&registered),
            refused,
            domain,
            cap,
            expected,
            cancel,
            until,
            status,
        )
        .await?;
        watched_with(
            session.start_turn(&prepared.turn, &prepared.prompt),
            refused,
            domain,
            cap,
            expected,
            cancel,
            ceiling,
            status,
        )
        .await?;
        // The usage source exists once the dispatch's turn is accepted.
        usage.attach(&**session);
        note(
            domain,
            cap,
            hagency_store::AttemptPhase::TurnStarted,
            serde_json::json!({"session_id": session.session_id(), "turn_id": session.dispatch_turn()}),
        )
        .await;
        if let (Some(run), Some(approval)) = (approvals.take(), approval.take()) {
            // The session is the thread and the dispatch's turn the turn, as
            // Codex names its thread and turn; a continuation turn's own ID
            // rides on each request (`octosTurnId`).
            let thread =
                crate::approval::octos::opaque(session.session_id().ok_or(Failure::Protocol)?);
            let turn =
                crate::approval::octos::opaque(session.dispatch_turn().ok_or(Failure::Protocol)?);
            let connection = hagency_core::canonical::digest(&serde_json::json!([
                cap,
                session.id(),
                thread,
                turn
            ]))
            .map_err(|_| Failure::Admission)?;
            let context = hagency_core::approvals::HostApprovalContext {
                id: format!("owned_{connection}"),
                connection_id: connection,
                thread_id: thread,
                turn_id: turn,
                workspace_resource: approval.resource,
                workspace: approval.workspace,
                windows_paths: cfg!(windows),
                environment_id: None,
                may_write: approval.may_write,
                yolo: false,
            };
            run.bind_octos(
                domain,
                cap,
                expected,
                context,
                Deadline {
                    until: ceiling,
                    expires_at: approval.expires_at,
                },
                session,
            )
            .await?;
            return run
                .drive_octos(
                    crate::approval::octos::OctosDrive {
                        domain,
                        cap,
                        expected,
                        tools: &mut tools,
                        cancel,
                        until: ceiling,
                        status,
                        usage,
                        outcome: &mut outcome,
                        settlement_cause,
                    },
                    session,
                )
                .await;
        }
        loop {
            let event = watched_with(
                session.next(),
                refused,
                domain,
                cap,
                expected,
                cancel,
                ceiling,
                status,
            )
            .await?;
            // Usage first: each terminal's, then the session's at idle.
            let idle = outcome
                .observe(session, usage, &event, cancel, ceiling)
                .await?;
            match event {
                Event::Approval { .. } => return Err(Failure::UnsupportedApproval),
                Event::ToolCall { params } => {
                    answer_octos_tool(
                        &mut tools, session, &params, domain, cap, expected, cancel, ceiling,
                        status,
                    )
                    .await?;
                    if idle {
                        return Ok(());
                    }
                }
                _ if idle => return Ok(()),
                _ => {}
            }
        }
    };
    // The user's Octos home is checked through the turn, as a sign-in folder
    // is for Codex and Claude Code.
    let drive = async move {
        match local {
            Some(local) => local.watch(drive).await,
            None => drive.await,
        }
    };
    // ADR-183 decision D: the budget only notifies under the turn.
    let (drive, over_budget) = budget.watch(drive, domain, cap).await;
    report.over_budget = over_budget;
    report.protocol = match outcome.text {
        Some(text) => {
            report.text = Some(text);
            Protocol::Completed
        }
        None if outcome.ended => Protocol::Failed,
        None => Protocol::Unknown,
    };
    // A drive that ended in a settlement verdict outranks Octos's own idle,
    // as for Codex (ADR-046): no `SettlementUnknown` beside a completion.
    if matches!(drive, Err(Failure::SettlementUnknown)) && report.protocol == Protocol::Completed {
        report.protocol = Protocol::Unknown;
    }
    note(
        domain,
        cap,
        hagency_store::AttemptPhase::StopRequested,
        serde_json::json!({}),
    )
    .await;
    let session = report.octos.as_mut().ok_or(Failure::Worker)?;
    report.cleanup = session.stop();
    report.exit_identity = session.exit_identity();
    report.stderr_tail = session.stderr_tail(512);
    report.guardian_stderr_tail = session.guardian_stderr_tail();
    report.turn_failure = outcome.failure;
    drive
}
/// What one Claude turn borrows from `execute` (ADR-192).
struct ClaudeTurn<'a> {
    domain: &'a DomainStore,
    cap: &'a RunnerCapability,
    host: &'a Arc<Host>,
    limits: Limits,
    budget: &'a Budget,
    started: &'a hagency_store::OwnedDispatchScope,
    expected: &'a str,
    cancel: &'a Arc<AtomicBool>,
    until: Instant,
    ceiling: Instant,
    /// The approval context's workspace facts, when the host has approvals.
    approval: Option<TurnApproval>,
}
/// The approval context's workspace facts of a Claude or Octos turn.
struct TurnApproval {
    workspace: String,
    resource: String,
    may_write: bool,
    expires_at: u64,
}
/// One Claude turn on a fresh guardian-owned session (ADR-192), between the
/// shared admission in `execute` and the shared `settle`: initialize, bind the
/// scoped helper (ADR-158), one prompt, then read to its result (ADR-155).
/// With approvals, `system/init` binds the approval context and the Claude
/// coordinator drives the rest of the turn, so every permission request
/// becomes an owner card (decision 4). Without them a permission request
/// refuses the turn, as for Codex. The child is stopped before this returns;
/// the returned value is the drive's outcome for `settle`.
async fn run_claude_turn(
    turn: ClaudeTurn<'_>,
    launch: hagency_platform::Launch,
    prompt: String,
    helper: Option<hagency_runtime::claude::TaskMcp>,
    retained: bool,
    report: &mut Report,
) -> Result<(), Failure> {
    let ClaudeTurn {
        domain,
        cap,
        host,
        limits,
        budget,
        started,
        expected,
        cancel,
        until,
        ceiling,
        mut approval,
    } = turn;
    note(
        domain,
        cap,
        hagency_store::AttemptPhase::SpawnStarted,
        serde_json::json!({}),
    )
    .await;
    spawn_claude(host, launch, limits, cancel, until, report).await?;
    let session = report.claude.as_mut().ok_or(Failure::SpawnFailed)?;
    note(
        domain,
        cap,
        hagency_store::AttemptPhase::SpawnDone,
        serde_json::json!({"pid": session.id(), "warm": false}),
    )
    .await;
    let refused = |_: hagency_runtime::claude::session::Error| Failure::Protocol;
    let mut outcome = crate::approval::claude::ClaudeOutcome::default();
    let local = report.local_codex.clone();
    let status = &mut report.canonical_status;
    let usage = report.usage.as_mut().ok_or(Failure::UsageBinding)?;
    let mut approvals = report.approvals.as_mut();
    let settlement_cause = &mut report.settlement_cause;
    let drive = async {
        watched_with(
            session.initialize(),
            refused,
            domain,
            cap,
            expected,
            cancel,
            until,
            status,
        )
        .await?;
        note(
            domain,
            cap,
            hagency_store::AttemptPhase::Initialized,
            serde_json::json!({}),
        )
        .await;
        if let Some(helper) = helper {
            if retained {
                let context = host.task_context.as_ref().ok_or(Failure::Admission)?;
                // The retained context points the helper at this dispatch before
                // it connects, as for a late-bound Codex helper.
                let binding = context
                    .bind(
                        domain.clone(),
                        cap.clone(),
                        started.clone(),
                        until.into_std(),
                        cancel.clone(),
                    )
                    .await;
                checkpoint(cancel, until)?;
                binding.map_err(|error| match error {
                    hagency_store::Error::RunnerAuthority | hagency_store::Error::Quarantined => {
                        Failure::lost(AuthoritySite::TaskMcpBind, &error)
                    }
                    _ => Failure::Admission,
                })?;
            }
            watched_with(
                session.bind_task_mcp(helper),
                refused,
                domain,
                cap,
                expected,
                cancel,
                until,
                status,
            )
            .await?;
        }
        watched_with(
            session.prompt(&prompt),
            refused,
            domain,
            cap,
            expected,
            cancel,
            ceiling,
            status,
        )
        .await?;
        loop {
            let message = watched_with(
                session.next_message(),
                refused,
                domain,
                cap,
                expected,
                cancel,
                ceiling,
                status,
            )
            .await?;
            match &message {
                // The usage source exists from `system/init` on (ADR-157).
                Message::Event { kind, payload, .. }
                    if *kind == EventKind::System && payload["subtype"] == "init" =>
                {
                    usage.attach(&*session);
                    note(
                        domain,
                        cap,
                        hagency_store::AttemptPhase::TurnStarted,
                        serde_json::json!({"session_id": session.session_id()}),
                    )
                    .await;
                    let (Some(run), Some(approval)) = (approvals.take(), approval.take()) else {
                        continue;
                    };
                    // The session is the thread and the one prompt the turn,
                    // as Codex names its thread and turn (ADR-192).
                    let thread = crate::approval::claude::opaque(
                        session.session_id().ok_or(Failure::Protocol)?,
                    );
                    let turn = crate::approval::claude::TURN;
                    let connection = hagency_core::canonical::digest(&serde_json::json!([
                        cap,
                        session.id(),
                        thread,
                        turn
                    ]))
                    .map_err(|_| Failure::Admission)?;
                    let context = hagency_core::approvals::HostApprovalContext {
                        id: format!("owned_{connection}"),
                        connection_id: connection,
                        thread_id: thread,
                        turn_id: turn.into(),
                        workspace_resource: approval.resource,
                        workspace: approval.workspace,
                        windows_paths: cfg!(windows),
                        environment_id: None,
                        may_write: approval.may_write,
                        yolo: false,
                    };
                    run.bind_claude(
                        domain,
                        cap,
                        expected,
                        context,
                        Deadline {
                            until: ceiling,
                            expires_at: approval.expires_at,
                        },
                        session,
                    )
                    .await?;
                    return run
                        .drive_claude(
                            crate::approval::claude::ClaudeDrive {
                                domain,
                                cap,
                                cancel,
                                until: ceiling,
                                status,
                                usage,
                                outcome: &mut outcome,
                                settlement_cause,
                            },
                            session,
                        )
                        .await;
                }
                Message::Event { .. } => {
                    if outcome
                        .observe(session, usage, &message, cancel, ceiling)
                        .await?
                    {
                        return Ok(());
                    }
                }
                Message::Permission { .. } => return Err(Failure::UnsupportedApproval),
                Message::ControlCancel { .. } => {}
                Message::ControlResponse { .. } => return Err(Failure::Protocol),
            }
        }
    };
    let drive = async move {
        match local {
            Some(local) => local.watch(drive).await,
            None => drive.await,
        }
    };
    // ADR-183 decision D: the budget only notifies under the turn.
    let (drive, over_budget) = budget.watch(drive, domain, cap).await;
    report.over_budget = over_budget;
    report.protocol = match outcome.text {
        Some(text) => {
            report.text = Some(text);
            Protocol::Completed
        }
        None if outcome.ended => Protocol::Failed,
        None => Protocol::Unknown,
    };
    // A drive that ended in a settlement verdict outranks Claude's own result,
    // as for Codex (ADR-046): no `SettlementUnknown` beside a completion.
    if matches!(drive, Err(Failure::SettlementUnknown)) && report.protocol == Protocol::Completed {
        report.protocol = Protocol::Unknown;
    }
    note(
        domain,
        cap,
        hagency_store::AttemptPhase::StopRequested,
        serde_json::json!({}),
    )
    .await;
    let session = report.claude.as_mut().ok_or(Failure::Worker)?;
    report.cleanup = session.stop();
    report.exit_identity = session.exit_identity();
    report.stderr_tail = session.stderr_tail(512);
    report.guardian_stderr_tail = session.guardian_stderr_tail();
    report.turn_failure = outcome.failure;
    drive
}
async fn execute(
    domain: &DomainStore,
    cap: &RunnerCapability,
    host: Arc<Host>,
    limits: Limits,
    cancel: &Arc<AtomicBool>,
    budget: &Budget,
    report: &mut Report,
) -> Result<(), Failure> {
    // The budget bounds everything before the turn (see `checkpoint`). The
    // approval scope's own clock (`bind_owned_approval_context` checks it on
    // every maintenance call) follows the turn instead: an approval Codex
    // raises after the budget is admitted like any other.
    let until = budget.until;
    let scope = bounded(domain.owned_dispatch_scope(cap.clone()), cancel, until)
        .await?
        .map_err(|_| Failure::Admission)?;
    let expected = scope.fingerprint().to_owned();
    // MA-S2 (ADR-053 amendment): the Host admission re-check. Re-read the
    // bound account's readiness with the store's own predicate before any
    // workspace or process work — a fact that settled between selection and
    // admission parks the row with the named reason (committed, it is the
    // audit record) and refuses here. Neither the selector nor this admission
    // trusts the other's cache.
    bounded(domain.admit_owned_dispatch(cap.clone()), cancel, until)
        .await?
        .map_err(|_| Failure::Admission)?;
    if let Some(binding) = report.factory.as_ref().or(report.warm.as_ref()) {
        binding.check(domain, &scope, cancel, until).await?;
    }
    let crate::host::Prepared {
        launch,
        io_limits,
        root,
        account,
        runner,
    } = if report.factory.is_some() {
        host.prepare_followup(&scope, cap, limits)?
    } else {
        host.prepare(&scope, cap, limits)?
    };
    report.account = account;
    report.local_codex = host.local_codex.clone();
    checkpoint(cancel, until)?;
    let start_reply = bounded(
        domain.start_owned_dispatch(cap.clone(), expected.clone()),
        cancel,
        until,
    )
    .await?;
    // Delivery-loss test double only. DomainStore has actually committed the
    // start; production contains no way to manufacture or recover a lost reply.
    #[cfg(test)]
    let start_reply = if host.discard_start_reply && start_reply.is_ok() {
        Err(hagency_store::Error::OutcomeUnknown)
    } else {
        start_reply
    };
    let started = start_reply.map_err(|error| {
        if matches!(
            error,
            hagency_store::Error::OutcomeUnknown | hagency_store::Error::Unavailable
        ) {
            Failure::StartUnknown
        } else {
            Failure::Admission
        }
    })?;
    let approval_workspace = if host.approvals.is_some() {
        Some(root.approval_path()?)
    } else {
        None
    };
    #[cfg(feature = "test-diagnostics")]
    let workspace_path = root.path().to_owned();
    let workspace = Binding::start(root, domain.clone(), cap, &started, cancel.clone())?;
    let _retire_workspace = workspace.retirement(); // all returns and unwinds
    report.workspace = Some(workspace.clone()); // before any child can exist
    report.stopped_scope = Some(started.clone());
    let required = report.registration.is_some();
    if let Some(gate) = report.registration.take() {
        let acknowledged = gate.publish(workspace.handoff())?;
        bounded(acknowledged, cancel, until)
            .await?
            .map_err(|_| Failure::Admission)?;
    } else {
        *report.handoff.lock().map_err(|_| Failure::Worker)? = Some(workspace.handoff());
    }
    #[cfg(test)]
    if host.panic_after_workspace {
        panic!("offline post-Started workspace unwind");
    }
    report.canonical_status = Some(started.task().status);
    checkpoint(cancel, until)?;
    let binding = bounded(
        UsageRun::bind(domain.clone(), cap.clone(), started.clone()),
        cancel,
        until,
    )
    .await?;
    // Test build only: discard an actual acknowledged source binding, never
    // manufacture a successful write or reopen execution after receipt loss.
    #[cfg(test)]
    let binding = if host.discard_usage_binding_reply && binding.is_ok() {
        Err(hagency_store::Error::OutcomeUnknown)
    } else {
        binding
    };
    report.usage = Some(binding.map_err(|_| Failure::UsageBinding)?);
    checkpoint(cancel, until)?;
    if required {
        bounded(
            domain.check_owned_dispatch(cap.clone(), expected.clone()),
            cancel,
            until,
        )
        .await?
        .map_err(|error| Failure::lost(AuthoritySite::DispatchCheck, &error))?;
        checkpoint(cancel, until)?;
    }
    workspace.check_root().map_err(|_| Failure::Admission)?;
    if let Some(account) = &report.account {
        account
            .check()
            .map_err(|error| Failure::lost(AuthoritySite::AccountCheck, &error))?;
    }
    if let Some(local) = &report.local_codex {
        local.check()?;
    }
    checkpoint(cancel, until)?;
    let approval_may_write = match &runner {
        crate::host::PreparedRunner::Codex { settings, .. } => !settings.is_read_only(),
        // Every owned dispatch holds its exclusive workspace lease (ADR-192).
        crate::host::PreparedRunner::Claude { .. } | crate::host::PreparedRunner::Octos { .. } => {
            true
        }
    };
    if let Some(live) = &mut report.live {
        live.possible();
    }
    // ADR-183 decision D: the transport's lifetime and its wait for the next
    // unsolicited event are the runtime's ceiling, not the budget. The host
    // prepared both as `operation_ms` ("the original operation deadline
    // still bound every quiet turn"); the transport refuses every read past
    // the lifetime and ends a quiet turn at the event wait — the budget by
    // another name, so both follow the turn now. The write timeout stays the
    // RPC response bound (`response_ms`): a request the child does not
    // accept is a transport fact, not the budget. `ceiling` is the same
    // instant on this side, the bound of every wait from the turn on;
    // `ceiling_expires_at` is its wall-clock twin for the approval scope.
    let io_limits = hagency_runtime::codex::transport::Limits {
        event_wait_ms: TURN_CEILING_MS,
        lifetime_ms: TURN_CEILING_MS,
        ..io_limits
    };
    let ceiling = Instant::now() + Duration::from_millis(TURN_CEILING_MS);
    let ceiling_expires_at = crate::approval::state::wall_now()?
        .checked_add(TURN_CEILING_MS)
        .ok_or(Failure::Deadline)?;
    let turn_approval = match (&runner, &approval_workspace) {
        (crate::host::PreparedRunner::Codex { .. }, _) | (_, None) => None,
        (_, Some(workspace)) => Some(TurnApproval {
            workspace: workspace.clone(),
            resource: scope
                .input()
                .resources
                .first()
                .ok_or(Failure::Admission)?
                .id
                .clone(),
            may_write: approval_may_write,
            expires_at: ceiling_expires_at,
        }),
    };
    let (settings, input, late_helper) = match runner {
        crate::host::PreparedRunner::Codex {
            settings,
            input,
            late_helper,
        } => (settings, input, late_helper),
        crate::host::PreparedRunner::Claude {
            prompt,
            helper,
            retained,
        } => {
            let drive = run_claude_turn(
                ClaudeTurn {
                    domain,
                    cap,
                    host: &host,
                    limits,
                    budget,
                    started: &started,
                    expected: &expected,
                    cancel,
                    until,
                    ceiling,
                    approval: turn_approval,
                },
                launch,
                prompt,
                helper,
                retained,
                report,
            )
            .await;
            return settle(
                domain, cap, &host, report, drive, &scope, started, expected, cancel, ceiling,
            )
            .await;
        }
        crate::host::PreparedRunner::Octos {
            prompt,
            profile,
            session,
            turn,
            workspace,
            tools,
        } => {
            let drive = run_octos_turn(
                OctosTurn {
                    domain,
                    cap,
                    host: &host,
                    limits,
                    budget,
                    expected: &expected,
                    cancel,
                    until,
                    ceiling,
                    approval: turn_approval,
                },
                launch,
                OctosSession {
                    prompt,
                    profile,
                    session,
                    turn,
                    workspace,
                    tools,
                },
                report,
            )
            .await;
            return settle(
                domain, cap, &host, report, drive, &scope, started, expected, cancel, ceiling,
            )
            .await;
        }
    };
    if report.owner.is_none() {
        note(
            domain,
            cap,
            hagency_store::AttemptPhase::SpawnStarted,
            serde_json::json!({}),
        )
        .await;
        spawn_prepared(
            host.clone(),
            NativeStartup {
                launch,
                settings,
                io_limits,
            },
            limits,
            cancel,
            until,
            report,
        )
        .await?;
    } else {
        report
            .owner
            .as_mut()
            .ok_or(Failure::SpawnFailed)?
            .consume_warm_idle(io_limits, limits.response_ms, ceiling)
            .map_err(|_| Failure::Protocol)?;
    }
    // The actual child owner is retained before initialize or any startup await.
    let runner = report.owner.as_mut().ok_or(Failure::SpawnFailed)?;
    note(
        domain,
        cap,
        hagency_store::AttemptPhase::SpawnDone,
        serde_json::json!({"pid": runner.id(), "warm": report.warm.is_some()}),
    )
    .await;
    let runtime_stage = &mut report.runtime_stage;
    let initialized = report.warm.is_some();
    let local_codex = report.local_codex.clone();
    let drive = async {
        if !initialized {
            watched(
                runner.initialize(),
                domain,
                cap,
                &expected,
                cancel,
                until,
                &mut report.canonical_status,
            )
            .await?;
        }
        note(
            domain,
            cap,
            hagency_store::AttemptPhase::Initialized,
            serde_json::json!({}),
        )
        .await;
        if let Some(helper) = late_helper {
            let context = host.task_context.as_ref().ok_or(Failure::Admission)?;
            // Await the original retained physical job through return. Do not
            // abandon its IO at a wrapper timeout or create a second launcher.
            let binding = context
                .bind(
                    domain.clone(),
                    cap.clone(),
                    started.clone(),
                    until.into_std(),
                    cancel.clone(),
                )
                .await;
            checkpoint(cancel, until)?;
            binding.map_err(|error| match error {
                hagency_store::Error::RunnerAuthority | hagency_store::Error::Quarantined => {
                    Failure::lost(AuthoritySite::TaskMcpBind, &error)
                }
                _ => Failure::Admission,
            })?;
            runner
                .bind_task_mcp(helper)
                .map_err(|_| Failure::Protocol)?;
        }
        *runtime_stage = RuntimeStage::ThreadStart;
        let thread_id = watched(
            runner.start_thread(),
            domain,
            cap,
            &expected,
            cancel,
            until,
            &mut report.canonical_status,
        )
        .await?;
        *runtime_stage = RuntimeStage::TurnStart;
        let turn_id = watched(
            runner.start_turn(input),
            domain,
            cap,
            &expected,
            cancel,
            until,
            &mut report.canonical_status,
        )
        .await?;
        note(
            domain,
            cap,
            hagency_store::AttemptPhase::TurnStarted,
            serde_json::json!({"thread_id": thread_id, "turn_id": turn_id}),
        )
        .await;
        let usage = report.usage.as_mut().ok_or(Failure::UsageBinding)?;
        usage.attach(runner);
        if let Some(approvals) = &mut report.approvals {
            let connection = hagency_core::canonical::digest(&serde_json::json!([
                cap,
                runner.id(),
                thread_id,
                turn_id
            ]))
            .map_err(|_| Failure::Admission)?;
            let context = hagency_core::approvals::HostApprovalContext {
                id: format!("owned_{connection}"),
                connection_id: connection,
                thread_id,
                turn_id,
                workspace_resource: scope
                    .input()
                    .resources
                    .first()
                    .ok_or(Failure::Admission)?
                    .id
                    .clone(),
                workspace: approval_workspace.ok_or(Failure::Admission)?,
                windows_paths: cfg!(windows),
                environment_id: None,
                may_write: approval_may_write,
                yolo: false,
            };
            approvals
                .bind(
                    domain,
                    cap,
                    &expected,
                    context,
                    Deadline {
                        until: ceiling,
                        expires_at: ceiling_expires_at,
                    },
                    runner,
                )
                .await?;
            *runtime_stage = RuntimeStage::Update;
            return approvals
                .drive(
                    crate::approval::Drive {
                        domain,
                        cap,
                        cancel,
                        until: ceiling,
                        status: &mut report.canonical_status,
                        usage,
                        observation: &mut report.runtime_observation,
                        settlement_cause: &mut report.settlement_cause,
                    },
                    runner,
                )
                .await;
        }
        loop {
            *runtime_stage = RuntimeStage::Update;
            let (update, observation) = watched(
                runner.next_observed_update(),
                domain,
                cap,
                &expected,
                cancel,
                ceiling,
                &mut report.canonical_status,
            )
            .await?;
            if usage.observe(&observation) {
                // Storage refusal closes capture only. Cancellation/deadline
                // still reaches the existing retained process cleanup path.
                let _ = bounded(usage.record_pending(), cancel, ceiling).await?;
            }
            // The runner's own tool activity (TS `recordRunnerActivity`,
            // `runner.ts:742-746`). The approval drive records the same events
            // through the same helper — see `record_runner_activity`.
            record_runner_activity(domain, cap, &update).await;
            match update {
                Update::TurnEnded => break,
                Update::Approval(_) | Update::ApprovalResolved { .. } => {
                    return Err(Failure::UnsupportedApproval);
                }
                _ => {}
            }
        }
        Ok(())
    };
    let drive = async move {
        match local_codex {
            Some(local) => local.watch(drive).await,
            None => drive.await,
        }
    };
    // ADR-183 decision D: the budget may elapse under the turn; it is
    // recorded and notified, and the turn is awaited until Codex ends it or
    // the host is cancelled. Nothing below this line reads the budget.
    let (drive, over_budget) = budget.watch(drive, domain, cap).await;
    report.over_budget = over_budget;
    // Capture before coordinator stop/removal. The runtime's own failure guard
    // may already have stopped it; its first transport cause remains retained.
    // Observation cannot alter the original drive or cleanup result.
    report
        .runtime_observation
        .get_or_insert_with(|| RuntimeObservation::capture(*runtime_stage, runner));
    report.protocol = match runner.protocol_outcome() {
        Some(Outcome::Completed { text }) => {
            report.text = Some(text.clone());
            Protocol::Completed
        }
        Some(Outcome::Failed) => Protocol::Failed,
        Some(Outcome::Interrupted) => Protocol::Interrupted,
        Some(Outcome::UnsupportedRequest) => Protocol::Unsupported,
        _ => Protocol::Unknown,
    };
    // ADR-046 precedence, report-side: a drive that ended in a settlement
    // verdict outranks the peer's own turn outcome — the captured completion
    // is demoted so no `SettlementUnknown` is ever delivered beside a
    // `Completed` protocol (the midwrite scenario: a written, receipt-less
    // frame at a turn end the peer did complete). The drive's verdict itself
    // still surfaces unchanged through `drive?` below.
    if matches!(drive, Err(Failure::SettlementUnknown)) && report.protocol == Protocol::Completed {
        report.protocol = Protocol::Unknown;
    }
    note(
        domain,
        cap,
        hagency_store::AttemptPhase::StopRequested,
        serde_json::json!({}),
    )
    .await;
    // Offline pin for ADR-182 only: a stop the guardian could not prove. The
    // one way to make the real guardian unsure is a tree that outlives
    // SIGKILL for its whole budget, which no offline fixture can do soundly;
    // the marker stands in for it, on a diagnostics build only, and it pins
    // the session's verdict so the host's own stop retry reads the same.
    #[cfg(feature = "test-diagnostics")]
    if workspace_path.join("owned-mcp.unproven-stop").is_file() {
        runner.unprove_stop();
    }
    report.cleanup = runner.stop();
    // The guardian's verdict, the leader's exit and both stderr tails are
    // kept with the attempt before any verdict is drawn from them (ADR-181).
    report.exit_identity = runner.exit_identity();
    report.stderr_tail = runner.stderr_tail(512);
    report.guardian_stderr_tail = runner.guardian_stderr_tail();
    // The provider's own reason for ending the turn (board #110), bounded to
    // the same 512 bytes as the stderr tail it sits beside. The real app server
    // names a usage-limit refusal here; native used to drop it, so the operator
    // saw only a bare `protocol` fault.
    report.turn_failure = runner.turn_failure_tail(512);
    settle(
        domain, cap, &host, report, drive, &scope, started, expected, cancel, ceiling,
    )
    .await
}
/// The settlement both runners share once their child is stopped: the stop
/// record, approval and live release, then held completion custody or the
/// runtime's own reply (ADR-192: Codex and Claude settle alike).
#[allow(clippy::too_many_arguments)]
async fn settle(
    domain: &DomainStore,
    cap: &RunnerCapability,
    host: &Host,
    report: &mut Report,
    drive: Result<(), Failure>,
    scope: &hagency_store::OwnedDispatchScope,
    started: hagency_store::OwnedDispatchScope,
    expected: String,
    cancel: &Arc<AtomicBool>,
    ceiling: Instant,
) -> Result<(), Failure> {
    note(
        domain,
        cap,
        hagency_store::AttemptPhase::StopReported,
        stop_detail(
            report.cleanup,
            &report.exit_identity,
            &report.guardian_stderr_tail,
        ),
    )
    .await;
    // F3 (macOS-reachable): release the deferred approval entries as soon as
    // the leader stopped on every OS; the live reservation and owner release
    // still wait on the full `whole_tree_stopped` proof below.
    if leader_stopped(report.cleanup)
        && let Some(approvals) = &mut report.approvals
    {
        approvals.stopped();
    }
    if stopped(report.cleanup)
        && let Some(live) = &mut report.live
    {
        live.release();
    }
    if host.task_helper_enabled() {
        // Observation only, after actual owner stop and before negative fencing.
        // This adds one bounded (2 s) fresh-clock writer read. Done changes the
        // epoch and still fails the exact renewal/settlement fingerprint. Never
        // use this status as execution, release, retry or reply authority.
        report.canonical_status = observed_canonical_status(domain, cap, scope).await;
    }
    // Post-turn (ADR-183 decision D): the settlement of a turn Codex ended is
    // bounded by cancellation and the runtime's ceiling, never by the budget —
    // a turn that ran over it is still settled from what the store committed.
    checkpoint(cancel, ceiling)?;
    // A matching explicit Done+body is completion custody, not a renewed task
    // epoch or permission to continue this process. The same runner was stopped
    // above. Scope is the opaque successful Start response, never admission data.
    // A settlement verdict outranks the completion path: a drive that ended
    // in `SettlementUnknown` (ADR-046's conclusive negative reconcile) never
    // consults held completion custody, so it is neither replaced by
    // `CleanupUnknown` on macOS nor swallowed into a published completion on
    // Linux; `drive?` below surfaces it unchanged. Every other drive end that
    // is not a cancellation, a deadline, an unsupported approval or a named
    // peer-gone refusal keeps consulting custody: the retained helper-finish
    // flows end without a terminal Codex turn and complete through the held
    // row. Any other drive error (a protocol refusal, a lost authority) also
    // consults custody, and publication still requires the store's own held
    // completion row, so nothing is ever published that the store did not
    // commit as finished. The excluded causes are named in
    // `observes_completion` (review H5): a refusal there must not pin a
    // `settlement_cause` onto the report — the same first-cause rule brief
    // 14 installed, now guarding the marker, not just its overwrite.
    if observes_completion(&drive)
        && let Some(reference) = domain
            .observe_owned_completion(cap.clone(), started.clone())
            .await
            .map_err(|error| settlement_failure(report, &error))?
    {
        // A held completion reference is the store's own committed finish for
        // this dispatch, read back through custody: the canonical status it
        // proves is Done. That is an observation of the row, not a promotion
        // (ADR-053); a drive that ended in a settlement verdict never reaches
        // this block, so no unknown-fate frame is ever reported as Done.
        report.canonical_status = Some(TaskState::Done);
        checkpoint(cancel, ceiling)?;
        if !stopped(report.cleanup) {
            // Cleanup uncertainty is reported BESIDE the held completion
            // custody, never instead of it: the row is retained for
            // reconciliation and is not published, and the host does not assert
            // an unobserved Done (ADR-053 "observed, never promoted"; ADR-060
            // "a negative/unknown cleanup path never publishes").
            // The worker's failure finalization records the store's own
            // observation of this failure as the settlement; nothing written
            // here survives it, so nothing is written here.
            return Err(Failure::CleanupUnknown);
        }
        // Keep the receipt future alive. The writer checks this original signal
        // and monotonic deadline after queue/lock before admitting final content;
        // cancellation after that eligibility decision cannot undo its commit.
        domain
            .publish_owned_completion(
                cap.clone(),
                started,
                reference,
                cancel.clone(),
                ceiling.into_std(),
            )
            .await
            .map_err(|error| match checkpoint(cancel, ceiling).err() {
                Some(refused) => refused,
                None => settlement_failure(report, &error),
            })?;
        report.settlement = Settlement::CanonicalReplyReady;
        report.text = None; // Stored explicit content is the sole final body.
        report.owner.take();
        report.claude.take();
        report.octos.take();
        note(
            domain,
            cap,
            hagency_store::AttemptPhase::Settled,
            serde_json::json!({"settlement": "canonical_reply_ready"}),
        )
        .await;
        return Ok(());
    }
    drive?;
    checkpoint(cancel, ceiling)?;
    if report.protocol != Protocol::Completed {
        return Err(Failure::Protocol);
    }
    if !stopped(report.cleanup) {
        return Err(Failure::CleanupUnknown);
    }
    let text = report.text.as_deref().ok_or(Failure::Protocol)?;
    // From this checkpoint the bounded writer commit is deliberately not
    // cancellation-raced. A later caller cancellation cannot undo a commit.
    let task = domain
        .complete_owned_dispatch(
            cap.clone(),
            expected,
            serde_json::json!({"upstream_text":text}),
        )
        .await
        .map_err(|error| settlement_failure(report, &error))?;
    report.canonical_status = Some(task.status);
    report.settlement = Settlement::Completed;
    report.owner.take();
    report.claude.take();
    report.octos.take();
    note(
        domain,
        cap,
        hagency_store::AttemptPhase::Settled,
        serde_json::json!({"settlement": "completed"}),
    )
    .await;
    Ok(())
}

/// The fixed-label record of a stop, for the attempt's `stop_reported` event:
/// what the guardian reported, how the leader and the guardian exited, and
/// the guardian's own last words. Never free text beyond the bounded tail.
fn stop_detail(
    cleanup: Cleanup,
    exit_identity: &Option<String>,
    guardian_stderr_tail: &str,
) -> serde_json::Value {
    let mut detail = serde_json::json!({
        "exit_identity": exit_identity,
        "guardian_stderr_tail": guardian_stderr_tail,
    });
    match cleanup {
        Cleanup::Pending => detail["cleanup"] = "pending".into(),
        Cleanup::Unknown { kind } => {
            detail["cleanup"] = "unknown".into();
            detail["cleanup_error"] = format!("{kind:?}").into();
        }
        Cleanup::Observed(report) => {
            detail["cleanup"] = if report.scope.whole_tree_stopped {
                "whole_tree_stopped"
            } else {
                "unproven"
            }
            .into();
            detail["stop_cause"] = serde_json::to_value(report.cause).unwrap_or_default();
            detail["stop_detail"] = serde_json::to_value(report.detail).unwrap_or_default();
            detail["leader_exited"] = report.scope.leader_exited.into();
            detail["signals_accepted"] = report.scope.signals_accepted.into();
            detail["whole_tree_stopped"] = report.scope.whole_tree_stopped.into();
            detail["refusal"] = serde_json::to_value(report.refusal).unwrap_or_default();
            detail["live_count"] = report.live_count.into();
            detail["leader_status"] = report.leader_status.into();
            detail["guardian_exit"] = report.guardian_exit.into();
        }
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::{
        Failure, Report, SettlementCause, observes_completion, runner_activity, settlement_failure,
    };
    use hagency_store::Error;

    /// The runner's item events project into the activity vocabulary exactly
    /// as TS `codexActivity` (`router/src/runner-activity.ts:4-18`) maps them,
    /// and nothing else counts. Without this the notice's
    /// `工具调用 N 次，已返回 M 次` counters stay at zero forever (board #91
    /// addendum).
    #[test]
    fn native_runner_activity_projects_item_events() {
        use hagency_runtime::codex::session::{ItemPhase, Update};
        use hagency_store::ActivityEvent;

        // Every mapped codex item type, both phases.
        for (item_type, kind) in [
            ("commandExecution", "command"),
            ("fileChange", "files"),
            ("webSearch", "search"),
            ("mcpToolCall", "tool"),
            ("dynamicToolCall", "tool"),
            ("collabAgentToolCall", "delegate"),
        ] {
            use sha2::Digest;
            let start = runner_activity(&Update::Item {
                id: "tool-1".into(),
                kind: item_type.into(),
                phase: ItemPhase::Active,
            })
            .expect("a mapped item type starts a tool");
            assert_eq!(
                start,
                ActivityEvent::ToolStart {
                    kind: kind.into(),
                    event_id: format!("{:x}", sha2::Sha256::digest(b"tool-1")),
                }
            );
            let end = runner_activity(&Update::Item {
                id: "tool-1".into(),
                kind: item_type.into(),
                phase: ItemPhase::Complete,
            })
            .expect("a mapped item type returns");
            assert_eq!(
                end,
                ActivityEvent::ToolEnd {
                    kind: kind.into(),
                    event_id: format!("{:x}", sha2::Sha256::digest(b"tool-1")),
                }
            );
        }
        // TS `return null`: an unmapped item type, and every non-item update.
        for item_type in ["agentMessage", "reasoning", "userMessage", "plan"] {
            assert!(
                runner_activity(&Update::Item {
                    id: "x".into(),
                    kind: item_type.into(),
                    phase: ItemPhase::Active,
                })
                .is_none(),
                "{item_type} must not count as a tool"
            );
        }
        assert!(runner_activity(&Update::TurnStarted).is_none());
        assert!(runner_activity(&Update::Progress).is_none());
        assert!(runner_activity(&Update::TurnEnded).is_none());
    }

    /// H5: a named `PeerUnavailable` refusal never reaches settlement, so the
    /// failure path must not observe completion custody for it — a store
    /// refusal there can never pin a `settlement_cause` onto the named
    /// verdict. The pre-existing exclusions keep their meaning.
    #[test]
    fn native_peer_unavailable_carries_no_settlement_cause() {
        for excluded in [
            Failure::PeerUnavailable,
            Failure::Cancelled,
            Failure::Deadline,
            Failure::UnsupportedApproval,
        ] {
            assert!(
                !observes_completion(&Err::<(), Failure>(excluded.clone())),
                "{excluded:?} must not observe completion custody"
            );
        }
        // Everything else — other named failures and any success — still
        // observes custody. `SettlementUnknown` does NOT: ADR-060's landed
        // precedence rule — a settlement verdict outranks the completion
        // path, and the failure finalization is the single writer of
        // failure and settlement — so it joins the exclusion list.
        assert!(
            !observes_completion(&Err::<(), Failure>(Failure::SettlementUnknown)),
            "SettlementUnknown outranks the completion path (ADR-060)"
        );
        assert!(observes_completion(&Err::<(), Failure>(Failure::Protocol)));
        assert!(observes_completion(&Ok::<(), Failure>(())));
    }

    /// Every store refusal that can produce `Failure::SettlementUnknown` maps
    /// to its own marker, and anything else collapses to `Storage`. The
    /// mapping is a fixed diagnostic discriminant: it never carries the
    /// store's error text and never feeds authority, retry, reply or leases.
    #[test]
    fn native_settlement_cause_markers_are_distinct() {
        let mapped = [
            (Error::Busy, SettlementCause::QueueBusy),
            (Error::Unavailable, SettlementCause::QueueUnavailable),
            (Error::OutcomeUnknown, SettlementCause::ReplyTimedOut),
            (Error::RunnerAuthority, SettlementCause::RunnerAuthority),
            (Error::State, SettlementCause::State),
            (Error::Quarantined, SettlementCause::Quarantined),
        ];
        for (error, expected) in mapped {
            assert_eq!(SettlementCause::of(&error), expected, "{error:?}");
        }
        // Every other refusal — authority, IO, schema, capacity — reports as
        // durable storage, never as one of the queue or reply markers.
        let fallback = [
            Error::LocalAuthority,
            Error::Conflict,
            Error::NotFound,
            Error::Unqualified,
            Error::InsufficientCapacity,
            Error::Locked,
            Error::Schema,
            Error::Capacity,
            Error::Io(std::io::Error::other("fixture")),
            Error::Json(serde_json::from_str::<serde_json::Value>("{").unwrap_err()),
        ];
        for error in fallback {
            assert_eq!(
                SettlementCause::of(&error),
                SettlementCause::Storage,
                "{error:?}"
            );
        }
        // All eight markers are pairwise distinct discriminants. The eighth is
        // not derived from a store refusal (see `AcceptanceUnrecorded`), but it
        // must still be distinct from every refusal-derived marker so a trace
        // label can never be mistaken for a queue or reply attribution.
        let all = [
            SettlementCause::QueueBusy,
            SettlementCause::QueueUnavailable,
            SettlementCause::ReplyTimedOut,
            SettlementCause::RunnerAuthority,
            SettlementCause::State,
            SettlementCause::Quarantined,
            SettlementCause::Storage,
            SettlementCause::AcceptanceUnrecorded,
        ];
        for (i, left) in all.iter().enumerate() {
            for right in &all[i + 1..] {
                assert_ne!(left, right);
            }
        }
    }

    /// The first cause on a path is the root cause. `settlement_failure` fills an
    /// unset marker and never replaces one, so a later unrelated refusal cannot
    /// overwrite `AcceptanceUnrecorded` — the verdict that names the record that
    /// actually went missing.
    #[test]
    fn native_settlement_cause_keeps_the_first_marker() {
        fn blank() -> Report {
            Report::new(std::sync::Arc::new(std::sync::Mutex::new(None)))
        }
        let mut report = blank();
        assert_eq!(report.settlement_cause, None);
        // The reconcile's conclusive absence is the root cause.
        report.settlement_cause = Some(SettlementCause::AcceptanceUnrecorded);
        // A later refusal-derived marker must not replace it.
        assert_eq!(
            settlement_failure(&mut report, &Error::Busy),
            Failure::SettlementUnknown
        );
        assert_eq!(
            report.settlement_cause,
            Some(SettlementCause::AcceptanceUnrecorded)
        );
        assert_eq!(
            settlement_failure(&mut report, &Error::OutcomeUnknown),
            Failure::SettlementUnknown
        );
        assert_eq!(
            report.settlement_cause,
            Some(SettlementCause::AcceptanceUnrecorded)
        );
        // An unset marker is still filled, so the first writer wins.
        let mut fresh = blank();
        settlement_failure(&mut fresh, &Error::OutcomeUnknown);
        assert_eq!(fresh.settlement_cause, Some(SettlementCause::ReplyTimedOut));
        assert_eq!(
            settlement_failure(&mut fresh, &Error::Busy),
            Failure::SettlementUnknown
        );
        assert_eq!(
            fresh.settlement_cause,
            Some(SettlementCause::ReplyTimedOut),
            "the first refusal is the root cause"
        );
    }
}
