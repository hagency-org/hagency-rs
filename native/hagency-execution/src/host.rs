use crate::workspace::{Root, Workspaces, WorktreeManager, WorktreeSpec};
use hagency_core::tasks::RunnerCapability;
use hagency_platform::Launch;
use hagency_runtime::codex::{
    session::{Settings, TASK_MCP_ENV, TaskMcp},
    transport,
};
use hagency_store::OwnedDispatchScope;
use std::sync::Arc;
use std::{collections::BTreeMap, ffi::OsString, net::SocketAddr, path::PathBuf};

/// A git worktree directory is created by `git worktree add` with default
/// permissions; the retained workspace custody gate (`workspace::Root::open` →
/// `private::check_handle`) requires a private (0700) directory owned by this
/// uid. Tighten a freshly-created worktree to 0700 before opening it, failing
/// closed on any refusal. The worktree is an untracked git checkout, so a mode
/// change does not disturb the repository.
fn make_private_dir(path: &std::path::Path) -> Result<(), crate::Failure> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| crate::Failure::Admission)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// The exact argv the host ever passes to the Codex CLI (ADR-139): one
/// argument, `app-server`. Sandbox policy and approval mode travel in the
/// typed initialize/thread request, and stdio is the pinned CLI's default
/// transport — so no `--sandbox`, `--ask-for-approval`, `--cd`, `--listen` or
/// `--stdio` flag may ever appear here. Both launch sites (the Host
/// constructor's admission validation and `Host::prepare`) build argv from
/// this one function, which `native_codex_argv_is_app_server_only` pins.
fn app_server_arguments() -> Vec<OsString> {
    vec!["app-server".into()]
}

