//! Concrete inline Host bridge to the existing owned launcher, not a launcher.
use crate::{ApprovalHost, Failure, Host, Limits, Operation, WarmLimits, WarmRuntime};
use cap_std::{ambient_authority, fs::Dir};
use hagency_store::{
    DomainStore, OwnedClaimProfile, OwnedProvisionScope, agent_home::ManagedAgentHome,
    task_context::RetainedTaskContext,
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;

/// Fixed process Host capability. No runtime configuration/proof callback,
/// serialization, cloning, private-account getter or caller-supplied readiness.
pub struct WarmHostPlan {
    guardian: PathBuf,
    /// The Codex executable; `None` on a machine that runs Claude Code only.
    executable: Option<PathBuf>,
    environment: BTreeMap<OsString, OsString>,
    bridge: WarmTaskBridge,
    approvals: ApprovalHost,
    limits: WarmLimits,
    files: Option<(usize, bool, bool)>,
    coordination: bool,
    local_codex: Option<Arc<crate::LocalBinding>>,
    claude: Option<ClaudePlan>,
    octos: Option<OctosPlan>,
}
/// ADR-192: what a Claude agent's Host is built from, beside the Codex plan.
struct ClaudePlan {
    executable: PathBuf,
    environment: BTreeMap<OsString, OsString>,
    local: Option<Arc<crate::LocalBinding>>,
}
/// ADR-193: what an Octos agent's Host is built from: the pinned `octos`
/// binary, its own allowlisted environment, the user's Octos home with the
/// profiles Hagency may run, and the private root of the agents' instance
/// directories.
struct OctosPlan {
    executable: PathBuf,
    environment: BTreeMap<OsString, OsString>,
    local: Arc<crate::LocalBinding>,
    instances: PathBuf,
}
/// The fixed native task helper, loopback origin and retained private context
/// root form one Host capability. No getters, cloning or serialization.
pub struct WarmTaskBridge {
    helper: PathBuf,
    address: SocketAddr,
    contexts: PathBuf,
    context_dir: Dir,
}
/// The actual original per-agent Host and one consumable initialized owner.
/// Drop/close has the same synchronous ownership-worker requirement as WarmRuntime.
pub struct FactoryRuntime {
    host: crate::SharedHost,
    domain: DomainStore,
    warm: Option<WarmRuntime>,
    /// The bound of the store's provision completion for an agent with no
    /// warm child (ADR-192); a warm child bounds its own.
    activation_ms: u64,
    phase: Phase,
    last: Option<String>,
    cancelled: AtomicBool,
}
enum Phase {
    Initial,
    /// A restart re-attached this agent: there is no warm child, and the first
    /// dispatch is already a follow-up from this rebuilt binding.
    Reattached(Box<crate::warm::Binding>),
    Queued(Arc<Admission>),
    Running(Arc<crate::operation::Continuation>),
    Spent,
}
struct Admission {
    capability: hagency_core::tasks::RunnerCapability,
    limits: Limits,
    binding: Option<crate::warm::Binding>,
}
/// Non-cloneable original admission. It carries no native process/worker, so
/// dropping a queued caller never joins a process on that async caller.
pub struct FactoryDispatch(Arc<Admission>);
impl WarmTaskBridge {
    pub fn new(helper: PathBuf, address: SocketAddr, contexts: PathBuf) -> Result<Self, Failure> {
        if !helper.is_absolute()
            || !helper.is_file()
            || !address.ip().is_loopback()
            || address.port() == 0
            || matches!(address,SocketAddr::V6(value) if value.scope_id()!=0 || value.flowinfo()!=0)
        {
            return Err(Failure::Admission);
        }
        hagency_store::task_context::validate_root(&contexts).map_err(|_| Failure::Admission)?;
        let context_dir = Dir::open_ambient_dir(&contexts, ambient_authority())
            .map_err(|_| Failure::Admission)?;
        Ok(Self {
            helper,
            address,
            contexts,
            context_dir,
        })
    }
}
impl WarmHostPlan {
    pub fn new(
        guardian: PathBuf,
        executable: PathBuf,
        environment: BTreeMap<OsString, OsString>,
        bridge: WarmTaskBridge,
        approvals: ApprovalHost,
        limits: WarmLimits,
    ) -> Result<Self, Failure> {
        if !limits.validate()
            || [&guardian, &executable]
                .iter()
                .any(|path| !path.is_absolute() || !path.is_file())
        {
            return Err(Failure::Admission);
        }
        Ok(Self {
            guardian,
            executable: Some(executable),
            environment,
            bridge,
            approvals,
            limits,
            files: None,
            coordination: false,
            local_codex: None,
            claude: None,
            octos: None,
        })
    }
    /// ADR-192: a plan for a machine that runs Claude Code only. Add the Claude
    /// runtime with `with_claude`; a Codex scope is refused by name.
    pub fn without_codex(
        guardian: PathBuf,
        environment: BTreeMap<OsString, OsString>,
        bridge: WarmTaskBridge,
        approvals: ApprovalHost,
        limits: WarmLimits,
    ) -> Result<Self, Failure> {
        if !limits.validate() || !guardian.is_absolute() || !guardian.is_file() {
            return Err(Failure::Admission);
        }
        Ok(Self {
            guardian,
            executable: None,
            environment,
            bridge,
            approvals,
            limits,
            files: None,
            coordination: false,
            local_codex: None,
            claude: None,
            octos: None,
        })
    }
    /// ADR-192: also run Claude Code agents, from this executable and, when the
    /// user's folder is bound, their local Claude binding. The environment is
    /// the Claude agents' own; the Codex one is untouched.
    pub fn with_claude(
        mut self,
        executable: PathBuf,
        environment: BTreeMap<OsString, OsString>,
        local: Option<crate::LocalCodex>,
    ) -> Result<Self, Failure> {
        if self.claude.is_some() || !executable.is_absolute() || !executable.is_file() {
            return Err(Failure::Admission);
        }
        let mut environment = environment;
        let local = match local {
            Some(local) => {
                if local.provider() != crate::LocalProvider::Claude {
                    return Err(Failure::Admission);
                }
                local.apply(&mut environment)?;
                Some(Arc::new(crate::LocalBinding::Folder(local)))
            }
            None => None,
        };
        self.claude = Some(ClaudePlan {
            executable,
            environment,
            local,
        });
        Ok(self)
    }
    /// ADR-193: also run Octos agents, from this pinned `octos` binary, the
    /// user's Octos home binding (required: admission reads the profile it
    /// names) and `instances`, the private root of each agent's instance
    /// directory. The environment is the Octos agents' own; the Codex one is
    /// untouched.
    pub fn with_octos(
        mut self,
        executable: PathBuf,
        environment: BTreeMap<OsString, OsString>,
        local: crate::LocalOctos,
        instances: PathBuf,
    ) -> Result<Self, Failure> {
        if self.octos.is_some()
            || !executable.is_absolute()
            || !executable.is_file()
            || !instances.is_absolute()
        {
            return Err(Failure::Admission);
        }
        let mut environment = environment;
        local.apply(&mut environment)?;
        self.octos = Some(OctosPlan {
            executable,
            environment,
            local: Arc::new(crate::LocalBinding::Octos(local)),
            instances,
        });
        Ok(self)
    }
    /// Pass the original Host's provider-owned binding without reopening it or
    /// exporting a credential/path capability. Per-agent task roots stay local.
    pub fn with_local_codex_from_host(mut self, host: &Host) -> Result<Self, Failure> {
        if self.local_codex.is_some() {
            return Err(Failure::Admission);
        }
        let local = host.local_codex.as_ref().ok_or(Failure::Admission)?;
        local.apply(&mut self.environment)?;
        self.local_codex = Some(local.clone());
        Ok(self)
    }
    /// Fixed presentation/capture limits, shared with the owning service's
    /// per-agent file backends. This grants no workspace or SDK authority.
    pub fn with_file_access(
        mut self,
        limit: usize,
        send: bool,
        receive: bool,
    ) -> Result<Self, Failure> {
        if self.files.is_some()
            || limit == 0
            || limit > hagency_core::file_delivery::MAX_FILE_BYTES as usize
            || (receive && hagency_core::received_files::receive_limit(limit).is_err())
        {
            return Err(Failure::Admission);
        }
        self.files = Some((limit, send, receive));
        Ok(self)
    }
    /// Presentation only (ADR180): every agent host may call coordination tools.
    pub fn with_coordination_tools(mut self) -> Self {
        self.coordination = true;
        self
    }
    /// The runtime of an agent a restart re-attached, from a scope and a home
    /// the store rebuilt and reopened. It starts no child and reads no retained
    /// task context: that file belongs to the original first task. The same
    /// per-agent Host is built as in `start`, its provider-managed account
    /// included (the reopened registry's own binding, gated on the same
    /// current facts), and the agent's next task launches through the
    /// ordinary follow-up binding. One runtime per scope per process.
    pub async fn reattach_runtime(
        &self,
        domain: DomainStore,
        scope: OwnedProvisionScope,
        home: Arc<ManagedAgentHome>,
    ) -> Result<FactoryRuntime, Failure> {
        match scope.resource().framework.as_str() {
            "claude" => return self.claude_runtime(domain, scope, home, false).await,
            "octos" => return self.octos_runtime(domain, scope, home, false).await,
            _ => {}
        }
        let executable = self.codex_executable()?;
        let until = Instant::now() + Duration::from_millis(self.limits.initialize.operation_ms);
        let account =
            tokio::time::timeout_at(until, domain.reattach_runtime_account(scope.clone()))
                .await
                .map_err(|_| Failure::Deadline)?
                .map_err(|error| Failure::lost(crate::AuthoritySite::FactoryAccount, &error))?;
        let guardian = self.guardian.clone();
        let environment = self.environment.clone();
        let helper = self.bridge.helper.clone();
        let address = self.bridge.address;
        let approvals = self.approvals.clone();
        let files = self.files;
        let coordination = self.coordination;
        let local_codex = self.local_codex.clone();
        let activation_ms = self.limits.initialize.response_ms;
        tokio::task::spawn_blocking(move || {
            if Instant::now() >= until {
                return Err(Failure::Deadline);
            }
            let runtime_lease = scope.claim_runtime().map_err(|_| Failure::Admission)?;
            let workspace = format!("work_{}", scope.engagement_id());
            let work = home.workdir_path().map_err(|_| Failure::Admission)?;
            let mut environment = environment;
            if let Some(local) = &local_codex {
                // Explicit provider selection, never arbitrary coordinator HOME.
                if account.is_some() {
                    return Err(Failure::Admission);
                }
                local.admit_provision(&scope)?;
                local.separate_from(&work)?;
                local.apply(&mut environment)?;
            } else {
                let agent_home = home.home_path().map_err(|_| Failure::Admission)?;
                environment.insert("HOME".into(), agent_home.clone().into_os_string());
                environment.insert("CODEX_HOME".into(), agent_home.into_os_string());
            }
            let mut host = Host::new(
                guardian,
                executable,
                environment,
                BTreeMap::from([(workspace.clone(), work)]),
            )?
            .with_task_helper(helper, address)?
            .with_approvals(approvals)?;
            if let Some((limit, send, receive)) = files {
                host = host.with_file_limit(limit)?;
                if send {
                    host = host.with_file_tools()?;
                }
                if receive {
                    host = host.with_receive_tools()?;
                }
            }
            if coordination {
                host = host.with_coordination_tools()?;
            }
            if let Some(account) = account {
                host = host.with_managed_account(account)?;
            }
            if let Some(local) = local_codex.clone() {
                host = host.with_retained_local_codex(local)?;
            }
            let root = host.reattach_root(&scope, &home, &workspace)?;
            let binding = crate::warm::Binding::reattached(
                scope,
                runtime_lease,
                home,
                root,
                workspace,
                local_codex,
            );
            Ok(FactoryRuntime {
                host: host.into_shared(),
                domain,
                warm: None,
                activation_ms,
                phase: Phase::Reattached(Box::new(binding)),
                last: None,
                cancelled: AtomicBool::new(false),
            })
        })
        .await
        .map_err(|_| Failure::Worker)?
    }
    pub async fn start(
        &self,
        domain: DomainStore,
        scope: OwnedProvisionScope,
        home: Arc<ManagedAgentHome>,
    ) -> Result<FactoryRuntime, Failure> {
        match scope.resource().framework.as_str() {
            "claude" => return self.claude_runtime(domain, scope, home, true).await,
            "octos" => return self.octos_runtime(domain, scope, home, true).await,
            _ => {}
        }
        let executable = self.codex_executable()?;
        // The original deadline includes the original writer and blocking Host
        // preparation. start_at never grants a fresh initialization interval.
        let until = Instant::now() + Duration::from_millis(self.limits.initialize.operation_ms);
        let account =
            tokio::time::timeout_at(until, domain.provision_runtime_account(scope.clone()))
                .await
                .map_err(|_| Failure::Deadline)?
                .map_err(|error| Failure::lost(crate::AuthoritySite::FactoryAccount, &error))?;
        let held = self
            .bridge
            .context_dir
            .try_clone()
            .map_err(|_| Failure::Admission)?
            .into_std_file();
        let guardian = self.guardian.clone();
        let environment = self.environment.clone();
        let helper = self.bridge.helper.clone();
        let address = self.bridge.address;
        let contexts = self.bridge.contexts.clone();
        let approvals = self.approvals.clone();
        let limits = self.limits;
        let files = self.files;
        let coordination = self.coordination;
        let local_codex = self.local_codex.clone();
        // A lost caller does not discard a possible owner on an HTTP worker:
        // this retained blocking task owns the returned WarmRuntime until handoff.
        tokio::task::spawn_blocking(move || {
            if Instant::now() >= until {
                return Err(Failure::Deadline);
            }
            let current = Dir::open_ambient_dir(&contexts, ambient_authority())
                .map_err(|_| Failure::Admission)?
                .into_std_file();
            if !hagency_platform::same_directory(&held, &current).map_err(|_| Failure::Admission)? {
                return Err(Failure::Admission);
            }
            let workspace = format!("work_{}", scope.engagement_id());
            let context_id = hagency_core::canonical::transport_digest(&serde_json::json!([
                "inline_factory",
                scope.engagement_id()
            ]))
            .map_err(|_| Failure::Admission)?;
            let context =
                RetainedTaskContext::new(contexts, &context_id).map_err(|_| Failure::Admission)?;
            let work = home.workdir_path().map_err(|_| Failure::Admission)?;
            let mut environment = environment;
            if let Some(local) = &local_codex {
                // Explicit provider selection, never arbitrary coordinator HOME.
                if account.is_some() {
                    return Err(Failure::Admission);
                }
                local.admit_provision(&scope)?;
                local.separate_from(&work)?;
                local.apply(&mut environment)?;
            } else {
                let agent_home = home.home_path().map_err(|_| Failure::Admission)?;
                environment.insert("HOME".into(), agent_home.clone().into_os_string());
                environment.insert("CODEX_HOME".into(), agent_home.into_os_string());
            }
            let mut host = Host::new(
                guardian,
                executable,
                environment,
                BTreeMap::from([(workspace.clone(), work)]),
            )?
            .with_task_helper(helper, address)?
            .with_retained_task_context(context)?
            .with_approvals(approvals)?;
            if let Some((limit, send, receive)) = files {
                host = host.with_file_limit(limit)?;
                if send {
                    host = host.with_file_tools()?;
                }
                if receive {
                    host = host.with_receive_tools()?;
                }
            }
            if coordination {
                host = host.with_coordination_tools()?;
            }
            if let Some(account) = account {
                host = host.with_managed_account(account)?;
            }
            if let Some(local) = local_codex {
                host = host.with_retained_local_codex(local)?;
            }
            let host = host.into_shared();
            let warm = WarmRuntime::start_at(
                domain.clone(),
                scope,
                home,
                host.clone(),
                workspace,
                limits,
                until,
            )?;
            Ok(FactoryRuntime {
                host,
                domain,
                warm: Some(warm),
                activation_ms: limits.initialize.response_ms,
                phase: Phase::Initial,
                last: None,
                cancelled: AtomicBool::new(false),
            })
        })
        .await
        .map_err(|_| Failure::Worker)?
    }
}
impl WarmHostPlan {
    fn codex_executable(&self) -> Result<PathBuf, Failure> {
        self.executable
            .clone()
            .ok_or_else(|| Failure::UnsupportedRunner {
                framework: "codex".into(),
            })
    }
    /// ADR-192: a Claude agent's runtime. It has no warm child before its first
    /// task: provisioning and a restart both attach it the way a restart
    /// re-attaches a Codex agent, and every dispatch starts a fresh session.
    /// `original` is provisioning: it takes the provision's original claim, as
    /// a warm start does, so the activation may complete it.
    async fn claude_runtime(
        &self,
        domain: DomainStore,
        scope: OwnedProvisionScope,
        home: Arc<ManagedAgentHome>,
        original: bool,
    ) -> Result<FactoryRuntime, Failure> {
        let claude = self
            .claude
            .as_ref()
            .ok_or_else(|| Failure::UnsupportedRunner {
                framework: "claude".into(),
            })?;
        // Managed accounts are Codex homes; a Claude agent signs in through the
        // user's own folder (ADR-192 decision 7).
        if scope.requires_managed_account() {
            return Err(Failure::Admission);
        }
        let until = Instant::now() + Duration::from_millis(self.limits.initialize.operation_ms);
        let guardian = self.guardian.clone();
        let executable = claude.executable.clone();
        let environment = claude.environment.clone();
        let local = claude.local.clone();
        let helper = self.bridge.helper.clone();
        let address = self.bridge.address;
        let approvals = self.approvals.clone();
        let files = self.files;
        let activation_ms = self.limits.initialize.response_ms;
        tokio::task::spawn_blocking(move || {
            if Instant::now() >= until {
                return Err(Failure::Deadline);
            }
            if original {
                scope.claim_warm().map_err(|_| Failure::Admission)?;
            }
            let runtime_lease = scope.claim_runtime().map_err(|_| Failure::Admission)?;
            let workspace = format!("work_{}", scope.engagement_id());
            let work = home.workdir_path().map_err(|_| Failure::Admission)?;
            let mut environment = environment;
            if let Some(local) = &local {
                local.admit_provision(&scope)?;
                local.separate_from(&work)?;
                local.apply(&mut environment)?;
            } else {
                let agent_home = home.home_path().map_err(|_| Failure::Admission)?;
                environment.insert("HOME".into(), agent_home.clone().into_os_string());
                environment.insert("CLAUDE_CONFIG_DIR".into(), agent_home.into_os_string());
            }
            let mut host = Host::new(
                guardian,
                executable,
                environment,
                BTreeMap::from([(workspace.clone(), work)]),
            )?
            .with_claude_runner()?
            .with_claude_write_guard(helper.clone())?
            .with_task_helper(helper, address)?
            .with_approvals(approvals)?;
            if let Some((limit, send, receive)) = files {
                host = host.with_file_limit(limit)?;
                if send {
                    host = host.with_file_tools()?;
                }
                if receive {
                    host = host.with_receive_tools()?;
                }
            }
            if let Some(local) = local.clone() {
                host = host.with_retained_local_codex(local)?;
            }
            let root = host.reattach_root(&scope, &home, &workspace)?;
            let binding = crate::warm::Binding::reattached(
                scope,
                runtime_lease,
                home,
                root,
                workspace,
                local,
            );
            Ok(FactoryRuntime {
                host: host.into_shared(),
                domain,
                warm: None,
                activation_ms,
                phase: Phase::Reattached(Box::new(binding)),
                last: None,
                cancelled: AtomicBool::new(false),
            })
        })
        .await
        .map_err(|_| Failure::Worker)?
    }
    /// ADR-193: an Octos agent's runtime, attached like a Claude agent's: no
    /// warm child, and every dispatch starts its own `octos serve --stdio`
    /// through the follow-up binding. Its Host has approvals but no task
    /// helper: Octos takes no MCP server per session, and its task tools are
    /// host tools on its own connection (ADR-193 decision 5). Admission reads
    /// the profile the resource names from the user's Octos home.
    async fn octos_runtime(
        &self,
        domain: DomainStore,
        scope: OwnedProvisionScope,
        home: Arc<ManagedAgentHome>,
        original: bool,
    ) -> Result<FactoryRuntime, Failure> {
        let octos = self
            .octos
            .as_ref()
            .ok_or_else(|| Failure::UnsupportedRunner {
                framework: "octos".into(),
            })?;
        // Managed accounts are Codex homes; Octos uses the keys in its own
        // store (ADR-193 operator decision 2).
        if scope.requires_managed_account() {
            return Err(Failure::Admission);
        }
        let until = Instant::now() + Duration::from_millis(self.limits.initialize.operation_ms);
        let guardian = self.guardian.clone();
        let executable = octos.executable.clone();
        let environment = octos.environment.clone();
        let local = octos.local.clone();
        let instances = octos.instances.clone();
        let helper = self.bridge.helper.clone();
        let address = self.bridge.address;
        let approvals = self.approvals.clone();
        let files = self.files;
        let activation_ms = self.limits.initialize.response_ms;
        tokio::task::spawn_blocking(move || {
            if Instant::now() >= until {
                return Err(Failure::Deadline);
            }
            if original {
                scope.claim_warm().map_err(|_| Failure::Admission)?;
            }
            let runtime_lease = scope.claim_runtime().map_err(|_| Failure::Admission)?;
            let workspace = format!("work_{}", scope.engagement_id());
            let work = home.workdir_path().map_err(|_| Failure::Admission)?;
            local.admit_provision(&scope)?;
            local.separate_from(&work)?;
            let mut environment = environment;
            local.apply(&mut environment)?;
            let mut host = Host::new(
                guardian,
                executable,
                environment,
                BTreeMap::from([(workspace.clone(), work)]),
            )?
            .with_octos_runner(instances)?
            .with_task_helper(helper, address)?
            .with_approvals(approvals)?;
            if let Some((limit, send, receive)) = files {
                host = host.with_file_limit(limit)?;
                if send {
                    host = host.with_file_tools()?;
                }
                if receive {
                    host = host.with_receive_tools()?;
                }
            }
            host = host.with_retained_local_codex(local.clone())?;
            let root = host.reattach_root(&scope, &home, &workspace)?;
            let binding = crate::warm::Binding::reattached(
                scope,
                runtime_lease,
                home,
                root,
                workspace,
                Some(local),
            );
            Ok(FactoryRuntime {
                host: host.into_shared(),
                domain,
                warm: None,
                activation_ms,
                phase: Phase::Reattached(Box::new(binding)),
                last: None,
                cancelled: AtomicBool::new(false),
            })
        })
        .await
        .map_err(|_| Failure::Worker)?
    }
}
impl FactoryRuntime {
    pub async fn ready(&mut self) -> Result<(), Failure> {
        match self.warm.as_mut() {
            Some(warm) => warm.ready().await,
            // ADR-192, ADR-193: a Claude or Octos agent has no warm child
            // before its first task; it is ready while its own root and local
            // folder check.
            None => self.attached()?.check_attached(),
        }
    }
    /// The binding of a Claude or Octos agent attached without a warm child,
    /// before its first dispatch. Anything else has no attached-only
    /// readiness.
    fn attached(&self) -> Result<&crate::warm::Binding, Failure> {
        match &self.phase {
            Phase::Reattached(binding)
                if matches!(
                    self.host.0.runner,
                    crate::host::Runner::Claude | crate::host::Runner::Octos
                ) =>
            {
                Ok(binding)
            }
            _ => Err(Failure::Admission),
        }
    }
    /// What the warm child's idle re-qualification recorded (ADR-183
    /// decision D, warm rule); `None` once the child was handed off. For the
    /// host to project; never authority.
    pub fn idle_status(&self) -> Option<crate::WarmIdleStatus> {
        self.warm.as_ref().map(WarmRuntime::idle_status)
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(warm) = &self.warm {
            warm.cancel();
        }
        if let Phase::Running(original) = &self.phase {
            original.cancel();
        }
    }
    pub async fn activate(&mut self) -> Result<hagency_core::project::Engagement, Failure> {
        if let Some(warm) = self.warm.as_mut() {
            return warm.activate().await;
        }
        // ADR-192: the store's provision completion, bracketed by the attached
        // agent's own checks as a warm child brackets it with its process's.
        let binding = self.attached()?;
        binding.check_attached()?;
        let scope = binding.scope().clone();
        let until = Instant::now() + Duration::from_millis(self.activation_ms);
        let engagement =
            tokio::time::timeout_at(until, self.domain.complete_original_provision(scope))
                .await
                .map_err(|_| Failure::Deadline)?
                .map_err(|error| Failure::lost(crate::AuthoritySite::WarmProvision, &error))?;
        self.attached()?.check_attached()?;
        Ok(engagement)
    }
    pub fn bind_claim_profile(
        &self,
        profile: OwnedClaimProfile,
    ) -> Result<OwnedClaimProfile, Failure> {
        self.host.0.bind_claim_profile(profile)
    }
    pub fn dispatch(
        &mut self,
        capability: hagency_core::tasks::RunnerCapability,
        limits: Limits,
    ) -> Result<Operation, Failure> {
        let ticket = self.reserve_dispatch(capability, limits)?;
        self.dispatch_reserved(ticket)
    }
    /// Spend original admission before queuing any possibly blocking handoff.
    /// A failed/lost queued job cannot select a replacement capability.
    pub fn reserve_dispatch(
        &mut self,
        capability: hagency_core::tasks::RunnerCapability,
        limits: Limits,
    ) -> Result<FactoryDispatch, Failure> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(Failure::Admission);
        }
        let fingerprint = hagency_core::canonical::transport_digest(&serde_json::json!(capability))
            .map_err(|_| Failure::Admission)?;
        if self.last.as_ref() == Some(&fingerprint) {
            return Err(Failure::Admission);
        }
        let binding = match &self.phase {
            Phase::Initial if self.warm.is_some() => None,
            Phase::Reattached(binding) => Some((**binding).clone()),
            Phase::Running(original) => Some(original.binding()?),
            _ => return Err(Failure::Admission),
        };
        let original = Arc::new(Admission {
            capability,
            limits,
            binding,
        });
        self.phase = Phase::Queued(original.clone());
        self.last = Some(fingerprint);
        Ok(FactoryDispatch(original))
    }
    /// Only the exact ticket from this original runtime can consume its slot.
    /// Call on the ownership worker: failed warm handoff/drop may join there.
    pub fn dispatch_reserved(&mut self, ticket: FactoryDispatch) -> Result<Operation, Failure> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(Failure::Admission);
        }
        if !matches!(&self.phase,Phase::Queued(original) if Arc::ptr_eq(original,&ticket.0)) {
            return Err(Failure::Admission);
        }
        self.phase = Phase::Spent;
        let admission = &ticket.0;
        // A refused handoff is that attempt's failure, not the agent's
        // (ADR-182 decision 2): the runtime keeps the follow-up root the
        // handoff was made from — the warm child's own binding, or the one a
        // restart rebuilt — so the next dispatch launches a follow-up, exactly
        // as after a restart, instead of finding a spent runtime.
        let (operation, fallback) = match &admission.binding {
            None => {
                let warm = self.warm.take().ok_or(Failure::Admission)?;
                let fallback = warm.binding();
                (
                    warm.dispatch_requiring_workspace(
                        admission.capability.clone(),
                        admission.limits,
                    ),
                    fallback,
                )
            }
            Some(binding) => (
                Operation::start_factory_followup(
                    self.domain.clone(),
                    admission.capability.clone(),
                    self.host.clone(),
                    admission.limits,
                    binding.clone(),
                ),
                binding.clone(),
            ),
        };
        let operation = match operation {
            Ok(operation) => operation,
            Err(failure) => {
                self.phase = Phase::Reattached(Box::new(fallback));
                return Err(failure);
            }
        };
        self.phase = Phase::Running(operation.continuation());
        Ok(operation)
    }
}
