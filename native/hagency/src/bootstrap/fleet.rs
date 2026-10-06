//! Original inline agents, finite service ownership and per-agent file routing.
use super::{
    DriverMode, Failure, Shared, Status, StatusHandle, driver::Driver, workspace::WorkspaceAccess,
};
use crate::{
    file_service::{FileHandle, FileOwner},
    receive_service::{ReceiveHandle, ReceiveOwner},
};
use hagency_core::tasks::RunnerCapability;
use hagency_matrix::{CancellationToken, Collector, ProvisionedAgent};
use hagency_store::DomainStore;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Fixed private Host composition, not a complete deployment profile.
pub struct Setup {
    pub state: PathBuf,
    pub limit: usize,
    pub send: bool,
    pub receive: bool,
    pub limits: hagency_execution::Limits,
}
#[derive(Clone)]
pub(crate) struct Backends {
    pub files: Option<FileHandle>,
    pub receives: Option<ReceiveHandle>,
    status: StatusHandle,
}
#[derive(Clone)]
pub(crate) struct Routes {
    domain: DomainStore,
    entries: Arc<Mutex<BTreeMap<String, Backends>>>,
    running: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
}

/// An agent whose attempt failed is lost to the fleet unless its worker is
/// alive with that failure in the store's hands: unresolved dispatches the
/// operator resolves, or an open fence (ADR-182). Its own status keeps
/// reporting the failure either way.
fn lost(status: &super::Status) -> bool {
    status.error.is_some() && status.unresolved_dispatches == 0 && status.fenced.is_none()
}
impl Routes {
    /// Current worker observation only; no private runtime details leave here.
    pub(crate) fn agent_availability(&self, engagement: &str) -> &'static str {
        if self.closed.load(Ordering::Acquire) || !self.running.load(Ordering::Acquire) {
            return "unavailable";
        }
        let Ok(entries) = self.entries.lock() else {
            return "unavailable";
        };
        let Some(entry) = entries.get(engagement) else {
            return "not_attached";
        };
        let status = entry.status.get();
        if status.fenced.is_some() || status.unresolved_dispatches > 0 {
            return "blocked";
        }
        if status.error.is_some()
            || status.matrix_error.is_some()
            || matches!(
                status.state,
                "not_started"
                    | "not_attached"
                    | "awaiting_owner"
                    | "closed"
                    | "retiring"
                    | "refresh_refused"
                    | "awaiting_operator"
                    | "launch_refused"
            )
        {
            return "unavailable";
        }
        "available"
    }
    pub(crate) async fn select(
        &self,
        cap: RunnerCapability,
    ) -> Result<Backends, hagency_store::Error> {
        // Full original credential authentication is a bounded writer read,
        // including for historical GET after workspace release. This lookup
        // itself grants no execution/IO; each backend still checks its scope.
        let engagement = self.domain.runner_service_engagement(cap).await?;
        self.entries
            .lock()
            .map_err(|_| hagency_store::Error::OutcomeUnknown)?
            .get(&engagement)
            .cloned()
            .ok_or(hagency_store::Error::NotFound)
    }
    fn insert(&self, engagement: String, backends: Backends) -> Result<(), Failure> {
        let mut entries = self.entries.lock().map_err(|_| Failure::OutcomeUnknown)?;
        // A row that only said "waiting for the owner" is replaced by the
        // agent it was waiting for; any other existing row is a conflict.
        let placeholder = entries
            .get(&engagement)
            .is_some_and(|entry| entry.status.state() == "awaiting_owner");
        if self.closed.load(Ordering::Acquire)
            || (entries.len() >= 17 && !placeholder)
            || (entries.contains_key(&engagement) && !placeholder)
        {
            return Err(Failure::Registration);
        }
        entries.insert(engagement, backends);
        Ok(())
    }
    /// Drop a "waiting for the owner" row whose provision no longer waits and
    /// was not admitted. Any other row is left alone.
    fn remove_awaiting(&self, engagement: &str) {
        if let Ok(mut entries) = self.entries.lock()
            && entries
                .get(engagement)
                .is_some_and(|entry| entry.status.state() == "awaiting_owner")
        {
            entries.remove(engagement);
        }
    }
    fn awaiting_rows(&self) -> Vec<String> {
        self.entries
            .lock()
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(_, entry)| entry.status.state() == "awaiting_owner")
                    .map(|(engagement, _)| engagement.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
    pub(crate) fn state(&self) -> &'static str {
        if self.closed.load(Ordering::Acquire) {
            return "stopped";
        }
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if self.failed.load(Ordering::Acquire)
            || entries.values().any(|entry| lost(&entry.status.get()))
        {
            return "outcome_unknown";
        }
        if self.running.load(Ordering::Acquire) {
            "running"
        } else {
            "not_started"
        }
    }
    pub(crate) fn snapshot(&self) -> serde_json::Value {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let failed = self.failed.load(Ordering::Acquire)
            || entries.values().any(|entry| lost(&entry.status.get()));
        let agents: Vec<_>=entries.iter().map(|(engagement,entry)|serde_json::json!({"engagement_id":engagement,"status":entry.status.get()})).collect();
        serde_json::json!({"profile":"inline_factory_service_checkpoint_v1","running":self.running.load(Ordering::Acquire),
            "closed":self.closed.load(Ordering::Acquire),"failed":failed,"registered_backends":entries.len(),"agents":agents})
    }
}
struct AgentOwner {
    shared: Shared,
    status: StatusHandle,
    driver: Option<Driver>,
    files: Option<FileOwner>,
    receives: Option<ReceiveOwner>,
    /// ADR-187 §A.5: an imported fleet's agent polls its own invitations
    /// (TS `pollAgentInvites`); stopped with the agent.
    invites: Option<(CancellationToken, tokio::task::JoinHandle<()>)>,
}
impl AgentOwner {
    fn quiesce(&self) {
        if let Some((cancel, _)) = &self.invites {
            cancel.cancel();
        }
        if let Some(driver) = &self.driver {
            driver.cancel();
        }
        if let Some(files) = &self.files {
            files.quiesce();
        }
        if let Some(receives) = &self.receives {
            receives.quiesce();
        }
        self.shared.workspace.retire();
    }
    async fn close(&mut self) -> Result<(), Failure> {
        self.quiesce();
        let mut failed = false;
        if let Some(files) = &mut self.files {
            failed |= files.close().await.is_err();
        }
        if let Some(receives) = &mut self.receives {
            failed |= receives.close().await.is_err();
        }
        if let Some(driver) = &mut self.driver {
            failed |= driver.close().await.is_err();
        }
        if failed {
            self.status.fail(Failure::OutcomeUnknown);
            Err(Failure::OutcomeUnknown)
        } else {
            self.status.phase("closed");
            Ok(())
        }
    }
}
/// Where the fleet's agents come from: a coordinator install's Collector, or
/// (ADR-187) an imported fleet's own provisioning host.
#[derive(Clone)]
pub(crate) enum Provider {
    Coordinator(Arc<Collector>),
    Fleet {
        host: Arc<hagency_matrix::TokenProvisioningHost>,
        sweep: Arc<hagency_matrix::MembershipSweep>,
        /// ADR-187 amendment: each owner's approval pump; an agent's approval
        /// requests go to its owner's.
        owner_notices: Arc<
            Mutex<BTreeMap<String, tokio::sync::mpsc::Sender<hagency_execution::ApprovalRequests>>>,
        >,
        /// Each admitted agent's transport, for its public approval notice.
        agents: super::approval::AgentDirectory,
    },
}
impl Provider {
    async fn provisioned_engagements(
        &self,
        domain: &DomainStore,
    ) -> Result<Vec<String>, hagency_matrix::Error> {
        match self {
            Self::Coordinator(c) => c.provisioned_engagements().await,
            Self::Fleet { host, .. } => host.provisioned_engagements(domain).await,
        }
    }
    async fn reattach(
        &self,
        domain: &DomainStore,
        engagement: &str,
        cancel: &CancellationToken,
    ) -> Result<hagency_matrix::ProvisionedAgent, hagency_matrix::Error> {
        match self {
            Self::Coordinator(c) => {
                c.reattach_provisioned_agent(engagement, cancel).await?;
                c.take_provisioned_agent(engagement)
            }
            Self::Fleet { host, .. } => {
                let (task_host, domain, id, cancel) = (
                    host.clone(),
                    domain.clone(),
                    engagement.to_owned(),
                    cancel.clone(),
                );
                tokio::spawn(
                    async move { task_host.reattach_completed(&domain, &id, &cancel).await },
                )
                .await
                .map_err(|_| hagency_matrix::Error::OutcomeUnknown)??;
                host.take_agent(engagement)
            }
        }
    }
    fn awaiting_owner_engagements(&self) -> Vec<(String, u64)> {
        match self {
            Self::Coordinator(c) => c.awaiting_owner_engagements(),
            Self::Fleet { host, .. } => host.awaiting_owner_engagements(),
        }
    }
    fn take_next(&self) -> Result<Option<hagency_matrix::ProvisionedAgent>, hagency_matrix::Error> {
        match self {
            Self::Coordinator(c) => c.take_next_provisioned_agent(),
            Self::Fleet { host, .. } => host.take_next_agent(),
        }
    }
    fn sweep(&self, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
        match self {
            Self::Coordinator(c) => tokio::spawn(c.clone().membership_sweep_loop(cancel)),
            Self::Fleet { sweep, .. } => tokio::spawn(sweep.clone().run(cancel)),
        }
    }
    async fn close_agents(&self) -> Result<(), hagency_matrix::Error> {
        match self {
            Self::Coordinator(c) => c.close_provisioned_agents().await,
            Self::Fleet { host, .. } => host.close_agents().await,
        }
    }
}
/// Host-only original fleet service. Bootstrap owns it; no HTTP/runtime input
/// can install a backend, reconstruct a factory owner or assert readiness.
pub struct Service {
    setup: Setup,
    provider: Provider,
    domain: DomainStore,
    routes: Routes,
    agents: Vec<AgentOwner>,
    factory_close: Option<tokio::task::JoinHandle<Result<(), hagency_matrix::Error>>>,
    factory_closed: Option<Result<(), Failure>>,
    started: bool,
}
impl Service {
    pub fn new(
        domain: DomainStore,
        coordinator: Arc<Collector>,
        setup: Setup,
    ) -> Result<Self, Failure> {
        Self::with_provider(domain, Provider::Coordinator(coordinator), setup)
    }
    /// ADR-187: a fleet service whose agents come from `provider`.
    pub(crate) fn with_provider(
        domain: DomainStore,
        provider: Provider,
        setup: Setup,
    ) -> Result<Self, Failure> {
        if !setup.state.is_absolute()
            || setup.limit == 0
            || setup.limit > hagency_core::file_delivery::MAX_FILE_BYTES as usize
            || (setup.receive && hagency_core::received_files::receive_limit(setup.limit).is_err())
            || !setup.limits.validate()
        {
            return Err(Failure::Config {
                field: "factory-service.json",
                fix: "receive limit must be a valid received-files limit, send limit within the file byte ceiling, and limits valid",
            });
        }
        hagency_store::private::directory(&setup.state).map_err(|_| Failure::Config {
            field: "factory-service state directory",
            fix: "the directory must exist, be owner-private (0700) and writable by the service",
        })?;
        if setup.send {
            hagency_store::private::directory(&setup.state.join("factory-file-media"))
                .map_err(|_| Failure::Config {
                    field: "factory-file-media",
                    fix: "the media directory must exist, be owner-private (0700) and writable by the service",
                })?;
        }
        let routes = Routes {
            domain: domain.clone(),
            entries: Arc::new(Mutex::new(BTreeMap::new())),
            running: Arc::new(AtomicBool::new(false)),
            failed: Arc::new(AtomicBool::new(false)),
            closed: Arc::new(AtomicBool::new(false)),
        };
        Ok(Self {
            setup,
            provider,
            domain,
            routes,
            agents: vec![],
            factory_close: None,
            factory_closed: None,
            started: false,
        })
    }
    pub(crate) fn routes(&self) -> Routes {
        self.routes.clone()
    }
    pub(crate) fn register_root(
        &self,
        engagement: String,
        files: Option<FileHandle>,
        receives: Option<ReceiveHandle>,
        status: StatusHandle,
    ) -> Result<(), Failure> {
        self.routes.insert(
            engagement,
            Backends {
                files,
                receives,
                status,
            },
        )
    }
    pub fn statuses(&self) -> Vec<Status> {
        self.agents.iter().map(|agent| agent.status.get()).collect()
    }
    async fn admit(
        &mut self,
        agent: ProvisionedAgent,
        notices: tokio::sync::mpsc::Sender<hagency_execution::ApprovalRequests>,
        reattached: bool,
    ) -> Result<(), Failure> {
        if self.agents.len() >= 16 || self.routes.closed.load(Ordering::Acquire) {
            return Err(Failure::Registration);
        }
        let engagement = agent.session().engagement_id.clone();
        // An imported fleet's agent uses its owner's approval pump.
        let notices = match &self.provider {
            Provider::Coordinator(_) => notices,
            Provider::Fleet { owner_notices, .. } => {
                let owner = self
                    .domain
                    .engagement_owner(engagement.clone())
                    .await
                    .map_err(|_| Failure::OutcomeUnknown)?
                    .ok_or(Failure::Registration)?;
                owner_notices
                    .lock()
                    .map_err(|_| Failure::OutcomeUnknown)?
                    .get(&owner)
                    .cloned()
                    .ok_or(Failure::Registration)?
            }
        };
        let shared = Shared {
            domain: self.domain.clone(),
            collector: agent.shared_collector(),
            workspace: WorkspaceAccess::new(),
        };
        if let Provider::Fleet { agents, .. } = &self.provider
            && let Ok(mut agents) = agents.lock()
        {
            agents.insert(engagement.clone(), shared.collector.clone());
        }
        // Retain the partially constructed owner before any failing startup or
        // await. Its original factory is also still held by the coordinator.
        let invites = matches!(self.provider, Provider::Fleet { .. }).then(|| {
            let cancel = CancellationToken::new();
            let task = super::invites::start(shared.clone(), cancel.clone());
            (cancel, task)
        });
        self.agents.push(AgentOwner {
            shared,
            status: StatusHandle::for_mode(DriverMode::Continuous),
            driver: None,
            files: None,
            receives: None,
            invites,
        });
        let owner = self.agents.last_mut().ok_or(Failure::Startup)?;
        let start = async {
            if self.setup.send {
                let namespace = hagency_core::canonical::digest(&serde_json::json!([
                    "native_factory_file_storage_v1",
                    engagement
                ]))
                .map_err(|_| Failure::Config {
                    field: "factory file storage namespace",
                    fix: "canonical digest failed; the engagement id must stay ASCII",
                })?;
                owner.files = Some(
                    FileOwner::start(
                        owner.shared.clone(),
                        crate::file_service::Setup {
                            directory: self.setup.state.join("factory-file-media").join(&namespace),
                            namespace,
                            limit: self.setup.limit,
                        },
                    )
                    .map_err(|_| Failure::Startup)?,
                );
            }
            if self.setup.receive {
                owner.receives = Some(
                    ReceiveOwner::start(
                        owner.shared.clone(),
                        crate::receive_service::Setup {
                            limit: self.setup.limit,
                        },
                    )
                    .map_err(|_| Failure::Startup)?,
                );
            }
            self.routes.insert(
                engagement,
                Backends {
                    files: owner.files.as_ref().map(FileOwner::handle),
                    receives: owner.receives.as_ref().map(ReceiveOwner::handle),
                    status: owner.status.clone(),
                },
            )?;
            owner.driver = Some(
                Driver::start_agent(
                    agent,
                    owner.shared.clone(),
                    owner.files.as_ref().map(FileOwner::handle),
                    owner.status.clone(),
                    notices,
                    self.setup.limits,
                )
                .await?,
            );
            Ok(())
        }
        .await;
        if let Err(error) = start {
            owner.quiesce();
            if reattached {
                // An agent a restart could not bring back is skipped and shown;
                // it fails neither the fleet nor the agents that did come back.
                owner.status.not_attached(None);
            } else {
                owner.status.fail(error);
                self.routes.failed.store(true, Ordering::Release);
            }
        }
        start
    }
    /// After a restart: bring back every agent this service's inline factory
    /// already completed, from what their provision left on disk. One agent that
    /// cannot come back is skipped and published as `not_attached`; it never
    /// stops the others, the coordinator or readiness.
    async fn reattach_known_agents(
        &mut self,
        notices: &tokio::sync::mpsc::Sender<hagency_execution::ApprovalRequests>,
        cancel: &CancellationToken,
    ) {
        let known = match self.provider.provisioned_engagements(&self.domain).await {
            Ok(known) => known,
            Err(error) => {
                tracing::warn!(?error, "factory agents could not be listed for re-attach");
                return;
            }
        };
        for engagement in known {
            if cancel.is_cancelled() || self.routes.closed.load(Ordering::Acquire) {
                return;
            }
            let attached = self
                .provider
                .reattach(&self.domain, &engagement, cancel)
                .await;
            match attached {
                Ok(agent) => {
                    if self.admit(agent, notices.clone(), true).await.is_ok() {
                        tracing::info!(%engagement, "factory agent re-attached");
                    } else {
                        tracing::warn!(%engagement, "re-attached factory agent did not start");
                    }
                }
                Err(hagency_matrix::Error::Busy) => {
                    // This process already owns the provision (the coordinator
                    // completed it before the fleet started): ordinary
                    // discovery takes it, exactly once, as it always did.
                    tracing::debug!(%engagement, "factory agent already owned here; left to discovery");
                }
                Err(error) => {
                    tracing::warn!(%engagement, ?error, "factory agent not re-attached");
                    let status = StatusHandle::for_mode(DriverMode::Continuous);
                    status.not_attached(Some(&error));
                    let _ = self.register_root(engagement, None, None, status);
                }
            }
        }
    }
    /// Publish the provisions waiting for their owner as `awaiting_owner`
    /// rows, and drop the rows of provisions that stopped waiting without
    /// being admitted. Status only: no decision reads these rows.
    fn reconcile_awaiting_owners(&self) {
        let waiting = self.provider.awaiting_owner_engagements();
        for (engagement, since) in &waiting {
            let present = self
                .routes
                .entries
                .lock()
                .map(|entries| entries.contains_key(engagement))
                .unwrap_or(true);
            if !present {
                let status = StatusHandle::for_mode(DriverMode::Continuous);
                status.awaiting_owner(*since);
                let _ = self.register_root(engagement.clone(), None, None, status);
            }
        }
        for engagement in self.routes.awaiting_rows() {
            if !waiting.iter().any(|(e, _)| *e == engagement) {
                self.routes.remove_awaiting(&engagement);
            }
        }
    }
    /// The actual service keeps discovery independent of the coordinator's
    /// potentially long inline intake. No job is replayed or reconstructed.
    pub async fn run(
        &mut self,
        notices: tokio::sync::mpsc::Sender<hagency_execution::ApprovalRequests>,
        cancel: &CancellationToken,
    ) -> Result<(), Failure> {
        if self.routes.closed.load(Ordering::Acquire) || self.started {
            return Err(Failure::Startup);
        }
        self.started = true;
        self.routes.running.store(true, Ordering::Release);
        struct Running(Arc<AtomicBool>);
        impl Drop for Running {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _running = Running(self.routes.running.clone());
        // Board #71: the idle-agent membership sweep (TS parity,
        // backend-v2.js:14192-14228 + :17511's hourly scheduling), started
        // beside the fleet loop on the shared collector's own credential.
        // The loop ends itself on the same shutdown token; the guard aborts
        // it on every exit path so a closed service leaves no task behind.
        // Never terminal: read failures retry with the retained 1 s -> 60 s
        // backoff inside the loop, so no failure here can end the service.
        struct SweepGuard(tokio::task::JoinHandle<()>);
        impl Drop for SweepGuard {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _sweep = SweepGuard(self.provider.sweep(cancel.clone()));
        self.reattach_known_agents(&notices, cancel).await;
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {biased;_ = cancel.cancelled()=>return Ok(()),_ = tick.tick()=>{}}
            self.reconcile_awaiting_owners();
            let next = self
                .provider
                .take_next()
                .map_err(|_| Failure::OutcomeUnknown);
            match next {
                Ok(Some(agent)) => {
                    self.admit(agent, notices.clone(), false).await?;
                }
                Ok(None) => {}
                Err(error) => {
                    self.routes.failed.store(true, Ordering::Release);
                    return Err(error);
                }
            }
        }
    }
    pub fn quiesce(&self) {
        self.routes.closed.store(true, Ordering::Release);
        for agent in &self.agents {
            agent.quiesce();
        }
    }
    pub(crate) async fn drain_agents(&mut self) -> Result<(), Failure> {
        self.quiesce();
        let mut failed = false;
        for agent in &mut self.agents {
            failed |= agent.close().await.is_err();
        }
        // Do not close an enrolled SDK while its original file/driver owners
        // are still unknown. All other admitted owners were drained above.
        if failed {
            self.routes.failed.store(true, Ordering::Release);
            return Err(Failure::OutcomeUnknown);
        }
        Ok(())
    }
    pub async fn close(&mut self) -> Result<(), Failure> {
        // A failed drain is reported, not obeyed (ADR-182): what a worker
        // could not prove is in the store as a fence or an unknown verdict,
        // and the SDKs close after it all the same, writing nothing (ADR-047).
        let drained = self.drain_agents().await;
        if let Some(result) = self.factory_closed {
            return drained.and(result);
        }
        if self.factory_close.is_none() {
            let provider = self.provider.clone();
            self.factory_close = Some(tokio::spawn(async move { provider.close_agents().await }));
        }
        let result = match tokio::time::timeout(
            Duration::from_secs(2),
            self.factory_close.as_mut().ok_or(Failure::OutcomeUnknown)?,
        )
        .await
        {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(_) => Err(Failure::OutcomeUnknown),
            Err(_) => return Err(Failure::OutcomeUnknown),
        };
        self.factory_close = None;
        self.factory_closed = Some(result);
        drained.and(result)
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.quiesce();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_service::{FileError, test_common};
    #[tokio::test]
    async fn native_factory_failure_diagnostics() {
        let f = test_common::Fixture::new();
        let collector =
            Arc::new(Collector::new(f.config("https://127.0.0.1:1/"), f.store.clone()).unwrap());
        let fleet = Service::new(
            f.store.clone(),
            collector.clone(),
            Setup {
                state: f.root.path().join("fleet-diagnostics"),
                limit: 128,
                send: false,
                receive: false,
                limits: hagency_execution::Limits {
                    operation_ms: 5000,
                    response_ms: 1000,
                },
            },
        )
        .unwrap();
        let failed = StatusHandle::new(true);
        let healthy = StatusHandle::new(true);
        failed.fail(Failure::OutcomeUnknown);
        healthy.phase("idle");
        fleet
            .register_root("en_failed".into(), None, None, failed.clone())
            .unwrap();
        fleet
            .register_root("en_healthy".into(), None, None, healthy.clone())
            .unwrap();
        let value = fleet.routes.snapshot();
        assert_eq!(value["failed"], true);
        assert_eq!(value["registered_backends"], 2);
        assert_eq!(value["agents"][0]["engagement_id"], "en_failed");
        assert_eq!(value["agents"][0]["status"]["error"], "outcome_unknown");
        assert_eq!(value["agents"][1]["engagement_id"], "en_healthy");
        assert_eq!(value["agents"][1]["status"]["state"], "idle");
        assert_eq!(failed.state(), "outcome_unknown");
        assert_eq!(healthy.state(), "idle");
        assert!(!fleet.routes.closed.load(Ordering::Acquire));
        healthy.phase("receiving");
        assert_eq!(
            fleet.routes.snapshot()["agents"][1]["status"]["state"],
            "receiving"
        );
        assert!(serde_json::to_vec(&value).unwrap().len() < 2048);
        // An agent with an unresolved dispatch is alive and the operator's
        // (ADR-182): it keeps reporting its failure, counts the dispatch, and
        // is not what makes a fleet failed; the agent that is gone still is.
        failed.custody(1, None);
        let value = fleet.routes.snapshot();
        assert_eq!(value["agents"][0]["status"]["error"], "outcome_unknown");
        assert_eq!(value["agents"][0]["status"]["unresolved_dispatches"], 1);
        assert!(
            value["agents"][1]["status"]
                .get("unresolved_dispatches")
                .is_none()
        );
        assert_eq!(value["failed"], false);
        assert_eq!(fleet.routes.state(), "not_started");
        assert_eq!(fleet.routes.agent_availability("en_healthy"), "unavailable");
        fleet.routes.running.store(true, Ordering::Release);
        assert_eq!(fleet.routes.agent_availability("en_healthy"), "available");
        assert_eq!(fleet.routes.agent_availability("en_failed"), "blocked");
        assert_eq!(fleet.routes.agent_availability("en_absent"), "not_attached");
        healthy.not_attached(None);
        assert_eq!(fleet.routes.agent_availability("en_healthy"), "unavailable");
        healthy.phase("receiving");
        fleet.routes.running.store(false, Ordering::Release);
        // A fenced agent is not lost either: its state word says so.
        failed.custody(0, Some("dispatch".into()));
        let value = fleet.routes.snapshot();
        assert_eq!(value["agents"][0]["status"]["state"], "fenced");
        assert_eq!(value["agents"][0]["status"]["fenced"], "dispatch");
        assert_eq!(value["failed"], false);
        let gone = StatusHandle::new(true);
        gone.fail(Failure::OutcomeUnknown);
        fleet
            .register_root("en_gone".into(), None, None, gone.clone())
            .unwrap();
        assert_eq!(fleet.routes.snapshot()["failed"], true);
        // The next attempt keeps the custody words: the driver re-reads them
        // from the store on each pass, nothing else clears them.
        failed.begin_attempt();
        assert_eq!(
            fleet.routes.snapshot()["agents"][0]["status"]["fenced"],
            "dispatch"
        );
        drop(fleet);
        collector.close().await.unwrap();
        test_common::shutdown_domain(&f.store, "factory diagnostics").await;
    }
    #[tokio::test]
    async fn native_configured_fleet_shutdown_isolation() {
        let f = test_common::Fixture::new();
        let (a, mut first, ra) =
            super::super::driver::tests::workspace_operation(&f, "service_first").await;
        let (b, mut second, rb) =
            super::super::driver::tests::workspace_operation(&f, "service_second").await;
        let collector =
            Arc::new(Collector::new(f.config("https://127.0.0.1:1/"), f.store.clone()).unwrap());
        let one = Shared {
            domain: f.store.clone(),
            collector: collector.clone(),
            workspace: WorkspaceAccess::new(),
        };
        let two = Shared {
            domain: f.store.clone(),
            collector: collector.clone(),
            workspace: WorkspaceAccess::new(),
        };
        let (binding, ack_a) = ra.into_parts();
        assert!(one.workspace.register(a.clone(), binding).await.is_ok());
        let (binding, ack_b) = rb.into_parts();
        assert!(two.workspace.register(b.clone(), binding).await.is_ok());
        let path = hagency_files::RelativeFile::new("same.txt").unwrap();
        let first_guard = one.workspace.acquire(&a).await.unwrap();
        let second_guard = two.workspace.acquire(&b).await.unwrap();
        let existing = f.root.path().join("original-missing-media");
        hagency_store::private::directory(&existing).unwrap();
        let failed = FileOwner::start(
            one.clone(),
            crate::file_service::Setup {
                directory: existing.clone(),
                namespace: "first_service".into(),
                limit: 128,
            },
        )
        .unwrap();
        // Actual existing incomplete media directory: original recovery must
        // refuse without manufacturing a replacement journal or close ACK.
        assert_eq!(failed.handle().initialize().await, Err(FileError::Unknown));
        let healthy = FileOwner::start(
            two.clone(),
            crate::file_service::Setup {
                directory: f.root.path().join("second-media"),
                namespace: "second_service".into(),
                limit: 128,
            },
        )
        .unwrap();
        let first_owner = AgentOwner {
            shared: one,
            status: StatusHandle::new(true),
            driver: None,
            files: Some(failed),
            receives: None,
            invites: None,
        };
        let second_owner = AgentOwner {
            shared: two,
            status: StatusHandle::new(true),
            driver: None,
            files: Some(healthy),
            receives: None,
            invites: None,
        };
        first_owner.quiesce();
        assert!(first_guard.validate_current().await.is_err());
        second_guard.validate_current().await.unwrap();
        assert_eq!(
            second_guard
                .snapshot(&path, 4 * 1024 * 1024)
                .unwrap()
                .bytes(),
            b"service_second"
        );
        let mut fleet = Service::new(
            f.store.clone(),
            collector.clone(),
            Setup {
                state: f.root.path().join("fleet-state"),
                limit: 128,
                send: false,
                receive: false,
                limits: hagency_execution::Limits {
                    operation_ms: 5000,
                    response_ms: 1000,
                },
            },
        )
        .unwrap();
        fleet.agents = vec![first_owner, second_owner];
        // ADR-182 decision 5: the unknown file job is reported as unknown and
        // the close goes on — every agent drained, the factory closed — so
        // the process can exit on one SIGTERM.
        assert_eq!(fleet.close().await, Err(Failure::OutcomeUnknown));
        assert_eq!(fleet.agents[0].status.state(), "outcome_unknown");
        assert_eq!(
            fleet.agents[1].status.state(),
            "closed",
            "second actual worker was acknowledged and joined despite first failure"
        );
        assert!(second_guard.validate_current().await.is_err());
        assert!(
            fleet.factory_close.is_none()
                && fleet
                    .factory_closed
                    .as_ref()
                    .is_none_or(|closed| closed.is_ok()),
            "the unknown drain no longer stops the factory close"
        );
        assert_eq!(fleet.routes.state(), "stopped");
        assert_eq!(std::fs::read_dir(existing).unwrap().count(), 0);
        drop(ack_a);
        drop(ack_b);
        for operation in [&mut first, &mut second] {
            assert_eq!(
                operation.wait().await.unwrap().protocol,
                hagency_execution::Protocol::NotStarted
            );
        }
        drop(fleet);
        collector.close().await.unwrap();
        test_common::shutdown_domain(&f.store, "original fleet drain isolation").await;
    }
}