/// Which coding agent a Host launches (ADR-192, ADR-193). Fixed when the host
/// is built: a resource of another framework is refused by name before any
/// workspace or process work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    Codex,
    Claude,
    Octos,
}
impl Runner {
    pub fn framework(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Octos => "octos",
        }
    }
}
/// The frameworks a Host can run, each refused by name on another runner.
fn native_framework(framework: &str) -> bool {
    matches!(framework, "codex" | "claude" | "octos")
}
/// A provider key never reaches Octos: it uses the keys in its own store
/// (ADR-193 decision 3, operator decision 2).
fn provider_key(key: &std::ffi::OsStr) -> bool {
    let key = key.to_string_lossy().to_ascii_uppercase();
    ["_API_KEY", "_AUTH_TOKEN", "_ACCESS_TOKEN"]
        .iter()
        .any(|suffix| key.ends_with(suffix))
}
/// The dispatch attempt's session key and turn ID (ADR-193): fresh per
/// attempt, and traceable to it. The turn ID is a canonical lowercase UUID
/// (version 8, whose bits carry the attempt's digest).
fn octos_names(
    profile: &str,
    capability: &RunnerCapability,
) -> Result<(String, String), super::Failure> {
    let digest = hagency_core::canonical::digest(&serde_json::json!([
        "octos_attempt",
        capability.dispatch_id,
        capability.fence
    ]))
    .map_err(|_| super::Failure::Admission)?;
    let hex = digest.get(..32).ok_or(super::Failure::Admission)?;
    let variant = match hex.as_bytes()[16] {
        b'0'..=b'3' => '8',
        b'4'..=b'7' => '9',
        b'8'..=b'b' => 'a',
        _ => 'b',
    };
    let turn = format!(
        "{}-{}-8{}-{variant}{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    );
    Ok((format!("{profile}:local:hagency-{hex}"), turn))
}

/// One operation uses a 100 ms..20 min absolute monotonic execution deadline.
/// Native RPC response/write waits are 10 ms..2 s. Acknowledged turn silence
/// uses the same original operation budget. Cancellation is checked every
/// 20 ms; authority is scheduled every 100 ms and its receipt may take up to 2 s.
/// Synchronous join has a conservative operation-budget-plus-30 s combined wait
/// allowance (execution plus platform startup/stop/drop and final domain receipts).
/// This bounds library waits, not OS scheduling or a stalled kernel syscall.
#[derive(Clone, Copy)]
pub struct Limits {
    pub operation_ms: u64,
    pub response_ms: u64,
}
impl Limits {
    pub fn validate(self) -> bool {
        (100..=hagency_core::tasks::MAX_OWNED_OPERATION_MS).contains(&self.operation_ms)
            && (10..=2_000).contains(&self.response_ms)
            && self.response_ms <= self.operation_ms
    }
    /// Host claim lifetime only. The short renewable lease remains independent;
    /// this never extends an existing capability or operation deadline.
    pub fn capability_ms(self) -> Result<u64, super::Failure> {
        if !self.validate() {
            return Err(super::Failure::Admission);
        }
        Ok((self.operation_ms + 30_000).max(60_000))
    }
}

/// Host configuration only: no Deserialize and no runtime/HTTP constructor.
/// Opens and retains already-private workspace roots. The caller must keep
/// their fixed paths and ancestors stable for the entire operation. Retained
/// source custody and comparison checks do not isolate hostile namespace
/// mutation, and this API must not enable a production runner catalog.
pub struct Host {
    pub(crate) runner: Runner,
    pub(crate) guardian: PathBuf,
    pub(crate) approvals: Option<crate::ApprovalHost>,
    executable: PathBuf,
    environment: BTreeMap<OsString, OsString>,
    workspaces: Workspaces,
    managed_account: Option<hagency_store::ManagedAccount>,
    /// The provider-owned local binding: the user's Codex or Claude Code
    /// folder, or their Octos home (ADR-193).
    pub(crate) local_codex: Option<Arc<crate::LocalBinding>>,
    /// ADR-193: the private root of the Octos agents' instance directories.
    octos_instances: Option<PathBuf>,
    task_helper: Option<(PathBuf, SocketAddr)>,
    pub(crate) task_context: Option<Arc<hagency_store::task_context::RetainedTaskContext>>,
    file_tools: bool,
    receive_tools: bool,
    coordination_tools: bool,
    #[cfg(test)]
    pub(crate) discard_start_reply: bool,
    #[cfg(test)]
    pub(crate) approval_fault: Option<crate::approval::Fault>,
    #[cfg(test)]
    pub(crate) approval_gate: Option<Arc<crate::approval::Gate>>,
    #[cfg(test)]
    pub(crate) discard_usage_binding_reply: bool,
    #[cfg(test)]
    pub(crate) panic_after_workspace: bool,
    // Test-double marker (ADR-053 amendment); no production builder sets it.
    pub(crate) guardian_prepare_stall: bool,
}

/// A retained immutable host configuration that may be reused for sequential
/// owned operations. Sharing does not duplicate account authority, reopen a
/// workspace root, or create a new runner configuration: every operation
/// prepares from the same originally-admitted [`Host`].
///
/// The handle intentionally exposes no access to the underlying host. It is
/// only accepted by the operation constructors that re-run all per-dispatch
/// admission and authority checks.
#[derive(Clone)]
pub struct SharedHost(pub(crate) Arc<Host>);

impl Host {
    pub(crate) fn bind_claim_profile(
        &self,
        profile: hagency_store::OwnedClaimProfile,
    ) -> Result<hagency_store::OwnedClaimProfile, super::Failure> {
        if let Some(local) = &self.local_codex {
            return local.bind_claim(profile);
        }
        match &self.managed_account {
            Some(account) => account
                .bind_claim_profile(profile)
                .map_err(|error| super::Failure::lost(super::AuthoritySite::HostClaim, &error)),
            None => Ok(profile),
        }
    }
    pub fn new(
        guardian: PathBuf,
        executable: PathBuf,
        environment: BTreeMap<OsString, OsString>,
        workspaces: BTreeMap<String, PathBuf>,
    ) -> Result<Self, super::Failure> {
        if !guardian.is_absolute()
            || !executable.is_absolute()
            || workspaces.is_empty()
            || workspaces.len() > 16
            || environment.keys().any(|key| {
                key.to_string_lossy()
                    .eq_ignore_ascii_case(TaskMcp::FILE_TOOLS_ENV)
                    || key
                        .to_string_lossy()
                        .eq_ignore_ascii_case(TaskMcp::RECEIVE_TOOLS_ENV)
            })
        {
            return Err(super::Failure::Admission);
        }
        let workspaces = Workspaces::open(workspaces)?;
        // Reuse the launch validator for exact environment/argv byte limits.
        Launch {
            executable: executable.clone(),
            arguments: app_server_arguments(),
            directory: workspaces.first_path()?.to_path_buf(),
            environment: environment.clone(),
            require_crash_containment: false,
        }
        .validate()
        .map_err(|_| super::Failure::Admission)?;
        Ok(Self {
            runner: Runner::Codex,
            guardian,
            approvals: None,
            executable,
            environment,
            workspaces,
            managed_account: None,
            local_codex: None,
            octos_instances: None,
            task_helper: None,
            task_context: None,
            file_tools: false,
            receive_tools: false,
            coordination_tools: false,
            #[cfg(test)]
            discard_start_reply: false,
            #[cfg(test)]
            approval_fault: None,
            #[cfg(test)]
            approval_gate: None,
            #[cfg(test)]
            discard_usage_binding_reply: false,
            #[cfg(test)]
            panic_after_workspace: false,
            guardian_prepare_stall: false,
        })
    }
    /// Consume the original registry binding. Managed scope cannot fall through
    /// to the fixed development HOME, and another account cannot replace it.
    pub fn with_managed_account(
        mut self,
        account: hagency_store::ManagedAccount,
    ) -> Result<Self, super::Failure> {
        // Managed accounts are Codex homes; a Claude host signs in through the
        // user's own folder only (ADR-192 decision 7), and Octos uses the keys
        // in its own store (ADR-193).
        if self.managed_account.is_some()
            || self.local_codex.is_some()
            || self.runner != Runner::Codex
        {
            return Err(super::Failure::Admission);
        }
        self.managed_account = Some(account);
        Ok(self)
    }
    /// Explicit provider-owned local login, separate from managed readiness.
    pub fn with_local_codex(self, local: crate::LocalCodex) -> Result<Self, super::Failure> {
        self.with_retained_local_codex(Arc::new(crate::LocalBinding::Folder(local)))
    }
    pub(crate) fn with_retained_local_codex(
        mut self,
        local: Arc<crate::LocalBinding>,
    ) -> Result<Self, super::Failure> {
        // The binding names the runner's own agent folder: select the runner
        // first (`with_claude_runner`, `with_octos_runner`), then bind its
        // folder.
        if self.managed_account.is_some()
            || self.local_codex.is_some()
            || !local.serves(self.runner)
        {
            return Err(super::Failure::Admission);
        }
        local.apply(&mut self.environment)?;
        self.local_codex = Some(local);
        Ok(self)
    }
    /// Launch Claude Code instead of Codex (ADR-192). Select it before binding
    /// a local folder; a managed account (Codex homes only) refuses it.
    pub fn with_claude_runner(mut self) -> Result<Self, super::Failure> {
        if self.runner != Runner::Codex
            || self.managed_account.is_some()
            || self.local_codex.is_some()
        {
            return Err(super::Failure::Admission);
        }
        self.runner = Runner::Claude;
        Ok(self)
    }
    /// Launch Octos instead of Codex (ADR-193): one `octos serve --stdio` per
    /// dispatch. `instances` is the private root, created when absent, that
    /// holds each Octos agent's own instance directory (its serve lock and
    /// stores). A managed account or a local folder binding refuses it.
    pub fn with_octos_runner(mut self, instances: PathBuf) -> Result<Self, super::Failure> {
        if self.runner != Runner::Codex
            || self.managed_account.is_some()
            || self.local_codex.is_some()
            || !instances.is_absolute()
            || instances.as_os_str().len() > 512
        {
            return Err(super::Failure::Admission);
        }
        hagency_store::private::directory(&instances).map_err(|_| super::Failure::Admission)?;
        self.runner = Runner::Octos;
        self.octos_instances = Some(instances);
        Ok(self)
    }
    pub fn runner(&self) -> Runner {
        self.runner
    }
    /// Host-selected native executable and literal loopback endpoint only. The
    /// task/capability are supplied later from the validated owned dispatch.
    /// The host must protect the executable, workspace and Codex config/home;
    /// executable path checks are not executable custody, and retained source
    /// roots do not establish stable namespace provisioning.
    pub fn with_approvals(
        mut self,
        approvals: crate::ApprovalHost,
    ) -> Result<Self, super::Failure> {
        if self.approvals.is_some() {
            return Err(super::Failure::Admission);
        }
        self.approvals = Some(approvals);
        Ok(self)
    }
    pub fn with_task_helper(
        mut self,
        executable: PathBuf,
        address: SocketAddr,
    ) -> Result<Self, super::Failure> {
        if !address.ip().is_loopback()
            || address.port() == 0
            || matches!(address, SocketAddr::V6(v) if v.scope_id()!=0 || v.flowinfo()!=0)
            || !executable.is_file()
            || self.environment.keys().any(|key| {
                TASK_MCP_ENV
                    .iter()
                    .any(|reserved| key.to_string_lossy().eq_ignore_ascii_case(reserved))
            })
        {
            return Err(super::Failure::Admission);
        }
        TaskMcp::new(
            executable.clone(),
            "validation_only".into(),
            self.system_root()?,
        )
        .map_err(|_| super::Failure::Admission)?;
        self.task_helper = Some((executable, address));
        Ok(self)
    }
    /// Explicit one-shot task-context bridge for the same initialized owner.
    /// It does not permit pre-Started spawn or create a runtime readiness fact.
    pub fn with_retained_task_context(
        mut self,
        context: Arc<hagency_store::task_context::RetainedTaskContext>,
    ) -> Result<Self, super::Failure> {
        if self.task_helper.is_none() || self.task_context.is_some() {
            return Err(super::Failure::Admission);
        }
        let mut environment = BTreeMap::new();
        context
            .apply_environment(&mut environment)
            .map_err(|_| super::Failure::Admission)?;
        self.task_context = Some(context);
        Ok(self)
    }
    /// Select a smaller copy profile before starting an operation. Source
    /// handles are duplicated from the same retained roots, never reopened.
    pub fn with_file_limit(mut self, max_bytes: usize) -> Result<Self, super::Failure> {
        self.workspaces = self.workspaces.limit(max_bytes)?;
        Ok(self)
    }
    /// Fixed development presentation opt-in, after configuring the required
    /// helper. File admission still requires the service's original authority.
    pub fn with_file_tools(mut self) -> Result<Self, super::Failure> {
        if self.task_helper.is_none() {
            return Err(super::Failure::Admission);
        }
        self.file_tools = true;
        Ok(self)
    }
    /// Test double only (ADR-053 amendment): delay the blocking spawn thread
    /// past the granted operation budget, so the bounded-spawn expiry is
    /// exercised deterministically and a REAL late child still appears through
    /// the custody handoff. Compiled unconditionally so the integration
    /// selector can reach it (a `#[cfg(test)]` double is invisible to
    /// integration tests); no production caller sets it, and the stall itself
    /// is a multiple of the granted operation budget — never a bare literal.
    pub fn with_guardian_prepare_stall(mut self) -> Self {
        self.guardian_prepare_stall = true;
        self
    }
    /// Presentation only (ADR180): lets the owned runtime call the coordination
    /// tools its helper already serves. Project and fleet authority stay in the
    /// service behind the runner capability.
    pub fn with_coordination_tools(mut self) -> Result<Self, super::Failure> {
        if self.task_helper.is_none() {
            return Err(super::Failure::Admission);
        }
        self.coordination_tools = true;
        Ok(self)
    }
    /// Presentation only, after configuring the original native task helper.
    pub fn with_receive_tools(mut self) -> Result<Self, super::Failure> {
        if self.task_helper.is_none() {
            return Err(super::Failure::Admission);
        }
        self.receive_tools = true;
        Ok(self)
    }
    /// Retain this exact admitted host for more than one owned operation.
    /// Per-operation state remains in [`crate::Operation`]; this only shares
    /// the immutable configuration and retained workspace/account handles.
    pub fn into_shared(self) -> SharedHost {
        SharedHost(Arc::new(self))
    }
    fn system_root(&self) -> Result<Option<String>, super::Failure> {
        let mut roots = self
            .environment
            .iter()
            .filter(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("SystemRoot"));
        let first = roots
            .next()
            .map(|(_, v)| {
                v.to_str()
                    .map(str::to_owned)
                    .ok_or(super::Failure::Admission)
            })
            .transpose()?;
        if roots.next().is_some() {
            return Err(super::Failure::Admission);
        }
        Ok(first)
    }
    pub(crate) fn task_helper_enabled(&self) -> bool {
        self.task_helper.is_some()
    }
    pub(crate) fn prepare(
        &self,
        scope: &OwnedDispatchScope,
        capability: &RunnerCapability,
        limits: Limits,
    ) -> Result<Prepared, super::Failure> {
        self.prepare_bound(scope, capability, limits, self.task_context.as_ref())
    }
    /// A positively retired factory operation may start distinct work through
    /// the ordinary direct binding. No initial warm context is reused or changed.
    pub(crate) fn prepare_followup(
        &self,
        scope: &OwnedDispatchScope,
        capability: &RunnerCapability,
        limits: Limits,
    ) -> Result<Prepared, super::Failure> {
        self.prepare_bound(scope, capability, limits, None)
    }
    fn prepare_bound(
        &self,
        scope: &OwnedDispatchScope,
        capability: &RunnerCapability,
        limits: Limits,
        task_context: Option<&Arc<hagency_store::task_context::RetainedTaskContext>>,
    ) -> Result<Prepared, super::Failure> {
        // ADR-142, ADR-192: a dispatch naming the other coding agent than this
        // host's runner is refused by name before any workspace is resolved,
        // custody-checked or process spawned. Hoisted above the resource slice
        // and the workspace get/check so "no process and no workspace work" is
        // literal, and distinct from the generic Admission refusal below.
        let framework = scope.resource().framework.as_str();
        if framework != self.runner.framework() && native_framework(framework) {
            return Err(super::Failure::UnsupportedRunner {
                framework: framework.to_owned(),
            });
        }
        if let Some(local) = &self.local_codex {
            local.admit(scope)?;
        }
        let [workspace] = scope.input().resources.as_slice() else {
            return Err(super::Failure::Admission);
        };
        if !workspace.exclusive || !limits.validate() {
            return Err(super::Failure::Admission);
        }
        // ADR-011 (board #78, TS backend-v2.js:2057-2075): a dispatch whose
        // AGENT record enables `worktree` mode and whose session has a thread
        // root resolves to its per-thread worktree. The repository is the
        // agent's own workspace root — the shared Root this dispatch already
        // holds (backend-v2.js:2057 `agent.workdir || agent.homeDir`); a
        // missing `worktrees_dir` is the TS 'workspace-unavailable' refusal
        // (backend-v2.js:2008). Anything else keeps the shared workspace.
        // Failure to resolve parks/refuses (Admission), never a terminal state.
        let shared_root = self.workspaces.get(&workspace.id)?;
        let root = match (scope.workspace_mode(), scope.thread_root()) {
            ("worktree", Some(thread)) => {
                let Some(worktrees_dir) = scope.worktrees_dir() else {
                    return Err(super::Failure::Admission);
                };
                let spec = WorktreeSpec {
                    repository_path: shared_root.path().to_string_lossy().into_owned(),
                    worktrees_dir: worktrees_dir.to_string(),
                    agent_id: scope.engagement_id().to_string(),
                    thread_root_event_id: thread.to_string(),
                    bootstrap: (!scope.worktree_bootstrap().is_empty())
                        .then(|| scope.worktree_bootstrap().to_vec()),
                };
                let info = WorktreeManager::new()
                    .ensure(spec)
                    .map_err(|_| super::Failure::Admission)?;
                // `git worktree add` creates a directory with default
                // permissions; the retained workspace custody gate requires a
                // private (0700) directory owned by this uid. Tighten it before
                // opening the root; fail closed on any refusal.
                make_private_dir(info.path.as_path())?;
                Arc::new(Root::open(info.path)?)
            }
            _ => shared_root,
        };
        root.check().map_err(|_| super::Failure::Admission)?;
        // ADR-116 amendment: the session and process working directory string
        // is the ordinary projection of the retained root. On Windows the
        // canonical root is a verbatim `\\?\` path; a child launched there
        // reports that form as its callback cwd, which the untrusted path
        // parser refuses, so no owner grant could ever gain a reusable scope.
        // Directory custody still comes from the retained handle checked above;
        // the projection refuses device namespaces and ambiguous aliases.
        let path = std::path::PathBuf::from(root.approval_path()?);
        if let Some(local) = &self.local_codex {
            local.separate_from(root.path())?;
        }
        if self.runner == Runner::Claude {
            return self.prepare_claude(scope, capability, limits, task_context, path, root);
        }
        if self.runner == Runner::Octos {
            return self.prepare_octos(scope, capability, limits, path, root);
        }
        let resource = scope.resource();
        if resource.framework != "codex"
            || resource.provider.as_deref().is_some_and(|v| v != "openai")
        {
            return Err(super::Failure::Admission);
        }
        let effort = resource.reasoning.as_deref().unwrap_or("medium");
        if !["none", "minimal", "low", "medium", "high", "xhigh"].contains(&effort) {
            return Err(super::Failure::Admission);
        }
        let mut settings = Settings::new(path.clone(), resource.model.clone(), effort.into())
            .map_err(|_| super::Failure::Admission)?;
        // Every byte comes from the immutable dispatch payload. This informational
        // text grants neither task maintenance, process authority nor approval.
        let input = hagency_core::canonical::encode_payload(&scope.input().payload)
            .map_err(|_| super::Failure::Admission)?;
        if input.len() > hagency_runtime::codex::session::MAX_TEXT_BYTES {
            return Err(super::Failure::Admission);
        }
        let mut environment = self.environment.clone();
        let mut late_helper = None;
        let account = match &self.managed_account {
            Some(account) => {
                let launch = account
                    .prepare_launch(scope)
                    .map_err(|_| super::Failure::Admission)?;
                launch
                    .apply_codex_environment(&mut environment)
                    .map_err(|_| super::Failure::Admission)?;
                Some(launch)
            }
            None if scope.requires_managed_account() => return Err(super::Failure::Admission),
            None => None,
        };
        if let Some(local) = &self.local_codex {
            local.apply(&mut environment)?;
        }
        if let Some((executable, address)) = &self.task_helper {
            let mut helper = TaskMcp::new(
                executable.clone(),
                scope.task().id.clone(),
                self.system_root()?,
            )
            .map_err(|_| super::Failure::Admission)?;
            environment.insert(TASK_MCP_ENV[0].into(), address.to_string().into());
            if let Some(context) = task_context {
                context
                    .separate_from(&path)
                    .map_err(|_| super::Failure::Admission)?;
                context
                    .apply_environment(&mut environment)
                    .map_err(|_| super::Failure::Admission)?;
            } else {
                let encoded =
                    serde_json::to_string(capability).map_err(|_| super::Failure::Admission)?;
                if encoded.len() > 4096 {
                    return Err(super::Failure::Admission);
                }
                environment.insert(TASK_MCP_ENV[1].into(), encoded.into());
                environment.insert(TASK_MCP_ENV[2].into(), scope.task().id.clone().into());
            }
            if self.file_tools {
                environment.insert(TaskMcp::FILE_TOOLS_ENV.into(), "1".into());
                helper = helper.with_file_tools();
            }
            if self.receive_tools {
                environment.insert(TaskMcp::RECEIVE_TOOLS_ENV.into(), "1".into());
                helper = helper.with_receive_tools();
            }
            if self.coordination_tools {
                helper = helper.with_coordination_tools();
            }
            if task_context.is_some() {
                late_helper = Some(helper);
            } else {
                settings = settings.with_task_mcp(helper);
            }
        }
        // ADR-139: argv is deliberately just "app-server" — sandbox and
        // approval travel in the typed initialize request, and stdio is the
        // pinned CLI's default transport (see adr-139-native-codex-launch-surface.md).
        // Both launch sites build argv from `app_server_arguments`, so
        // `native_codex_argv_is_app_server_only` pins every argument the
        // host can ever pass to the Codex CLI.
        let launch = self.launch(path.clone(), environment)?;
        Ok(Prepared {
            launch,
            io_limits: transport::Limits {
                write_timeout_ms: limits.response_ms,
                // Tool execution need not produce unsolicited events within
                // an RPC response interval. Fixed lifetime and the original
                // operation deadline still bound every quiet turn.
                event_wait_ms: limits.operation_ms,
                lifetime_ms: limits.operation_ms,
            },
            root,
            account,
            runner: PreparedRunner::Codex {
                settings,
                input,
                late_helper,
            },
        })
    }
    /// ADR-192: the Claude half of `prepare_bound`, after the shared workspace
    /// and custody checks. Fixed task profile (ADR-158), the local Claude
    /// folder's environment and the same task-helper inheritance as Codex.
    fn prepare_claude(
        &self,
        scope: &OwnedDispatchScope,
        capability: &RunnerCapability,
        limits: Limits,
        task_context: Option<&Arc<hagency_store::task_context::RetainedTaskContext>>,
        path: PathBuf,
        root: Arc<Root>,
    ) -> Result<Prepared, super::Failure> {
        let resource = scope.resource();
        // Claude resources carry no reasoning setting (ADR-192 decision 8), and
        // managed accounts are Codex homes.
        if resource.framework != "claude"
            || resource
                .provider
                .as_deref()
                .is_some_and(|v| v != "anthropic")
            || resource.reasoning.is_some()
            || scope.requires_managed_account()
        {
            return Err(super::Failure::Admission);
        }
        // Every byte comes from the immutable dispatch payload. This informational
        // text grants neither task maintenance, process authority nor approval.
        let prompt = hagency_core::canonical::encode_payload(&scope.input().payload)
            .map_err(|_| super::Failure::Admission)?;
        if prompt.is_empty() || prompt.len() > hagency_runtime::claude::MAX_TEXT_BYTES {
            return Err(super::Failure::Admission);
        }
        let mut environment = self.environment.clone();
        if let Some(local) = &self.local_codex {
            local.apply(&mut environment)?;
        }
        let helper = match &self.task_helper {
            Some((executable, address)) => {
                let mut helper = hagency_runtime::claude::TaskMcp::new(
                    executable.clone(),
                    scope.task().id.clone(),
                )
                .map_err(|_| super::Failure::Admission)?;
                environment.insert(TASK_MCP_ENV[0].into(), address.to_string().into());
                if let Some(context) = task_context {
                    context
                        .separate_from(&path)
                        .map_err(|_| super::Failure::Admission)?;
                    context
                        .apply_environment(&mut environment)
                        .map_err(|_| super::Failure::Admission)?;
                } else {
                    let encoded =
                        serde_json::to_string(capability).map_err(|_| super::Failure::Admission)?;
                    if encoded.len() > 4096 {
                        return Err(super::Failure::Admission);
                    }
                    environment.insert(TASK_MCP_ENV[1].into(), encoded.into());
                    environment.insert(TASK_MCP_ENV[2].into(), scope.task().id.clone().into());
                }
                if self.file_tools {
                    environment.insert(TaskMcp::FILE_TOOLS_ENV.into(), "1".into());
                    helper = helper.with_file_tools();
                }
                if self.receive_tools {
                    environment.insert(TaskMcp::RECEIVE_TOOLS_ENV.into(), "1".into());
                    helper = helper.with_receive_tools();
                }
                // Coordination tools reach other sessions and have no Claude
                // profile yet; this host does not offer them to Claude.
                Some(helper)
            }
            None => None,
        };
        // Every owned dispatch holds its exclusive workspace lease (checked in
        // `prepare_bound`), so the TS rule selects `auto`, never `plan`.
        let arguments = hagency_runtime::claude::task_arguments(&resource.model, true)
            .map_err(|_| super::Failure::Admission)?
            .into_iter()
            .map(OsString::from)
            .collect();
        let launch = Launch {
            executable: self.executable.clone(),
            arguments,
            directory: path,
            environment,
            require_crash_containment: false,
        };
        launch.validate().map_err(|_| super::Failure::Admission)?;
        Ok(Prepared {
            launch,
            io_limits: transport::Limits {
                write_timeout_ms: limits.response_ms,
                event_wait_ms: limits.operation_ms,
                lifetime_ms: limits.operation_ms,
            },
            root,
            account: None,
            runner: PreparedRunner::Claude {
                prompt,
                helper,
                retained: task_context.is_some(),
            },
        })
    }
    /// ADR-193: the Octos half of `prepare_bound`, after the shared workspace
    /// and custody checks. One `octos serve --stdio` on the dispatch's
    /// workspace with the agent's own instance directory, network denied. The
    /// resource names the profile it runs; no provider key reaches Octos.
    fn prepare_octos(
        &self,
        scope: &OwnedDispatchScope,
        capability: &RunnerCapability,
        limits: Limits,
        path: PathBuf,
        root: Arc<Root>,
    ) -> Result<Prepared, super::Failure> {
        let resource = scope.resource();
        let profile = resource
            .octos_profile
            .clone()
            .ok_or(super::Failure::Admission)?;
        if resource.framework != "octos"
            || resource.reasoning.is_some()
            || scope.requires_managed_account()
            || !hagency_runtime::octos::profile_id(&profile)
        {
            return Err(super::Failure::Admission);
        }
        // Every byte comes from the immutable dispatch payload. This informational
        // text grants neither task maintenance, process authority nor approval.
        let prompt = hagency_core::canonical::encode_payload(&scope.input().payload)
            .map_err(|_| super::Failure::Admission)?;
        if prompt.is_empty() || prompt.len() > hagency_runtime::octos::MAX_TEXT_BYTES {
            return Err(super::Failure::Admission);
        }
        let instances = self
            .octos_instances
            .as_ref()
            .ok_or(super::Failure::Admission)?;
        // One short private directory per agent: one serve at a time holds it.
        let agent = hagency_core::canonical::digest(&serde_json::json!([
            "octos_instance",
            scope.engagement_id()
        ]))
        .map_err(|_| super::Failure::Admission)?;
        let instance = instances.join(agent.get(..16).ok_or(super::Failure::Admission)?);
        hagency_store::private::directory(&instance).map_err(|_| super::Failure::Admission)?;
        // Hagency's own settings (decision 3): Octos reads neither a project's
        // `.octos/config.json` nor the user's config.
        let config = instance.join("hagency-octos-config.json");
        hagency_store::private::replace(&config, hagency_runtime::octos::CONFIG)
            .map_err(|_| super::Failure::Admission)?;
        // The one file Octos writes into a writable workspace stays out of Git.
        root.exclude_from_git(&format!("/{}", hagency_runtime::octos::WORKSPACE_POLICY))?;
        let arguments = hagency_runtime::octos::serve_arguments(
            path.to_str().ok_or(super::Failure::Admission)?,
            instance.to_str().ok_or(super::Failure::Admission)?,
            config.to_str().ok_or(super::Failure::Admission)?,
        )
        .map_err(|_| super::Failure::Admission)?
        .into_iter()
        .map(OsString::from)
        .collect();
        // ADR-193 decision 5: the task tools are Hagency's host tools, served
        // by the same scoped helper as for Codex and Claude Code. The helper
        // is Hagency's own child: the capability never reaches Octos.
        let (executable, address) = self.task_helper.as_ref().ok_or(super::Failure::Admission)?;
        let mut task_tools = hagency_runtime::octos::task_tools::TaskTools::new(
            executable.clone(),
            scope.task().id.clone(),
        )
        .map_err(|_| super::Failure::Admission)?;
        let encoded = serde_json::to_string(capability).map_err(|_| super::Failure::Admission)?;
        if encoded.len() > 4096 {
            return Err(super::Failure::Admission);
        }
        let mut helper_environment = BTreeMap::from([
            (
                OsString::from(TASK_MCP_ENV[0]),
                OsString::from(address.to_string()),
            ),
            (OsString::from(TASK_MCP_ENV[1]), OsString::from(encoded)),
            (
                OsString::from(TASK_MCP_ENV[2]),
                OsString::from(scope.task().id.clone()),
            ),
        ]);
        if self.file_tools {
            helper_environment.insert(TaskMcp::FILE_TOOLS_ENV.into(), "1".into());
            task_tools = task_tools.with_file_tools();
        }
        if self.receive_tools {
            helper_environment.insert(TaskMcp::RECEIVE_TOOLS_ENV.into(), "1".into());
            task_tools = task_tools.with_receive_tools();
        }
        let prompt = task_tools.prompt(&prompt);
        if prompt.len() > hagency_runtime::octos::MAX_TEXT_BYTES {
            return Err(super::Failure::Admission);
        }
        let tools = crate::octos_tools::Spec {
            executable: executable.clone(),
            environment: helper_environment,
            tools: task_tools.tools(),
        };
        let mut environment = self.environment.clone();
        environment.retain(|key, _| !provider_key(key));
        // First use would otherwise download a 334 MB embedding model.
        environment.insert("OCTOS_NO_MODEL_DOWNLOAD".into(), "1".into());
        let launch = Launch {
            executable: self.executable.clone(),
            arguments,
            directory: path.clone(),
            environment,
            require_crash_containment: false,
        };
        launch.validate().map_err(|_| super::Failure::Admission)?;
        let (session, turn) = octos_names(&profile, capability)?;
        Ok(Prepared {
            launch,
            io_limits: transport::Limits {
                write_timeout_ms: limits.response_ms,
                event_wait_ms: limits.operation_ms,
                lifetime_ms: limits.operation_ms,
            },
            root,
            account: None,
            runner: PreparedRunner::Octos {
                prompt,
                profile,
                session,
                turn,
                workspace: path.to_str().ok_or(super::Failure::Admission)?.to_owned(),
                tools,
            },
        })
    }
    fn launch(
        &self,
        path: std::path::PathBuf,
        environment: BTreeMap<OsString, OsString>,
    ) -> Result<Launch, super::Failure> {
        let launch = Launch {
            executable: self.executable.clone(),
            arguments: app_server_arguments(),
            directory: path,
            environment,
            require_crash_containment: false,
        };
        launch.validate().map_err(|_| super::Failure::Admission)?;
        Ok(launch)
    }
    /// The checks `prepare_warm` makes on the scope, the home and the workspace
    /// root, without preparing a launch: a re-attached agent starts no warm
    /// child, its next task launches through the ordinary follow-up binding.
    pub(crate) fn reattach_root(
        &self,
        scope: &hagency_store::OwnedProvisionScope,
        home: &hagency_store::agent_home::ManagedAgentHome,
        workspace_id: &str,
    ) -> Result<Arc<crate::workspace::Root>, super::Failure> {
        // Every runner has the task helper. An Octos agent reaches it through
        // host tools on its own OUP connection (ADR-193 decision 5).
        if self.task_helper.is_none() || self.task_context.is_some() {
            return Err(super::Failure::Admission);
        }
        let resource = scope.resource();
        // ADR-192: the agent's own runner, by name; then its provider.
        if resource.framework != self.runner.framework() && native_framework(&resource.framework) {
            return Err(super::Failure::UnsupportedRunner {
                framework: resource.framework.clone(),
            });
        }
        // An Octos profile names its own provider (ADR-193 decision 7).
        let provider = match self.runner {
            Runner::Codex => Some("openai"),
            Runner::Claude => Some("anthropic"),
            Runner::Octos => None,
        };
        if resource.framework != self.runner.framework()
            || provider
                .is_some_and(|provider| resource.provider.as_deref().is_some_and(|v| v != provider))
            || (self.runner != Runner::Codex && resource.reasoning.is_some())
            || (self.runner == Runner::Octos && resource.octos_profile.is_none())
        {
            return Err(super::Failure::Admission);
        }
        home.check_provision_scope(scope)
            .map_err(|_| super::Failure::Admission)?;
        let root = self.workspaces.get(workspace_id)?;
        root.check().map_err(|_| super::Failure::Admission)?;
        if root.path()
            != home
                .workdir_path()
                .map_err(|_| super::Failure::Admission)?
                .as_path()
        {
            return Err(super::Failure::Admission);
        }
        if let Some(local) = &self.local_codex {
            local.admit_provision(scope)?;
            local.separate_from(root.path())?;
        }
        Ok(root)
    }
    pub(crate) fn prepare_warm(
        &self,
        scope: &hagency_store::OwnedProvisionScope,
        home: &hagency_store::agent_home::ManagedAgentHome,
        workspace_id: &str,
        limits: Limits,
    ) -> Result<Prepared, super::Failure> {
        if !limits.validate() || self.task_helper.is_none() || self.task_context.is_none() {
            return Err(super::Failure::Admission);
        }
        let resource = scope.resource();
        // A warm child is an app server; Claude and Octos agents start their
        // session on demand until a ready-ahead session lands (ADR-192, ADR-193).
        if matches!(resource.framework.as_str(), "claude" | "octos") || self.runner != Runner::Codex
        {
            return Err(super::Failure::UnsupportedRunner {
                framework: resource.framework.clone(),
            });
        }
        if resource.framework != "codex"
            || resource.provider.as_deref().is_some_and(|v| v != "openai")
        {
            return Err(super::Failure::Admission);
        }
        home.check_provision_scope(scope)
            .map_err(|_| super::Failure::Admission)?;
        let root = self.workspaces.get(workspace_id)?;
        root.check().map_err(|_| super::Failure::Admission)?;
        if root.path()
            != home
                .workdir_path()
                .map_err(|_| super::Failure::Admission)?
                .as_path()
        {
            return Err(super::Failure::Admission);
        }
        let path = std::path::PathBuf::from(root.approval_path()?);
        let effort = resource.reasoning.as_deref().unwrap_or("medium");
        if !["none", "minimal", "low", "medium", "high", "xhigh"].contains(&effort) {
            return Err(super::Failure::Admission);
        }
        let settings = Settings::new(path.clone(), resource.model.clone(), effort.into())
            .map_err(|_| super::Failure::Admission)?;
        let mut environment = self.environment.clone();
        if let Some(local) = &self.local_codex {
            local.admit_provision(scope)?;
            local.separate_from(root.path())?;
            local.apply(&mut environment)?;
        }
        let account = match &self.managed_account {
            Some(account) => {
                let launch = account
                    .prepare_provision_launch(scope)
                    .map_err(|_| super::Failure::Admission)?;
                launch
                    .apply_codex_environment(&mut environment)
                    .map_err(|_| super::Failure::Admission)?;
                Some(launch)
            }
            None if scope.requires_managed_account() => return Err(super::Failure::Admission),
            None => None,
        };
        let (_, address) = self.task_helper.as_ref().ok_or(super::Failure::Admission)?;
        environment.insert(TASK_MCP_ENV[0].into(), address.to_string().into());
        let context = self
            .task_context
            .as_ref()
            .ok_or(super::Failure::Admission)?;
        context
            .separate_from(&path)
            .map_err(|_| super::Failure::Admission)?;
        context
            .apply_environment(&mut environment)
            .map_err(|_| super::Failure::Admission)?;
        if self.file_tools {
            environment.insert(TaskMcp::FILE_TOOLS_ENV.into(), "1".into());
        }
        if self.receive_tools {
            environment.insert(TaskMcp::RECEIVE_TOOLS_ENV.into(), "1".into());
        }
        Ok(Prepared {
            launch: self.launch(path, environment)?,
            io_limits: transport::Limits {
                write_timeout_ms: limits.response_ms,
                event_wait_ms: limits.operation_ms,
                lifetime_ms: limits.operation_ms,
            },
            root,
            account,
            runner: PreparedRunner::Codex {
                settings,
                input: String::new(),
                late_helper: None,
            },
        })
    }
}

pub(crate) struct Prepared {
    pub(crate) launch: Launch,
    pub(crate) io_limits: transport::Limits,
    pub(crate) root: Arc<Root>,
    pub(crate) account: Option<hagency_store::ManagedLaunch>,
    pub(crate) runner: PreparedRunner,
}
/// The runner-specific half of a prepared launch (ADR-192).
pub(crate) enum PreparedRunner {
    Codex {
        settings: Settings,
        input: String,
        late_helper: Option<TaskMcp>,
    },
    /// The prompt is the dispatch payload. The scoped helper is bound after
    /// initialize (ADR-158), through the retained context when there is one.
    Claude {
        prompt: String,
        helper: Option<hagency_runtime::claude::TaskMcp>,
        retained: bool,
    },
    /// ADR-193: the profile the resource names, this attempt's fresh session
    /// key and turn ID, and the workspace Octos must bind.
    Octos {
        prompt: String,
        profile: String,
        session: String,
        turn: String,
        workspace: String,
        /// The task helper this dispatch's host tools run on (decision 5).
        tools: crate::octos_tools::Spec,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_owned_long_operation_limits() {
        use hagency_core::tasks::{MAX_OWNED_CAPABILITY_MS, MAX_OWNED_OPERATION_MS};
        assert_eq!(
            MAX_OWNED_OPERATION_MS,
            hagency_runtime::codex::MAX_REQUEST_MS
        );
        for (operation_ms, capability_ms) in [
            (100, 60_000),
            (30_000, 60_000),
            (45_000, 75_000),
            (MAX_OWNED_OPERATION_MS, MAX_OWNED_CAPABILITY_MS),
        ] {
            let limits = Limits {
                operation_ms,
                response_ms: 100,
            };
            assert!(limits.validate());
            assert_eq!(limits.capability_ms().unwrap(), capability_ms);
        }
        for operation_ms in [0, 99, MAX_OWNED_OPERATION_MS + 1, u64::MAX] {
            let limits = Limits {
                operation_ms,
                response_ms: 100,
            };
            assert!(!limits.validate());
            assert!(limits.capability_ms().is_err());
        }
        for response_ms in [0, 9, 2001, u64::MAX] {
            assert!(
                !Limits {
                    operation_ms: MAX_OWNED_OPERATION_MS,
                    response_ms
                }
                .validate()
            );
        }
        assert!(
            !Limits {
                operation_ms: 100,
                response_ms: 101
            }
            .validate()
        );
        let approvals = crate::ApprovalHost::new(4, 2, 598_000, 2000).unwrap();
        assert!(approvals.fits(Limits {
            operation_ms: MAX_OWNED_OPERATION_MS,
            response_ms: 2000
        }));
        assert!(!approvals.fits(Limits {
            operation_ms: 599_999,
            response_ms: 2000
        }));
        assert!(crate::ApprovalHost::new(4, 2, 598_001, 2000).is_err());
        assert!(crate::ApprovalHost::new(4, 2, u64::MAX, 2000).is_err());
        // Increasing active execution must not silently increase pre-activation
        // startup's distinct original thirty-second budget.
        assert!(
            crate::WarmLimits {
                initialize: Limits {
                    operation_ms: 30_000,
                    response_ms: 2000
                },
                idle_ms: 1000
            }
            .validate()
        );
        assert!(
            !crate::WarmLimits {
                initialize: Limits {
                    operation_ms: 30_001,
                    response_ms: 2000
                },
                idle_ms: 1000
            }
            .validate()
        );
    }

    #[test]
    fn native_codex_argv_is_app_server_only() {
        // ADR-139: the host passes exactly one argument, `app-server`. Both
        // launch sites (the Host constructor's admission validation and
        // `Host::prepare`) build argv from this single function, so pinning
        // its output pins every argument the host can ever pass.
        let arguments = app_server_arguments();
        assert_eq!(
            arguments,
            vec![OsString::from("app-server")],
            "the spawn argv must be exactly one argument: app-server"
        );
        // No sandbox, approval, directory or transport flag may ever appear
        // on argv: policy travels in the typed initialize/thread request and
        // stdio is the pinned CLI's default transport.
        assert!(
            arguments
                .iter()
                .all(|argument| { !argument.to_string_lossy().starts_with("--") }),
            "no flag may ever appear on the Codex spawn argv"
        );
    }

    #[test]
    fn native_receive_tools_host_profile() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        hagency_store::private::directory(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let executable = std::env::current_exe().unwrap();
        let host = |environment| {
            Host::new(
                executable.clone(),
                executable.clone(),
                environment,
                BTreeMap::from([("workspace".into(), root.clone())]),
            )
        };
        let default = host(BTreeMap::new()).unwrap();
        assert!(!default.receive_tools);
        assert!(matches!(
            default.with_receive_tools(),
            Err(super::super::Failure::Admission)
        ));
        for send in [false, true] {
            let mut configured = host(BTreeMap::new())
                .unwrap()
                .with_task_helper(executable.clone(), "127.0.0.1:13300".parse().unwrap())
                .unwrap();
            if send {
                configured = configured.with_file_tools().unwrap();
            }
            let configured = configured.with_receive_tools().unwrap();
            assert!(configured.receive_tools);
            assert_eq!(configured.file_tools, send);
        }
        for key in [
            "HAGENCY_RECEIVE_FILE_TOOLS",
            "hagency_receive_file_tools",
            "Hagency_Receive_File_Tools",
        ] {
            assert!(matches!(
                host(BTreeMap::from([(key.into(), "1".into())])),
                Err(super::super::Failure::Admission)
            ));
        }
    }

    #[test]
    fn native_file_tools_host_profile() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        hagency_store::private::directory(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let executable = std::env::current_exe().unwrap();
        let host = |environment| {
            Host::new(
                executable.clone(),
                executable.clone(),
                environment,
                BTreeMap::from([("workspace".into(), root.clone())]),
            )
        };
        let default = host(BTreeMap::new()).unwrap();
        assert!(!default.file_tools);
        assert!(matches!(
            default.with_file_tools(),
            Err(super::super::Failure::Admission)
        ));
        let configured = host(BTreeMap::new())
            .unwrap()
            .with_task_helper(executable.clone(), "127.0.0.1:13300".parse().unwrap())
            .unwrap()
            .with_file_tools()
            .unwrap();
        assert!(configured.file_tools);
        for key in [
            "HAGENCY_FILE_TOOLS",
            "hagency_file_tools",
            "Hagency_File_Tools",
        ] {
            // Reserved even without a helper/profile. An ambient inherited
            // marker must never opt an otherwise default Host into file tools.
            assert!(matches!(
                host(BTreeMap::from([(key.into(), "1".into())])),
                Err(super::super::Failure::Admission)
            ));
        }
    }
}
