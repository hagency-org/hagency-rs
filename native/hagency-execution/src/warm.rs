//! One pre-activation owner, consumed once by the original dispatch worker.
//! No factory receipt, launcher, capability or canonical task is created here.
use crate::operation::{NativeStartup, OwnedWork, bounded, spawn_prepared};
use crate::{Failure, Limits, Operation, Report, SharedHost};
use hagency_core::{canonical, tasks::RunnerCapability};
use hagency_runtime::{codex::session, owned::OwnedSession};
use hagency_store::{
    DomainStore, OwnedDispatchScope, OwnedProvisionScope, agent_home::ManagedAgentHome,
};
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
    sync::{Notify, mpsc, oneshot},
    time::{Instant, MissedTickBehavior, interval},
};

#[derive(Clone, Copy)]
/// Finite initial operation (100 ms..30 s) and idle budget (100 ms..20 min).
/// These are distinct phases, not extensions of a normal/active operation.
pub struct WarmLimits {
    pub initialize: Limits,
    pub idle_ms: u64,
}
impl WarmLimits {
    pub(crate) fn validate(self) -> bool {
        self.initialize.validate()
            && self.initialize.operation_ms <= 30_000
            && (100..=hagency_runtime::codex::MAX_REQUEST_MS).contains(&self.idle_ms)
    }
}
/// ADR-183 decision D, the warm rule: what the idle re-qualification
/// recorded. Fixed labels only; the host projects them. A failing check
/// stops nothing: it is counted, named and re-checked on the next tick, and
/// the next handoff's own admission decides that dispatch (ADR-182 decision
/// 2). `failing_since_ms` names the current failing run and clears when a
/// check passes again; the counters keep the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct WarmIdleStatus {
    /// Idle checks that failed since the child became ready.
    pub failures: u64,
    /// Wall-clock ms of the first failure of the current failing run; `None`
    /// while the checks pass.
    pub failing_since_ms: Option<u64>,
    pub last_failed_at_ms: Option<u64>,
    /// The check (`AuthoritySite` word, or `warm_idle` for a check that
    /// exceeded its own response bound) and the cause of the last failure.
    pub last_site: Option<&'static str>,
    pub last_cause: Option<&'static str>,
}
struct Ready {
    result: Mutex<Option<Result<(), Failure>>>,
    changed: Notify,
    idle: Mutex<WarmIdleStatus>,
    /// The idle check that is failing RIGHT NOW, kept beside the counted
    /// history (ADR-183 decision D, warm rule). It never ends the child; it
    /// is what a handoff asks before admitting the next dispatch, so a
    /// check failing at that moment refuses THAT dispatch (ADR-182 decision
    /// 2) instead of starting a turn the operation's own admission would
    /// refuse a moment later. Cleared by the next passing check.
    idle_failure: Mutex<Option<Failure>>,
}
impl Ready {
    fn read(&self) -> Result<Option<Result<(), Failure>>, Failure> {
        self.result
            .lock()
            .map(|result| result.clone())
            .map_err(|_| Failure::Worker)
    }
    fn set(&self, result: Result<(), Failure>) {
        if let Ok(mut current) = self.result.lock() {
            // A later cancellation/teardown cannot rewrite the original
            // observed failure (including an expired retained ready wait).
            if !matches!(current.as_ref(), Some(Err(_))) {
                *current = Some(result);
            }
        }
        self.changed.notify_one();
    }
    fn idle_status(&self) -> WarmIdleStatus {
        self.idle.lock().map(|idle| *idle).unwrap_or_default()
    }
    /// The currently failing idle check, for a handoff to refuse on.
    fn idle_refusal(&self) -> Option<Failure> {
        self.idle_failure.lock().ok().and_then(|f| f.clone())
    }
    /// One transient idle refusal: counted, named, logged with the same
    /// labels (ADR-181 point 7). The child is untouched.
    fn idle_failed(&self, failure: &Failure) {
        let (site, cause) = match failure {
            Failure::LostAuthority { site, cause } => (site.as_str(), cause.as_str()),
            Failure::Deadline => ("warm_idle", "timed_out"),
            _ => ("warm_idle", "other"),
        };
        if let Ok(mut current) = self.idle_failure.lock() {
            *current = Some(failure.clone());
        }
        let now = wall_ms();
        let failures = if let Ok(mut idle) = self.idle.lock() {
            idle.failures = idle.failures.saturating_add(1);
            idle.failing_since_ms.get_or_insert(now);
            idle.last_failed_at_ms = Some(now);
            idle.last_site = Some(site);
            idle.last_cause = Some(cause);
            idle.failures
        } else {
            0
        };
        tracing::warn!(
            site,
            cause,
            failures,
            "warm idle check failed; recorded, the child is kept and re-checked"
        );
    }
    /// A passing check ends the failing run; the history stays.
    fn idle_passed(&self) {
        if let Ok(mut current) = self.idle_failure.lock() {
            *current = None;
        }
        if let Ok(mut idle) = self.idle.lock()
            && idle.failing_since_ms.take().is_some()
        {
            tracing::info!(failures = idle.failures, "warm idle check passes again");
        }
    }
}
fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or_default()
}
/// The negative evidence that still ends the warm child while it idles
/// (ADR-183 decision D, warm rule): cancellation; the idle budget itself
/// (the child's own lifetime, `WarmLimits::idle_ms`, which the transport
/// enforces too); the leader observed gone or unobservable (`WarmQualify`:
/// the child's own exit); a workspace root or home directory that is no
/// longer there (`WarmRoot`/`WarmScope` with the path missing — proven
/// gone, unlike a permission or identity refusal on a path that exists);
/// and the store retiring the scope (`WarmProvision` revoked, generation
/// or not found). Everything else is transient and only recorded: a local
/// provider directory that fails its mode or identity check
/// (`LocalCodexCheck` — its paths are the provider's own, so a missing one
/// is caught at the next handoff's admission, not here), a root or home
/// refusal on a present path, an account binding refusal, a busy, timed-out
/// or unavailable writer, and a check slower than its own response bound.
fn fatal_idle(failure: &Failure, binding: &Binding, idle_until: Instant) -> bool {
    use crate::{AuthorityCause, AuthoritySite};
    fn gone(path: &std::path::Path) -> bool {
        matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    }
    match failure {
        Failure::Cancelled => true,
        Failure::Deadline => Instant::now() >= idle_until,
        Failure::LostAuthority { site, cause } => match site {
            AuthoritySite::WarmQualify => true,
            AuthoritySite::WarmRoot => gone(binding.root.path()),
            AuthoritySite::WarmScope => {
                binding.home.workdir_path().is_ok_and(|path| gone(&path))
                    || binding.home.home_path().is_ok_and(|path| gone(&path))
            }
            AuthoritySite::WarmProvision => matches!(
                cause,
                AuthorityCause::Revoked | AuthorityCause::Generation | AuthorityCause::NotFound
            ),
            _ => false,
        },
        _ => false,
    }
}
/// Owning, non-cloneable handle. A dropped ready wait leaves this exact worker
/// intact; dropping the handle explicitly cancels and synchronously joins it.
/// Like Operation::drop, join must not run on a latency-sensitive HTTP/UI worker.
pub struct WarmRuntime {
    worker: Option<JoinHandle<()>>,
    command: mpsc::Sender<Command>,
    inspection: Option<Inspection>,
    activation: Option<Activation>,
    activated: bool,
    response_ms: u64,
    cancel: Arc<AtomicBool>,
    ready: Arc<Ready>,
    domain: DomainStore,
    host: SharedHost,
    /// The same binding the worker qualifies, kept here so a refused handoff
    /// leaves the factory a root for a follow-up launch (ADR-182 decision 2)
    /// instead of a spent runtime nothing can dispatch to again.
    binding: Binding,
}
enum Command {
    Dispatch {
        work: OwnedWork,
        until: Instant,
    },
    Observe {
        until: Instant,
        reply: oneshot::Sender<Result<(), Failure>>,
    },
    Activate {
        until: Instant,
        reply: oneshot::Sender<Result<hagency_core::project::Engagement, Failure>>,
    },
}
struct Inspection {
    until: Instant,
    reply: oneshot::Receiver<Result<(), Failure>>,
}
struct Activation {
    until: Instant,
    reply: oneshot::Receiver<Result<hagency_core::project::Engagement, Failure>>,
}
#[derive(Clone)]
pub(crate) struct Binding {
    scope: OwnedProvisionScope,
    _runtime_lease: Arc<hagency_store::OwnedRuntimeLease>,
    home: Arc<ManagedAgentHome>,
    root: Arc<crate::workspace::Root>,
    workspace_id: String,
    failure: Option<Failure>,
    local_codex: Option<Arc<crate::LocalCodex>>,
}
impl Binding {
    /// The root of a follow-up chain for an agent a restart re-attached. Every
    /// field is one the original warm start also held; the caller has already
    /// checked the scope, the home and the workspace root against each other.
    pub(crate) fn reattached(
        scope: OwnedProvisionScope,
        runtime_lease: Arc<hagency_store::OwnedRuntimeLease>,
        home: Arc<ManagedAgentHome>,
        root: Arc<crate::workspace::Root>,
        workspace_id: String,
        local_codex: Option<Arc<crate::LocalCodex>>,
    ) -> Self {
        Self {
            scope,
            _runtime_lease: runtime_lease,
            home,
            root,
            workspace_id,
            failure: None,
            local_codex,
        }
    }
    pub(crate) fn scope(&self) -> &OwnedProvisionScope {
        &self.scope
    }
    /// ADR-192: an attached agent with no warm child is ready while its
    /// workspace root and its local sign-in folder still check.
    pub(crate) fn check_attached(&self) -> Result<(), Failure> {
        self.root.check().map_err(|_| Failure::Admission)?;
        if let Some(local) = &self.local_codex {
            local.check()?;
        }
        Ok(())
    }
    async fn current(
        &self,
        domain: &DomainStore,
        cancel: &AtomicBool,
        until: Instant,
    ) -> Result<(), Failure> {
        if let Some(local) = &self.local_codex {
            local.admit_provision(&self.scope)?;
        }
        self.root
            .check()
            .map_err(|_| Failure::lost_io(crate::AuthoritySite::WarmRoot))?;
        self.home
            .check_provision_scope(&self.scope)
            .map_err(|error| Failure::lost(crate::AuthoritySite::WarmScope, &error))?;
        bounded(
            domain.validate_warm_runtime_scope(self.scope.clone()),
            cancel,
            until,
        )
        .await?
        .map_err(|error| Failure::lost(crate::AuthoritySite::WarmProvision, &error))?;
        self.root
            .check()
            .map_err(|_| Failure::lost_io(crate::AuthoritySite::WarmRoot))?;
        self.home
            .check_provision_scope(&self.scope)
            .map_err(|error| Failure::lost(crate::AuthoritySite::WarmScope, &error))?;
        if let Some(local) = &self.local_codex {
            local.admit_provision(&self.scope)?;
        }
        Ok(())
    }
    pub(crate) async fn check(
        &self,
        domain: &DomainStore,
        dispatch: &OwnedDispatchScope,
        cancel: &AtomicBool,
        until: Instant,
    ) -> Result<(), Failure> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        let [workspace] = dispatch.input().resources.as_slice() else {
            return Err(Failure::Admission);
        };
        if dispatch.engagement_id() != self.scope.engagement_id()
            || !workspace.exclusive
            || workspace.id != self.workspace_id
            || canonical::transport_digest(&serde_json::json!(dispatch.resource()))
                .map_err(|_| Failure::Admission)?
                != canonical::transport_digest(&serde_json::json!(self.scope.resource()))
                    .map_err(|_| Failure::Admission)?
        {
            return Err(Failure::Admission);
        }
        self.current(domain, cancel, until).await
    }
}
impl WarmRuntime {
    pub fn start(
        domain: DomainStore,
        scope: OwnedProvisionScope,
        home: Arc<ManagedAgentHome>,
        host: SharedHost,
        workspace_id: String,
        limits: WarmLimits,
    ) -> Result<Self, Failure> {
        let until = Instant::now() + Duration::from_millis(limits.initialize.operation_ms);
        Self::start_at(domain, scope, home, host, workspace_id, limits, until)
    }
    pub(crate) fn start_at(
        domain: DomainStore,
        scope: OwnedProvisionScope,
        home: Arc<ManagedAgentHome>,
        host: SharedHost,
        workspace_id: String,
        limits: WarmLimits,
        until: Instant,
    ) -> Result<Self, Failure> {
        if !limits.validate() {
            return Err(Failure::Admission);
        }
        if Instant::now() >= until {
            return Err(Failure::Deadline);
        }
        // Shared by all recaptured/cloned scopes in the producing writer. A
        // failed/unknown original job never grants another warm attempt.
        scope.claim_warm().map_err(|_| Failure::Admission)?;
        let runtime_lease = scope.claim_runtime().map_err(|_| Failure::Admission)?;
        let prepared = host
            .0
            .prepare_warm(&scope, &home, &workspace_id, limits.initialize)?;
        let live = match &host.0.approvals {
            // Initialize has no turn/approval channel. Reserve original process
            // capacity here; the active handoff separately enforces fits().
            Some(policy) => Some(policy.reserve_live()?),
            None => None,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Failure::Worker)?;
        let (command, mut receive) = mpsc::channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let ready = Arc::new(Ready {
            result: Mutex::new(None),
            changed: Notify::new(),
            idle: Mutex::new(WarmIdleStatus::default()),
            idle_failure: Mutex::new(None),
        });
        let notice = ready.clone();
        let source = domain.clone();
        let fixed = host.clone();
        let binding = Binding {
            scope: scope.clone(),
            _runtime_lease: runtime_lease.clone(),
            home: home.clone(),
            root: prepared.root.clone(),
            workspace_id: workspace_id.clone(),
            failure: None,
            local_codex: host.0.local_codex.clone(),
        };
        // Keep the original deadline including preparation and worker/queue delay.
        let worker = std::thread::Builder::new()
            .name("hagency-warm-owned-runtime".into())
            .spawn(move || {
                let mut report = Box::new(Report::new(Arc::new(Mutex::new(None))));
                report.warm = Some(Binding {
                    scope,
                    _runtime_lease: runtime_lease,
                    home,
                    root: prepared.root.clone(),
                    workspace_id,
                    failure: None,
                    local_codex: fixed.0.local_codex.clone(),
                });
                report.account = prepared.account;
                report.live = live;
                let runner = prepared.runner;
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // `prepare_warm` prepares app servers only (ADR-192).
                    let crate::host::PreparedRunner::Codex { settings, .. } = runner else {
                        return Err(Failure::Admission);
                    };
                    runtime.block_on(retain(
                        &source,
                        &fixed,
                        NativeStartup {
                            launch: prepared.launch,
                            settings,
                            io_limits: prepared.io_limits,
                        },
                        limits,
                        until,
                        Controls {
                            cancel: &signal,
                            ready: &notice,
                            receive: &mut receive,
                        },
                        &mut report,
                    ))
                }))
                .unwrap_or(Err(Failure::Worker));
                match outcome {
                    // block_on has returned, so the original work runs on this very
                    // runtime/thread, never inside a nested or replacement reactor.
                    Ok(work) => work(&runtime, Some(report)),
                    Err(failure) => {
                        // Serialize failure with admission: an already-enqueued
                        // dispatch must still get the original worker's negative
                        // finalization, not a silently discarded result channel.
                        notice.set(Err(failure.clone()));
                        match receive.try_recv() {
                            Ok(Command::Dispatch { work, .. }) => {
                                if let Some(binding) = &mut report.warm {
                                    binding.failure = Some(failure);
                                }
                                work(&runtime, Some(report));
                            }
                            Ok(Command::Observe { reply, .. }) => {
                                let _ = reply.send(Err(failure));
                                report.retry_stop();
                            }
                            Ok(Command::Activate { reply, .. }) => {
                                let _ = reply.send(Err(failure));
                                report.retry_stop();
                            }
                            Err(_) => {
                                report.retry_stop();
                            }
                        }
                    }
                }
            })
            .map_err(|_| Failure::Worker)?;
        Ok(Self {
            worker: Some(worker),
            command,
            inspection: None,
            activation: None,
            activated: false,
            response_ms: limits.initialize.response_ms,
            cancel,
            ready,
            domain,
            host,
            binding,
        })
    }
    /// The follow-up root this warm child was started from: what a refused
    /// handoff leaves the factory with. It carries no process and no failure.
    pub(crate) fn binding(&self) -> Binding {
        self.binding.clone()
    }
    /// What the idle re-qualification recorded (ADR-183 decision D, warm
    /// rule): for the host to project; never authority.
    pub fn idle_status(&self) -> WarmIdleStatus {
        self.ready.idle_status()
    }
    /// Only the concrete factory bridge requests activation after checking its
    /// original enrolled SDK. The original worker qualifies its physical owner
    /// immediately before the same writer's scoped transaction, then rechecks.
    pub(crate) async fn activate(&mut self) -> Result<hagency_core::project::Engagement, Failure> {
        if self.activated || self.inspection.is_some() {
            return Err(Failure::Admission);
        }
        match self.ready.read()? {
            Some(Ok(())) => {}
            Some(Err(failure)) => return Err(failure),
            None => return Err(Failure::Admission),
        }
        if self.cancel.load(Ordering::Acquire) {
            return Err(Failure::Cancelled);
        }
        if self.activation.is_none() {
            let until = Instant::now() + Duration::from_millis(self.response_ms);
            let (reply, receive) = oneshot::channel();
            self.command
                .try_send(Command::Activate { until, reply })
                .map_err(|_| Failure::lost_io(crate::AuthoritySite::CommandChannel))?;
            self.activation = Some(Activation {
                until,
                reply: receive,
            });
        }
        let activation = self.activation.as_mut().ok_or(Failure::Worker)?;
        let until = activation.until;
        let mut result = match tokio::time::timeout_at(until, &mut activation.reply).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(Failure::lost_io(crate::AuthoritySite::CommandChannel)),
            Err(_) => Err(Failure::Deadline),
        };
        if result.is_ok() {
            if let Some(Err(failure)) = self.ready.read()? {
                result = Err(failure);
            } else if self.cancel.load(Ordering::Acquire) {
                result = Err(Failure::Cancelled);
            } else if Instant::now() >= until {
                result = Err(Failure::Deadline);
            }
        }
        self.activation = None;
        match &result {
            Ok(_) => self.activated = true,
            Err(failure) => {
                self.ready.set(Err(failure.clone()));
                self.cancel();
            }
        }
        result
    }
    /// Fresh inspection on the original worker. A dropped wait retains its
    /// exact receiver/deadline; an expired buffered positive cannot be reused.
    pub async fn ready(&mut self) -> Result<(), Failure> {
        loop {
            let changed = self.ready.changed.notified();
            if let Some(result) = self.ready.read()? {
                result?;
                break;
            }
            changed.await;
        }
        if self.cancel.load(Ordering::Acquire) {
            return Err(Failure::Cancelled);
        }
        if self.inspection.is_none() {
            let until = Instant::now() + Duration::from_millis(self.response_ms);
            let (reply, receive) = oneshot::channel();
            self.command
                .try_send(Command::Observe { until, reply })
                .map_err(|_| Failure::lost_io(crate::AuthoritySite::CommandChannel))?;
            self.inspection = Some(Inspection {
                until,
                reply: receive,
            });
        }
        // Borrow the retained receiver: dropping this wait cannot discard or
        // enqueue a replacement inspection, or extend its original deadline.
        let inspection = self.inspection.as_mut().ok_or(Failure::Worker)?;
        let until = inspection.until;
        let mut result = match tokio::time::timeout_at(until, &mut inspection.reply).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(Failure::lost_io(crate::AuthoritySite::CommandChannel)),
            Err(_) => Err(Failure::Deadline),
        };
        if result.is_ok() {
            if let Some(Err(failure)) = self.ready.read()? {
                result = Err(failure);
            } else if self.cancel.load(Ordering::Acquire) {
                result = Err(Failure::Cancelled);
            } else if Instant::now() >= until {
                result = Err(Failure::Deadline);
            }
        }
        self.inspection = None;
        if let Err(failure) = &result {
            self.ready.set(Err(failure.clone()));
            self.cancel();
        }
        result
    }
    pub fn is_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn dispatch(
        self,
        capability: RunnerCapability,
        limits: Limits,
    ) -> Result<Operation, Failure> {
        self.handoff(capability, limits, false)
    }
    pub fn dispatch_requiring_workspace(
        self,
        capability: RunnerCapability,
        limits: Limits,
    ) -> Result<Operation, Failure> {
        self.handoff(capability, limits, true)
    }
    fn handoff(
        mut self,
        capability: RunnerCapability,
        limits: Limits,
        required: bool,
    ) -> Result<Operation, Failure> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(Failure::Cancelled);
        }
        if self.inspection.is_some() || self.activation.is_some() {
            return Err(Failure::Admission);
        }
        match self.ready.read()? {
            Some(Ok(())) => {}
            Some(Err(failure)) => return Err(failure),
            None => return Err(Failure::Admission),
        }
        // ADR-183 decision D (warm rule): an idle check that failed does not
        // end the child, but one that is failing right now refuses this
        // handoff — nothing has started, so the driver requeues the dispatch
        // with its launch backoff and the worker continues (ADR-182 decision
        // 2). Admitting it here would start a turn the operation's own
        // admission refuses a moment later, which would cost the dispatch.
        if let Some(failure) = self.ready.idle_refusal() {
            return Err(failure);
        }
        let until = Instant::now() + Duration::from_millis(limits.operation_ms);
        let (mut operation, work) = Operation::prepare(
            self.domain.clone(),
            capability,
            self.host.clone(),
            limits,
            required,
            true,
        )?;
        let current = self.ready.result.lock().map_err(|_| Failure::Worker)?;
        match current.as_ref() {
            Some(Ok(())) => {}
            Some(Err(failure)) => return Err(failure.clone()),
            None => return Err(Failure::Admission),
        }
        self.command
            .try_send(Command::Dispatch { work, until })
            .map_err(|_| Failure::lost_io(crate::AuthoritySite::WarmDispatch))?;
        drop(current);
        operation.adopt_worker(self.worker.take().ok_or(Failure::Worker)?);
        // With no worker left, Drop must not cancel the transferred owner.
        Ok(operation)
    }
}
impl Drop for WarmRuntime {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.cancel();
            let _ = worker.join();
        }
    }
}
async fn initialized<F: Future<Output = Result<(), session::Error>>>(
    future: F,
    binding: &Binding,
    domain: &DomainStore,
    account: Option<&hagency_store::ManagedLaunch>,
    cancel: &AtomicBool,
    until: Instant,
) -> Result<(), Failure> {
    tokio::pin!(future);
    let mut tick = interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _=tokio::time::sleep_until(until)=>return Err(Failure::Deadline),
            _=tick.tick()=>{binding.current(domain,cancel,until).await?;if let Some(account)=account {account.check().map_err(|error|Failure::lost(crate::AuthoritySite::AccountCheck,&error))?;}},
            result=&mut future=>return result.map_err(|_|Failure::Protocol),
        }
    }
}
struct Controls<'owner> {
    cancel: &'owner Arc<AtomicBool>,
    ready: &'owner Ready,
    receive: &'owner mut mpsc::Receiver<Command>,
}
/// The scope half of a qualification: the binding's current checks and the
/// account binding. Its refusal is the transient class while idle.
async fn scope_current(
    domain: &DomainStore,
    binding: &Binding,
    account: Option<&hagency_store::ManagedLaunch>,
    cancel: &AtomicBool,
    until: Instant,
) -> Result<(), Failure> {
    binding.current(domain, cancel, until).await?;
    if let Some(account) = account {
        account
            .check()
            .map_err(|error| Failure::lost(crate::AuthoritySite::AccountCheck, &error))?;
    }
    Ok(())
}
/// The full qualification, strict: every refusal is returned. Used at
/// activation and at the handoff (the dispatch's own admission, ADR-182
/// decision 2); the idle tick wraps it in `requalify`. The leader
/// observation — the child's own evidence — runs whatever the scope checks
/// said, on a bound taken at entry, so a slow scope check cannot starve it.
async fn qualify(
    domain: &DomainStore,
    binding: &Binding,
    owner: &mut OwnedSession,
    account: Option<&hagency_store::ManagedLaunch>,
    cancel: &AtomicBool,
    until: Instant,
) -> Result<(), Failure> {
    let owner_bound = until
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(Failure::Deadline)?
        .min(Duration::from_secs(5));
    let before = scope_current(domain, binding, account, cancel, until).await;
    owner
        .qualify_ready_owner(owner_bound)
        .map_err(|_| Failure::lost_io(crate::AuthoritySite::WarmQualify))?;
    // Actual physical IO precedes the final original writer observation.
    let after = scope_current(domain, binding, account, cancel, until).await;
    if cancel.load(Ordering::Acquire) {
        return Err(Failure::Cancelled);
    }
    before?;
    after?;
    if Instant::now() >= until {
        return Err(Failure::Deadline);
    }
    Ok(())
}
/// One idle tick's bounds and record (ADR-183 decision D, warm rule):
/// `until` is this check's own bound, `idle_until` the child's idle budget
/// that still ends it, `ready` where a transient refusal is recorded.
struct IdleTick<'owner> {
    until: Instant,
    idle_until: Instant,
    ready: &'owner Ready,
}
/// One idle re-qualification (ADR-183 decision D, warm rule): the same checks
/// as `qualify`, with the transient class recorded on `ready` instead of
/// returned, so the worker keeps the child and re-checks on the next tick.
/// Only `fatal_idle`'s evidence comes back as an error.
async fn requalify(
    domain: &DomainStore,
    binding: &Binding,
    owner: &mut OwnedSession,
    account: Option<&hagency_store::ManagedLaunch>,
    cancel: &AtomicBool,
    tick: IdleTick<'_>,
) -> Result<(), Failure> {
    match qualify(domain, binding, owner, account, cancel, tick.until).await {
        Ok(()) => {
            tick.ready.idle_passed();
            Ok(())
        }
        Err(failure) if fatal_idle(&failure, binding, tick.idle_until) => Err(failure),
        Err(failure) => {
            tick.ready.idle_failed(&failure);
            Ok(())
        }
    }
}
async fn retain(
    domain: &DomainStore,
    host: &SharedHost,
    startup: NativeStartup,
    limits: WarmLimits,
    until: Instant,
    controls: Controls<'_>,
    report: &mut Report,
) -> Result<OwnedWork, Failure> {
    let Controls {
        cancel,
        ready,
        receive,
    } = controls;
    report
        .warm
        .as_ref()
        .ok_or(Failure::Worker)?
        .current(domain, cancel, until)
        .await?;
    if let Some(live) = &mut report.live {
        live.possible();
    }
    spawn_prepared(
        host.0.clone(),
        startup,
        limits.initialize,
        cancel,
        until,
        report,
    )
    .await?;
    let binding = report.warm.as_ref().ok_or(Failure::Worker)?;
    let owner: &mut OwnedSession = report.owner.as_mut().ok_or(Failure::SpawnFailed)?;
    owner.reserve_warm_idle().map_err(|_| Failure::Protocol)?;
    bounded(
        initialized(
            owner.initialize(),
            binding,
            domain,
            report.account.as_ref(),
            cancel,
            until,
        ),
        cancel,
        until,
    )
    .await??;
    binding.current(domain, cancel, until).await?;
    if let Some(account) = &report.account {
        account
            .check()
            .map_err(|error| Failure::lost(crate::AuthoritySite::AccountCheck, &error))?;
    }
    let idle_until = Instant::now() + Duration::from_millis(limits.idle_ms);
    owner
        .enter_warm_idle(idle_until)
        .map_err(|_| Failure::Protocol)?;
    ready.set(Ok(()));
    // The idle loop (ADR-183 decision D, warm rule): every 100 ms the child
    // is re-qualified, and a transient refusal is recorded on `ready` and
    // re-checked, never a stop; `fatal_idle` names what still ends it. The
    // readiness inspection (`Observe`) is the same check on the host's own
    // bound and answers Ok while the child is up, with the refusal in the
    // idle status. Activation and the handoff keep the strict `qualify`: they
    // are admissions of their own, not idle re-qualification.
    let wait = async {
        let mut tick = interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _=tick.tick()=>requalify(domain,binding,owner,report.account.as_ref(),cancel,IdleTick {
                    until: idle_until.min(Instant::now()+Duration::from_millis(limits.initialize.response_ms)),idle_until,ready}).await?,
                command=receive.recv()=>match command.ok_or(Failure::Cancelled)? {
                    Command::Dispatch {work,until}=>{
                        // Transfer the closure out of the cancellable wait
                        // before any inspection await can drop consumed work.
                        return Ok((work,until));
                    },
                    Command::Observe {until,reply}=>{
                        let result=requalify(domain,binding,owner,report.account.as_ref(),cancel,IdleTick {until: until.min(idle_until),idle_until,ready}).await;
                        let failure=result.clone().err();let _=reply.send(result);
                        if let Some(failure)=failure {return Err(failure);}
                    },
                    Command::Activate {until,reply}=>{
                        let until=until.min(idle_until);
                        let result=async {
                            qualify(domain,binding,owner,report.account.as_ref(),cancel,until).await?;
                            let acknowledgment=bounded(domain.complete_original_provision(binding.scope.clone()),cancel,until).await?
                                .map_err(|error|Failure::lost(crate::AuthoritySite::WarmProvision,&error))?;
                            qualify(domain,binding,owner,report.account.as_ref(),cancel,until).await?;
                            Ok(acknowledgment)
                        }.await;
                        let failure=result.clone().err();let _=reply.send(result);
                        if let Some(failure)=failure {return Err(failure);}
                    },
                },
            }
        }
    };
    let (work, dispatch_until) = bounded(wait, cancel, idle_until).await??;
    // The owned work is now retained outside the bounded waiting future. Even
    // failed/expired physical qualification runs its original finalization.
    let failure = qualify(
        domain,
        report.warm.as_ref().ok_or(Failure::Worker)?,
        report.owner.as_mut().ok_or(Failure::Worker)?,
        report.account.as_ref(),
        cancel,
        dispatch_until
            .min(idle_until)
            .min(Instant::now() + Duration::from_millis(limits.initialize.response_ms)),
    )
    .await
    .err();
    if let Some(failure) = failure {
        report.warm.as_mut().ok_or(Failure::Worker)?.failure = Some(failure);
    }
    Ok(work)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_warm_ready_failure_sticky() {
        let ready = Ready {
            result: Mutex::new(None),
            changed: Notify::new(),
            idle: Mutex::new(WarmIdleStatus::default()),
            idle_failure: Mutex::new(None),
        };
        ready.set(Ok(()));
        assert_eq!(ready.read().unwrap(), Some(Ok(())));
        ready.set(Err(Failure::Deadline));
        ready.set(Err(Failure::Cancelled));
        ready.set(Ok(()));
        assert_eq!(ready.read().unwrap(), Some(Err(Failure::Deadline)));
    }
}
