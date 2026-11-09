//! Exact-commit capability plugins. Plugin code is untrusted user-level code;
//! this crate supplies package validation, registry identity, bounded invocation,
//! and runtime framing but never grants task or operator authority.

mod capability;
mod command;
mod harness;
mod management;
mod manifest;
mod qualification;
mod registry;
mod runtime;
mod settings;
mod source;
mod template;

pub use capability::{
    ApprovalDecision, EVENT_KINDS, EventQueue, MAX_EVENT_QUEUE, PluginToolApproval,
    PluginToolContent, PluginToolResult, RedactedEvent, ToolResultError, parse_tool_result,

};
pub use command::{
    ACTION_TOOL_DEADLINE, BUILD_DEADLINE, CommandEnvironment, CommandError, CommandRequest,
    CommandResult, LogRecord, MAX_GLOBAL_COMMANDS, MAX_LOG_BYTES, MAX_LOG_RECORDS,
    MAX_PLUGIN_COMMANDS, STARTUP_EVENT_DEADLINE, Scheduler, SchedulerSnapshot, bounded_utf8,
    run_command, run_supervised_command,
};
pub use harness::{
    HarnessFrame, HarnessNotification, HarnessOwnership, HarnessPrepared, HarnessProtocolError,
    HarnessRequest, decode_frame as decode_harness_frame, encode_request as encode_harness_request,
    encode_response as encode_harness_response,
};
pub use management::{ManagementParseError, PluginCommand, RefusalCode, RestartNotice};
pub use manifest::{
    ActionSpec, BuildSpec, EventSpec, HARNESS_PROTOCOL, HarnessSpec, MANIFEST_FILE, Manifest,
    ManifestError, PaneSpec, RUNTIME_PROTOCOL, RuntimeSpec, SettingKind, SettingSpec, StartupSpec,
    ToolSpec,
};
pub use qualification::{
    HarnessPluginRef, RuntimePluginRef, RuntimeQualification, evidence_digest,
};
pub use runtime::{
    HostRequest, MAX_FRAME_BYTES, MAX_PENDING_REQUESTS, PluginFrame, PluginNotification,
    RuntimeOutcome, RuntimeProtocolError, RuntimeSession, RuntimeUsage, decode_frame,
    encode_request,
};
pub use settings::{PluginSettingsDocument, PluginSettingsState, SETTINGS_SCHEMA, SettingsError};
pub use source::{
    FileEntry, PluginSource, SourceError, SourceSnapshot, checkout_github, package_manifest,
    snapshot_tree, validate_commit,
};
pub use template::{
    HARNESS_TEMPLATE_SCHEMA, HarnessTemplateDocument, digest as harness_template_digest,
};

use registry::{PluginRecord, Registry, RegistryError, now_ms};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use thiserror::Error;

pub const MAX_JSON_BYTES: usize = 1024 * 1024;
const CURRENT_HAGENCY_VERSION: &str = "0.1.0";

#[derive(Debug, Clone)]
pub struct RuntimeLaunch {
    pub runtime: RuntimePluginRef,
    pub framework: String,
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub package_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct HarnessLaunch {
    pub harness: HarnessPluginRef,
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub package_dir: PathBuf,
}

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugin subsystem is not ready: {0}")]
    NotReady(String),
    #[error("plugin is not found: {0}")]
    NotFound(String),
    #[error("plugin is disabled: {0}")]
    Disabled(String),
    #[error("plugin is stale: {0}")]
    Stale(String),
    #[error("plugin restart is required: {0}")]
    RestartRequired(String),
    #[error("plugin is incompatible")]
    Incompatible,
    #[error("plugin trust confirmation does not match the preview")]
    Untrusted,
    #[error("plugin build failed: {0}")]
    BuildFailed(String),
    #[error("plugin invocation is busy")]
    Busy,
    #[error("plugin invocation outcome is unknown")]
    OutcomeUnknown,
    #[error("plugin input or output exceeds its bound")]
    Capacity,
    #[error("plugin configuration is required: {0}")]
    ConfigurationRequired(String),
    #[error("plugin settings are invalid: {0}")]
    SettingsInvalid(String),
    #[error("plugin settings revision conflicts with current revision {0}")]
    SettingsConflict(u64),
    #[error("plugin manifest failed: {0}")]
    Manifest(#[from] ManifestError),
    #[error("plugin source failed: {0}")]
    Source(#[from] SourceError),
    #[error("plugin registry failed: {0}")]
    Registry(#[from] RegistryError),
    #[error("plugin settings failed: {0}")]
    Settings(#[from] SettingsError),
    #[error("plugin command failed: {0}")]
    Command(#[from] CommandError),
    #[error("plugin IO failed: {0}")]
    Io(#[from] io::Error),
    #[error("plugin JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginPreview {
    pub preview_id: String,
    pub source: PluginSource,
    pub manifest: Manifest,
    pub files: Vec<FileEntry>,
    pub build_argv: Vec<Vec<String>>,
    pub source_commit: Option<String>,
    pub source_sha256: String,
    pub manifest_sha256: String,
}
#[derive(Debug, Clone)]
struct PreviewState {
    preview: PluginPreview,
    root: PathBuf,
    source_owned: bool,
}

#[derive(Clone)]
pub struct PluginService {
    inner: Arc<Mutex<ManagerState>>,
}
struct ManagerState {
    registry: Registry,
    guardian: PathBuf,
    previews: BTreeMap<String, PreviewState>,
    scheduler: Scheduler,
    leases: BTreeMap<(String, String), u32>,
    logs: BTreeMap<String, VecDeque<LogRecord>>,
    events: EventQueue,
    invocations: BTreeMap<String, PluginInvocation>,
    idempotency: BTreeMap<String, String>,
    invocation_path: PathBuf,
    started: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInvocation {
    pub invocation_id: String,
    pub plugin_id: String,
    pub action_id: String,
    pub package_sha256: String,
    pub idempotency_key: String,
    pub state: String,
    pub input_sha256: String,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub at_ms: u64,
}

fn invocation_key(invocation: &PluginInvocation) -> String {
    format!(
        "{}\0{}\0action.v1\0{}\0{}",
        invocation.plugin_id,
        invocation.package_sha256,
        invocation.action_id,
        invocation.idempotency_key
    )
}

fn load_invocations(path: &Path) -> Result<BTreeMap<String, PluginInvocation>, PluginError> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn persist_invocations(
    path: &Path,
    invocations: &BTreeMap<String, PluginInvocation>,
) -> Result<(), PluginError> {
    let bytes = serde_json::to_vec_pretty(invocations)?;
    let temp = path.with_extension(format!("json.tmp-{}-{}", std::process::id(), now_ms()));
    {
        use std::io::Write;
        let mut file = fs::File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temp, path)?;
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginToolDescriptor {
    pub name: String,
    pub plugin_id: String,
    pub tool_id: String,
    pub title: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub package_sha256: String,
}
pub struct PluginLease {
    service: PluginService,
    plugin_id: String,
    package_sha256: String,
}
impl Drop for PluginLease {
    fn drop(&mut self) {
        let Ok(mut state) = self.service.inner.lock() else {
            return;
        };
        let key = (self.plugin_id.clone(), self.package_sha256.clone());
        if let Some(count) = state.leases.get_mut(&key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.leases.remove(&key);
            }
        }
        if let Some(record) = state.registry.get_mut(&self.plugin_id) {
            if let Some(revision) = record.revisions.get_mut(&self.package_sha256) {
                revision.active_leases = revision.active_leases.saturating_sub(1);
            }
            if record.active_package_sha256 == self.package_sha256 {
                record.active_leases = record.active_leases.saturating_sub(1);
            }
            let _ = state.registry.persist();
        }
    }
}
impl PluginLease {
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }
    pub fn package_sha256(&self) -> &str {
        &self.package_sha256
    }
}

fn map_template_settings_error(error: SettingsError) -> PluginError {
    match error {
        SettingsError::Invalid(message) if message.starts_with("missing required settings:") => {
            PluginError::ConfigurationRequired(message)
        }
        SettingsError::Invalid(message) => PluginError::SettingsInvalid(message),
        other => PluginError::Settings(other),
    }
}

impl PluginService {
    fn settings_root(&self, id: &str) -> Result<PathBuf, PluginError> {
        Ok(self.state_dir()?.join("plugins/config").join(id))
    }
    fn manifest_for(&self, record: &PluginRecord) -> Result<Manifest, PluginError> {
        Ok(Manifest::parse(&fs::read(
            record.install_dir.join(MANIFEST_FILE),
        )?)?)
    }
    fn refresh_settings(
        &self,
        id: &str,
        supplied: Option<BTreeMap<String, serde_json::Value>>,
        expected_revision: Option<u64>,
    ) -> Result<PluginSettingsState, PluginError> {
        let record = self.inspect(id)?;
        let manifest = self.manifest_for(&record)?;
        let root = self.settings_root(id)?;
        let current = settings::load_current(&root)?;
        let (revision, values) = match current {
            Some(current) if current.package_sha256 == record.package_sha256 => {
                settings::validate_document(
                    &current,
                    id,
                    &record.package_sha256,
                    &manifest.settings,
                )?;
                if let Some(expected) = expected_revision {
                    if expected != current.revision {
                        return Err(PluginError::SettingsConflict(current.revision));
                    }
                }
                (
                    current.revision.saturating_add(1),
                    supplied.unwrap_or(current.values),
                )
            }
            Some(current) => (
                current.revision.saturating_add(1),
                supplied.unwrap_or(current.values),
            ),
            None => {
                if let Some(expected) = expected_revision {
                    if expected != 0 {
                        return Err(PluginError::SettingsConflict(0));
                    }
                }
                (1, supplied.unwrap_or_default())
            }
        };
        let state = settings::evaluate(
            id,
            &record.package_sha256,
            revision,
            &manifest.settings,
            &values,
            expected_revision.is_some(),
        )
        .map_err(|error| match error {
            SettingsError::Invalid(message) => PluginError::SettingsInvalid(message),
            other => PluginError::Settings(other),
        })?;
        settings::persist(&root, &state.document)?;
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?;
        let current = guard
            .registry
            .get_mut(id)
            .ok_or_else(|| PluginError::NotFound(id.into()))?;
        current.settings_sha256 = state.document.settings_sha256.clone();
        current.settings_revision = state.document.revision;
        current.settings_ready = state.settings_ready;
        current.missing_required = state.missing_required.clone();
        guard.registry.persist()?;
        Ok(state)
    }
    fn ensure_settings(&self, id: &str) -> Result<(), PluginError> {
        let _ = self.settings(id)?;
        Ok(())
    }
    pub fn settings(&self, id: &str) -> Result<PluginSettingsState, PluginError> {
        let record = self.inspect(id)?;
        let manifest = self.manifest_for(&record)?;
        let root = self.settings_root(id)?;
        let current = settings::load_current(&root)?;
        if let Some(current) = current
            && current.package_sha256 == record.package_sha256
        {
            settings::validate_document(&current, id, &record.package_sha256, &manifest.settings)?;
            return Ok(PluginSettingsState {
                definitions: manifest.settings,
                settings_ready: record.settings_ready,
                missing_required: record.missing_required,
                document: current,
            });
        }
        self.refresh_settings(id, None, None)
    }
    pub fn update_settings(
        &self,
        id: &str,
        expected_revision: u64,
        values: BTreeMap<String, serde_json::Value>,
    ) -> Result<PluginSettingsState, PluginError> {
        self.refresh_settings(id, Some(values), Some(expected_revision))
    }
    fn selected_settings(
        &self,
        id: &str,
        package_sha256: &str,
        settings_sha256: &str,
    ) -> Result<PathBuf, PluginError> {
        let state = self.settings(id)?;
        if state.document.package_sha256 != package_sha256 {
            return Err(PluginError::Untrusted);
        }
        let document = if state.document.settings_sha256 == settings_sha256 {
            state.document
        } else {
            let path = settings::revision_dir(&self.settings_root(id)?, settings_sha256)
                .join("settings.json");
            let document: PluginSettingsDocument = serde_json::from_slice(&fs::read(path)?)?;
            let record = self.inspect(id)?;
            let manifest = self.manifest_for(&record)?;
            settings::validate_document(&document, id, package_sha256, &manifest.settings)?;
            document
        };
        if document.settings_sha256 != settings_sha256 {
            return Err(PluginError::Untrusted);
        }
        let record = self.inspect(id)?;
        let manifest = self.manifest_for(&record)?;
        let checked = settings::evaluate(
            id,
            package_sha256,
            document.revision,
            &manifest.settings,
            &document.values,
            true,
        )
        .map_err(|error| match error {
            SettingsError::Invalid(message) => PluginError::ConfigurationRequired(message),
            other => PluginError::Settings(other),
        })?;
        if checked.document.settings_sha256 != settings_sha256 {
            return Err(PluginError::Untrusted);
        }
        Ok(settings::revision_dir(
            &self.settings_root(id)?,
            settings_sha256,
        ))
    }
    pub fn open(state_dir: impl AsRef<Path>) -> Result<Self, PluginError> {
        let registry = Registry::open(state_dir)?;
        let guardian = std::env::current_exe()?;
        let invocation_path = registry.state_dir().join("plugins/invocations.json");
        let mut invocations = load_invocations(&invocation_path)?;
        let mut recovered = false;
        for invocation in invocations.values_mut() {
            if matches!(invocation.state.as_str(), "queued" | "running") {
                invocation.state = "outcome_unknown".into();
                invocation.error = Some("service restarted before invocation completion".into());
                invocation.at_ms = now_ms();
                recovered = true;
            }
        }
        if recovered {
            persist_invocations(&invocation_path, &invocations)?;
        }
        let idempotency = invocations
            .values()
            .map(|invocation| (invocation_key(invocation), invocation.invocation_id.clone()))
            .collect();
        let service = Self {
            inner: Arc::new(Mutex::new(ManagerState {
                registry,
                guardian,
                previews: BTreeMap::new(),
                scheduler: Scheduler::new(),
                leases: BTreeMap::new(),
                logs: BTreeMap::new(),
                events: EventQueue::default(),
                invocations,
                idempotency,
                invocation_path,
                started: false,
            })),
        };
        let ids = service
            .list()?
            .into_iter()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        for id in ids {
            service.ensure_settings(&id)?;
        }
        Ok(service)
    }
    fn run_startup_entries(&self, record: &PluginRecord) -> Result<(), PluginError> {
        let result = (|| {
            let manifest = Manifest::parse(&fs::read(record.install_dir.join(MANIFEST_FILE))?)?;
            let lease = self.acquire_lease(&record.id)?;
            for startup in manifest.startup {
                let environment =
                    self.environment(&record.id, &record.install_dir, &startup.id, None)?;
                self.run_bounded(
                    &record.id,
                    CommandRequest {
                        argv: startup.command,
                        cwd: record.install_dir.clone(),
                        stdin: Vec::new(),
                        environment,
                        deadline: STARTUP_EVENT_DEADLINE,
                    },
                )?;
            }
            drop(lease);
            Ok::<(), PluginError>(())
        })();
        let readiness = if result.is_ok() {
            "ready"
        } else {
            "startup_failed"
        };
        let mut state = self
            .inner
            .lock()
            .map_err(|_| PluginError::NotReady("service lock poisoned".into()))?;
        if let Some(current) = state.registry.get_mut(&record.id) {
            current.readiness = readiness.into();
            state.registry.persist()?;
        }
        Ok(())
    }
    pub fn start(&self) -> Result<(), PluginError> {
        let records = {
            let mut state = self
                .inner
                .lock()
                .map_err(|_| PluginError::NotReady("service lock poisoned".into()))?;
            if state.started {
                return Ok(());
            }
            let ids: Vec<String> = state
                .registry
                .records()
                .map(|record| record.id.clone())
                .collect();
            for id in &ids {
                if let Some(record) = state.registry.get_mut(id) {
                    record.restart_required = false;
                    record.readiness = if record.stale {
                        "stale".into()
                    } else if record.enabled {
                        "ready".into()
                    } else {
                        "disabled".into()
                    };
                }
            }
            state.started = true;
            state.registry.persist()?;
            state
                .registry
                .records()
                .filter(|record| record.enabled && !record.stale)
                .cloned()
                .collect::<Vec<_>>()
        };
        for record in records {
            let _ = self.run_startup_entries(&record);
        }
        Ok(())
    }
    pub fn shutdown(&self) -> Result<(), PluginError> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| PluginError::NotReady("service lock poisoned".into()))?;
        state.started = false;
        Ok(())
    }
    pub fn state_dir(&self) -> Result<PathBuf, PluginError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?
            .registry
            .state_dir()
            .to_path_buf())
    }
    pub fn list(&self) -> Result<Vec<PluginRecord>, PluginError> {
        let state = self
            .inner
            .lock()
            .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?;
        Ok(state.registry.records().cloned().collect())
    }
    pub fn inspect(&self, id: &str) -> Result<PluginRecord, PluginError> {
        self.inner
            .lock()
            .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?
            .registry
            .get(id)
            .cloned()
            .ok_or_else(|| PluginError::NotFound(id.into()))
    }
    pub fn manifest(&self, id: &str) -> Result<Manifest, PluginError> {
        let record = self.inspect(id)?;
        Manifest::parse(&fs::read(record.install_dir.join(MANIFEST_FILE))?)
            .map_err(PluginError::from)
    }
    pub fn preview_install(&self, source: PluginSource) -> Result<PluginPreview, PluginError> {
        let (root, source_owned) = match &source {
            PluginSource::Linked { path } => (path.clone(), false),
            PluginSource::Github { .. } => {
                let staging = self.staging_path("preview");
                (checkout_github(&source, &staging)?, true)
            }
        };
        let (manifest, _manifest_bytes, snapshot) = package_manifest(&root)?;
        validate_compatibility(&manifest)?;
        let manifest_sha256 = sha256(&fs::read(root.join(MANIFEST_FILE))?);
        let preview = PluginPreview {
            preview_id: format!("preview-{}-{}", std::process::id(), now_ms()),
            source: source.clone(),
            build_argv: manifest.builds.iter().map(|v| v.command.clone()).collect(),
            source_commit: source.commit().map(str::to_owned),
            source_sha256: snapshot.digest,
            manifest,
            files: snapshot.files,
            manifest_sha256,
        };
        let mut state = self
            .inner
            .lock()
            .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?;
        state.previews.insert(
            preview.preview_id.clone(),
            PreviewState {
                preview: preview.clone(),
                root,
                source_owned,
            },
        );
        Ok(preview)
    }
    pub fn apply_install(
        &self,
        preview_id: &str,
        trust_digest: &str,
    ) -> Result<PluginRecord, PluginError> {
        let preview_state = {
            let state = self
                .inner
                .lock()
                .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?;
            state
                .previews
                .get(preview_id)
                .cloned()
                .ok_or_else(|| PluginError::NotFound(preview_id.into()))?
        };
        if trust_digest != preview_state.preview.source_sha256 {
            return Err(PluginError::Untrusted);
        }
        let preview_state = {
            let mut state = self
                .inner
                .lock()
                .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?;
            state
                .previews
                .remove(preview_id)
                .ok_or_else(|| PluginError::NotFound(preview_id.into()))?
        };
        let preview = &preview_state.preview;
        if preview_state.source_owned {
            for (index, build) in preview.manifest.builds.iter().enumerate() {
                let env = self.environment(
                    &preview.manifest.id,
                    &preview_state.root,
                    &format!("build-{index}"),
                    None,
                )?;
                let request = CommandRequest {
                    argv: build.command.clone(),
                    cwd: preview_state.root.clone(),
                    stdin: Vec::new(),
                    environment: env,
                    deadline: BUILD_DEADLINE,
                };
                if let Err(error) = self.run_bounded(&preview.manifest.id, request) {
                    return Err(PluginError::BuildFailed(error.to_string()));
                }
            }
        }
        let manifest_bytes = fs::read(preview_state.root.join(MANIFEST_FILE))?;
        if sha256(&manifest_bytes) != preview.manifest_sha256 {
            return Err(PluginError::BuildFailed(
                "manifest changed during build".into(),
            ));
        }
        let (manifest, _, snapshot) = package_manifest(&preview_state.root)?;
        if serde_json::to_vec(&manifest)? != serde_json::to_vec(&preview.manifest)? {
            return Err(PluginError::BuildFailed(
                "manifest changed during build".into(),
            ));
        }
        let package_sha256 = snapshot.digest.clone();
        let install_dir = if preview_state.source_owned {
            let commit = preview.source_commit.as_deref().unwrap_or("linked");
            let dir = self
                .state_dir()?
                .join("plugins/installs")
                .join(&manifest.id)
                .join(format!("{commit}-{package_sha256}"));
            if let Some(parent) = dir.parent() {
                fs::create_dir_all(parent)?;
            }
            if !dir.exists() {
                copy_tree(&preview_state.root, &dir)?;
                sync_tree(&dir)?;
            }
            dir
        } else {
            preview_state.root.clone()
        };
        let record = PluginRecord {
            id: manifest.id.clone(),
            source: preview.source.clone(),
            enabled: false,
            active_package_sha256: package_sha256.clone(),
            revisions: BTreeMap::new(),
            stale: false,
            restart_required: false,
            manifest_version: manifest.version.to_string(),
            source_sha256: preview.source_sha256.clone(),
            package_sha256,
            install_dir,
            capabilities: manifest.capabilities(),
            updated_at_ms: now_ms(),
            active_leases: 0,
            readiness: "disabled".into(),
            settings_sha256: String::new(),
            settings_revision: 0,
            settings_ready: true,
            missing_required: Vec::new(),
        };
        let mut state = self
            .inner
            .lock()
            .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?;
        state.registry.insert(record.clone())?;
        drop(state);
        self.ensure_settings(&record.id)?;
        self.inspect(&record.id)
    }
    pub fn link(&self, path: impl AsRef<Path>) -> Result<PluginRecord, PluginError> {
        let source = PluginSource::linked(path.as_ref().to_path_buf())?;
        let preview = self.preview_install(source)?;
        self.apply_link(&preview.preview_id)
    }
    fn apply_link(&self, preview_id: &str) -> Result<PluginRecord, PluginError> {
        let preview_state = {
            let mut state = self
                .inner
                .lock()
                .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?;
            state
                .previews
                .remove(preview_id)
                .ok_or_else(|| PluginError::NotFound(preview_id.into()))?
        };
        if preview_state.source_owned {
            return Err(PluginError::BuildFailed(
                "GitHub sources cannot be linked".into(),
            ));
        }
        let (_, _, snapshot) = package_manifest(&preview_state.root)?;
        let manifest = &preview_state.preview.manifest;
        let record = PluginRecord {
            id: manifest.id.clone(),
            source: preview_state.preview.source.clone(),
            enabled: false,
            active_package_sha256: snapshot.digest.clone(),
            revisions: BTreeMap::new(),
            stale: false,
            restart_required: false,
            manifest_version: manifest.version.to_string(),
            source_sha256: snapshot.digest.clone(),
            package_sha256: snapshot.digest,
            install_dir: preview_state.root,
            capabilities: manifest.capabilities(),
            updated_at_ms: now_ms(),
            active_leases: 0,
            settings_sha256: String::new(),
            settings_revision: 0,
            settings_ready: true,
            missing_required: Vec::new(),
            readiness: "disabled".into(),
        };
        let mut state = self
            .inner
            .lock()
            .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?;
        state.registry.insert(record.clone())?;
        drop(state);
        self.ensure_settings(&record.id)?;
        self.inspect(&record.id)
    }
    pub fn enable(&self, id: &str) -> Result<PluginRecord, PluginError> {
        self.set_enabled(id, true)
    }
    pub fn disable(&self, id: &str) -> Result<PluginRecord, PluginError> {
        self.set_enabled(id, false)
    }
    fn set_enabled(&self, id: &str, enabled: bool) -> Result<PluginRecord, PluginError> {
        let (record, started) = {
            let mut state = self
                .inner
                .lock()
                .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?;
            let started = state.started;
            let record = state
                .registry
                .get_mut(id)
                .ok_or_else(|| PluginError::NotFound(id.into()))?;
            record.enabled = enabled;
            record.readiness = if !enabled {
                "disabled".into()
            } else if record.restart_required {
                "restart_required".into()
            } else if record.stale {
                "stale".into()
            } else {
                "ready".into()
            };
            let record = record.clone();
            state.registry.persist()?;
            (record, started)
        };
        if enabled && started && !record.stale && !record.restart_required {
            let _ = self.run_startup_entries(&record);
        }
        Ok(record)
    }
    pub fn uninstall(&self, id: &str) -> Result<(), PluginError> {
        let record = self.inspect(id)?;
        if record.enabled
            || record.active_leases != 0
            || record
                .revisions
                .values()
                .any(|revision| revision.active_leases != 0)
            || record
                .revisions
                .keys()
                .any(|digest| self.lease_count(id, digest).unwrap_or(0) != 0)
        {
            return Err(PluginError::Busy);
        }
        let mut state = self
            .inner
            .lock()
            .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?;
        state.registry.remove(id)?;
        if !matches!(record.source, PluginSource::Linked { .. }) {
            let _ = fs::remove_dir_all(record.install_dir);
        }
        Ok(())
    }
    pub fn acquire_lease(&self, id: &str) -> Result<PluginLease, PluginError> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?;
        let record = state
            .registry
            .get_mut(id)
            .ok_or_else(|| PluginError::NotFound(id.into()))?;
        if !record.enabled {
            return Err(PluginError::Disabled(id.into()));
        }
        if record.stale {
            return Err(PluginError::Stale(id.into()));
        }
        let digest = record.active_package_sha256.clone();
        let revision = record
            .revisions
            .get_mut(&digest)
            .ok_or_else(|| PluginError::NotReady("active revision is missing".into()))?;
        revision.active_leases = revision.active_leases.saturating_add(1);
        record.active_leases = record.active_leases.saturating_add(1);
        *state.leases.entry((id.into(), digest.clone())).or_default() += 1;
        state.registry.persist()?;
        Ok(PluginLease {
            service: self.clone(),
            plugin_id: id.into(),
            package_sha256: digest,
        })
    }
    pub fn invoke_action(
        &self,
        id: &str,
        action_id: &str,
        input: &serde_json::Value,
    ) -> Result<InvocationResult, PluginError> {
        let record = self.inspect(id)?;
        self.validate_live_revision(&record)?;
        if !record.enabled {
            return Err(PluginError::Disabled(id.into()));
        }
        if record.stale {
            return Err(PluginError::Stale(id.into()));
        }
        if record.restart_required {
            return Err(PluginError::RestartRequired(id.into()));
        }
        if serde_json::to_vec(input)?.len() > MAX_JSON_BYTES {
            return Err(PluginError::Capacity);
        }
        let bytes = fs::read(record.install_dir.join(MANIFEST_FILE))?;
        let manifest = Manifest::parse(&bytes)?;
        let action = manifest
            .actions
            .iter()
            .find(|action| action.id == action_id)
            .ok_or_else(|| PluginError::NotFound(format!("{id}.{action_id}")))?;
        let lease = self.acquire_lease(id)?;
        let stdin = serde_json::to_vec(input)?;
        let env = self.environment(id, &record.install_dir, action_id, None)?;
        let result = self.run_bounded(
            id,
            CommandRequest {
                argv: action.command.clone(),
                cwd: record.install_dir.clone(),
                stdin,
                environment: env,
                deadline: ACTION_TOOL_DEADLINE,
            },
        )?;
        drop(lease);
        if result.stdout.len() > MAX_JSON_BYTES {
            return Err(PluginError::Capacity);
        }
        let value = serde_json::from_slice(&result.stdout)?;
        Ok(InvocationResult {
            invocation_id: format!("invocation-{}-{}", id, now_ms()),
            value,
            stderr: String::from_utf8_lossy(&result.stderr).into_owned(),
        })
    }

    pub fn tool_catalog(&self, id: &str) -> Result<Vec<PluginToolDescriptor>, PluginError> {
        let record = self.inspect(id)?;
        self.validate_live_revision(&record)?;
        if record.stale {
            return Err(PluginError::Stale(id.into()));
        }
        let manifest = Manifest::parse(&fs::read(record.install_dir.join(MANIFEST_FILE))?)?;
        Ok(manifest
            .tools
            .into_iter()
            .map(|tool| PluginToolDescriptor {
                name: format!("plugin.{id}.{}", tool.id),
                plugin_id: id.into(),
                tool_id: tool.id,
                title: tool.title,
                description: tool.description,
                input_schema: tool.input_schema,
                package_sha256: record.active_package_sha256.clone(),
            })
            .collect())
    }

    pub fn prepare_tool_approval(
        &self,
        framework: &str,
        id: &str,
        tool_id: &str,
        input: &serde_json::Value,
    ) -> Result<PluginToolApproval, PluginError> {
        let record = self.inspect(id)?;
        self.validate_live_revision(&record)?;
        if !record.enabled {
            return Err(PluginError::Disabled(id.into()));
        }
        if record.stale {
            return Err(PluginError::Stale(id.into()));
        }
        if record.restart_required {
            return Err(PluginError::RestartRequired(id.into()));
        }
        if framework.is_empty() || framework.len() > 64 {
            return Err(PluginError::Incompatible);
        }
        let manifest = Manifest::parse(&fs::read(record.install_dir.join(MANIFEST_FILE))?)?;
        if !manifest.tools.iter().any(|tool| tool.id == tool_id) {
            return Err(PluginError::NotFound(format!("{id}.{tool_id}")));
        }
        PluginToolApproval::new(framework, id, tool_id, &record.active_package_sha256, input)
    }

    pub fn invoke_approved_tool(
        &self,
        approval: &PluginToolApproval,
        input: &serde_json::Value,
        decision: ApprovalDecision,
    ) -> Result<PluginToolResult, PluginError> {
        let expected = self.prepare_tool_approval(
            &approval.framework,
            &approval.plugin_id,
            &approval.tool_id,
            input,
        )?;
        if expected != *approval {
            return Err(PluginError::Untrusted);
        }
        if decision == ApprovalDecision::Deny {
            return Ok(PluginToolResult {
                content: vec![PluginToolContent {
                    content_type: "text".into(),
                    text: "plugin tool denied by owner".into(),
                }],
                is_error: true,
            });
        }
        let record = self.inspect(&approval.plugin_id)?;
        let manifest = Manifest::parse(&fs::read(record.install_dir.join(MANIFEST_FILE))?)?;
        let tool = manifest
            .tools
            .iter()
            .find(|tool| tool.id == approval.tool_id)
            .ok_or_else(|| {
                PluginError::NotFound(format!("{}.{}", approval.plugin_id, approval.tool_id))
            })?;
        let lease = self.acquire_lease(&approval.plugin_id)?;
        let environment = self.environment(
            &approval.plugin_id,
            &record.install_dir,
            &approval.tool_id,
            None,
        )?;
        let result = self.run_bounded(
            &approval.plugin_id,
            CommandRequest {
                argv: tool.command.clone(),
                cwd: record.install_dir,
                stdin: serde_json::to_vec(input)?,
                environment,
                deadline: ACTION_TOOL_DEADLINE,
            },
        )?;
        drop(lease);
        parse_tool_result(&result.stdout).map_err(|_| PluginError::OutcomeUnknown)
    }

    pub fn submit_action(
        &self,
        id: &str,
        action_id: &str,
        input: serde_json::Value,
        idempotency_key: &str,
    ) -> Result<PluginInvocation, PluginError> {
        let record = self.inspect(id)?;
        if !record.enabled {
            return Err(PluginError::Disabled(id.into()));
        }
        if record.stale {
            return Err(PluginError::Stale(id.into()));
        }
        if idempotency_key.is_empty() || idempotency_key.len() > 128 {
            return Err(PluginError::Incompatible);
        }
        let input_sha256 = sha256(&serde_json::to_vec(&input)?);
        let key = format!(
            "{}\0{}\0action.v1\0{}\0{}",
            id, record.package_sha256, action_id, idempotency_key
        );
        let invocation = {
            let mut state = self
                .inner
                .lock()
                .map_err(|_| PluginError::NotReady("service lock poisoned".into()))?;
            if let Some(existing) = state.idempotency.get(&key) {
                return state.invocations.get(existing).cloned().ok_or_else(|| {
                    PluginError::NotReady("invocation custody is incomplete".into())
                });
            }
            let invocation = PluginInvocation {
                invocation_id: format!("invocation-{}", sha256(key.as_bytes())),
                plugin_id: id.into(),
                action_id: action_id.into(),
                package_sha256: record.package_sha256.clone(),
                idempotency_key: idempotency_key.into(),
                state: "queued".into(),
                input_sha256,
                result: None,
                error: None,
                at_ms: now_ms(),
            };
            state
                .idempotency
                .insert(key.clone(), invocation.invocation_id.clone());
            state
                .invocations
                .insert(invocation.invocation_id.clone(), invocation.clone());
            if let Err(error) = persist_invocations(&state.invocation_path, &state.invocations) {
                state.idempotency.remove(&key);
                state.invocations.remove(&invocation.invocation_id);
                return Err(error);
            }
            invocation
        };
        let service = self.clone();
        let invocation_id = invocation.invocation_id.clone();
        let plugin_id = id.to_owned();
        let action_id = action_id.to_owned();
        let input = input.clone();
        std::thread::spawn(move || {
            if let Ok(mut state) = service.inner.lock() {
                if let Some(item) = state.invocations.get_mut(&invocation_id) {
                    item.state = "running".into();
                    item.at_ms = now_ms();
                }
                let _ = persist_invocations(&state.invocation_path, &state.invocations);
            }
            let result = service.invoke_action(&plugin_id, &action_id, &input);
            if let Ok(mut state) = service.inner.lock() {
                if let Some(item) = state.invocations.get_mut(&invocation_id) {
                    match result {
                        Ok(result) => {
                            item.state = "succeeded".into();
                            item.result = Some(result.value);
                        }
                        Err(error) => {
                            item.state = if matches!(
                                error,
                                PluginError::OutcomeUnknown
                                    | PluginError::Command(CommandError::OutcomeUnknown)
                            ) {
                                "outcome_unknown"
                            } else {
                                "failed"
                            }
                            .into();
                            item.error = Some(error.to_string());
                        }
                    }
                    item.at_ms = now_ms();
                }
                let _ = persist_invocations(&state.invocation_path, &state.invocations);
            }
        });
        Ok(invocation)
    }

    pub fn invocation(&self, invocation_id: &str) -> Result<PluginInvocation, PluginError> {
        self.inner
            .lock()
            .map_err(|_| PluginError::NotReady("service lock poisoned".into()))?
            .invocations
            .get(invocation_id)
            .cloned()
            .ok_or_else(|| PluginError::NotFound(invocation_id.into()))
    }

    pub fn mark_stale(&self, id: &str) -> Result<(), PluginError> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| PluginError::NotReady("service lock poisoned".into()))?;
        let record = state
            .registry
            .get_mut(id)
            .ok_or_else(|| PluginError::NotFound(id.into()))?;
        record.stale = true;
        record.readiness = "stale".into();
        state.registry.persist()?;
        Ok(())
    }
    pub fn qualify_runtime(&self, qualification: &RuntimeQualification) -> Result<(), PluginError> {
        let record = self.inspect(&qualification.runtime.plugin_id)?;
        if record.stale {
            return Err(PluginError::Stale(record.id));
        }
        if record.restart_required {
            return Err(PluginError::RestartRequired(record.id));
        }
        if !record.enabled {
            return Err(PluginError::Disabled(record.id));
        }
        if record.package_sha256 != qualification.runtime.package_sha256 {
            return Err(PluginError::Untrusted);
        }
        self.selected_settings(
            &record.id,
            &qualification.runtime.package_sha256,
            &qualification.runtime.settings_sha256,
        )?;
        if qualification.evidence_digest.len() != 64
            || !qualification
                .evidence_digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(PluginError::Incompatible);
        }
        let manifest = Manifest::parse(&fs::read(record.install_dir.join(MANIFEST_FILE))?)?;
        if !manifest.runtimes.iter().any(|runtime| {
            runtime.id == qualification.runtime.adapter_id
                && runtime.framework == qualification.framework
                && runtime.protocol == RUNTIME_PROTOCOL
        }) {
            return Err(PluginError::Incompatible);
        }
        if qualification.model.is_empty()
            || qualification.model.len() > 256
            || qualification.provider.is_empty()
        {
            return Err(PluginError::Incompatible);
        }
        Ok(())
    }
    /// Re-check enablement and package identity at the execution boundary, then
    /// hold the package lease until the caller's owned runtime has stopped.
    pub fn claim_runtime(
        &self,
        qualification: &RuntimeQualification,
    ) -> Result<PluginLease, PluginError> {
        self.qualify_runtime(qualification)?;
        let lease = self.acquire_lease(&qualification.runtime.plugin_id)?;
        if lease.package_sha256() != qualification.runtime.package_sha256 {
            drop(lease);
            return Err(PluginError::Untrusted);
        }
        self.selected_settings(
            &qualification.runtime.plugin_id,
            &qualification.runtime.package_sha256,
            &qualification.runtime.settings_sha256,
        )?;
        Ok(lease)
    }

    /// Re-check the runtime package identity before retaining its execution lease.
    pub fn claim_runtime_launch(&self, launch: &RuntimeLaunch) -> Result<PluginLease, PluginError> {
        let current = self.runtime_launch(&launch.runtime, &launch.framework)?;
        let lease = self.acquire_lease(&launch.runtime.plugin_id)?;
        if lease.package_sha256() != current.runtime.package_sha256
            || launch.runtime.settings_sha256 != current.runtime.settings_sha256
        {
            drop(lease);
            return Err(PluginError::Untrusted);
        }
        Ok(lease)
    }

    /// Resolve a previously selected harness to its immutable package command.
    pub fn harness_launch(&self, harness: &HarnessPluginRef) -> Result<HarnessLaunch, PluginError> {
        let record = self.inspect(&harness.plugin_id)?;
        self.validate_live_revision(&record)?;
        if record.stale {
            return Err(PluginError::Stale(record.id));
        }
        if record.restart_required {
            return Err(PluginError::RestartRequired(record.id));
        }
        if !record.enabled {
            return Err(PluginError::Disabled(record.id));
        }
        if record.package_sha256 != harness.package_sha256 {
            return Err(PluginError::Untrusted);
        }
        self.selected_settings(
            &record.id,
            &harness.package_sha256,
            &harness.settings_sha256,
        )?;
        let manifest = Manifest::parse(&fs::read(record.install_dir.join(MANIFEST_FILE))?)?;
        manifest.validate_package_root(&record.install_dir)?;
        let spec = manifest
            .harnesses
            .iter()
            .find(|spec| spec.id == harness.harness_id)
            .ok_or(PluginError::Incompatible)?;
        Ok(HarnessLaunch {
            harness: harness.clone(),
            executable: record.install_dir.join(&spec.command[0]),
            arguments: spec.command[1..].iter().map(OsString::from).collect(),
            package_dir: record.install_dir,
        })
    }
    fn harness_configuration(
        &self,
        harness: &HarnessPluginRef,
    ) -> Result<Vec<SettingSpec>, PluginError> {
        self.harness_launch(harness)?;
        let record = self.inspect(&harness.plugin_id)?;
        let manifest = self.manifest_for(&record)?;
        let spec = manifest
            .harnesses
            .into_iter()
            .find(|spec| spec.id == harness.harness_id)
            .ok_or(PluginError::Incompatible)?;
        Ok(spec.configuration)
    }

    pub fn resolve_harness_template(
        &self,
        harness: &HarnessPluginRef,
        supplied: &BTreeMap<String, serde_json::Value>,
    ) -> Result<HarnessTemplateDocument, PluginError> {
        let configuration = self.harness_configuration(harness)?;
        let (values, _) = settings::resolve_values(&configuration, supplied, true)
            .map_err(map_template_settings_error)?;
        let mut document = HarnessTemplateDocument {
            schema: HARNESS_TEMPLATE_SCHEMA.into(),
            plugin_id: harness.plugin_id.clone(),
            harness_id: harness.harness_id.clone(),
            package_sha256: harness.package_sha256.clone(),
            settings_sha256: harness.settings_sha256.clone(),
            template_sha256: String::new(),
            values,
        };
        document.template_sha256 = template::digest(&document)?;
        if serde_json::to_vec(&document)?.len() > MAX_JSON_BYTES {
            return Err(PluginError::Capacity);
        }
        Ok(document)
    }

    pub fn validate_harness_template(
        &self,
        document: &HarnessTemplateDocument,
    ) -> Result<HarnessPluginRef, PluginError> {
        if document.schema != HARNESS_TEMPLATE_SCHEMA {
            return Err(PluginError::Untrusted);
        }
        let harness = HarnessPluginRef {
            plugin_id: document.plugin_id.clone(),
            harness_id: document.harness_id.clone(),
            package_sha256: document.package_sha256.clone(),
            settings_sha256: document.settings_sha256.clone(),
        };
        let configuration = self.harness_configuration(&harness)?;
        let (values, _) = settings::resolve_values(&configuration, &document.values, true)
            .map_err(map_template_settings_error)?;
        if values != document.values || template::digest(document)? != document.template_sha256 {
            return Err(PluginError::Untrusted);
        }
        if serde_json::to_vec(document)?.len() > MAX_JSON_BYTES {
            return Err(PluginError::Capacity);
        }
        Ok(harness)
    }

    pub fn claim_harness(&self, harness: &HarnessPluginRef) -> Result<PluginLease, PluginError> {
        let launch = self.harness_launch(harness)?;
        let lease = self.acquire_lease(&harness.plugin_id)?;
        if lease.package_sha256() != launch.harness.package_sha256
            || launch.harness.settings_sha256 != harness.settings_sha256
        {
            drop(lease);
            return Err(PluginError::Untrusted);
        }
        Ok(lease)
    }

    pub fn runtime_environment(
        &self,
        launch: &RuntimeLaunch,
        workspace: PathBuf,
    ) -> Result<CommandEnvironment, PluginError> {
        self.environment_with_settings(
            &launch.runtime.plugin_id,
            &launch.package_dir,
            &launch.runtime.adapter_id,
            Some(workspace),
            &launch.runtime.settings_sha256,
            &launch.runtime.package_sha256,
        )
    }

    pub fn harness_environment(
        &self,
        launch: &HarnessLaunch,
        workspace: PathBuf,
    ) -> Result<CommandEnvironment, PluginError> {
        self.environment_with_settings(
            &launch.harness.plugin_id,
            &launch.package_dir,
            &launch.harness.harness_id,
            Some(workspace),
            &launch.harness.settings_sha256,
            &launch.harness.package_sha256,
        )
    }

    /// Resolve a previously qualified runtime to its immutable package command.
    /// The returned executable is package-owned; callers still choose the task
    /// workspace as the process working directory.
    pub fn runtime_launch(
        &self,
        runtime: &RuntimePluginRef,
        framework: &str,
    ) -> Result<RuntimeLaunch, PluginError> {
        let record = self.inspect(&runtime.plugin_id)?;
        self.validate_live_revision(&record)?;
        if record.stale {
            return Err(PluginError::Stale(record.id));
        }
        if record.restart_required {
            return Err(PluginError::RestartRequired(record.id));
        }
        if !record.enabled {
            return Err(PluginError::Disabled(record.id));
        }
        if record.package_sha256 != runtime.package_sha256 {
            return Err(PluginError::Untrusted);
        }
        self.selected_settings(
            &record.id,
            &runtime.package_sha256,
            &runtime.settings_sha256,
        )?;
        let manifest = Manifest::parse(&fs::read(record.install_dir.join(MANIFEST_FILE))?)?;
        manifest.validate_package_root(&record.install_dir)?;
        let spec = manifest
            .runtimes
            .iter()
            .find(|spec| spec.id == runtime.adapter_id && spec.framework == framework)
            .ok_or(PluginError::Incompatible)?;
        let executable = record.install_dir.join(&spec.command[0]);
        let arguments = spec.command[1..].iter().map(OsString::from).collect();
        Ok(RuntimeLaunch {
            runtime: runtime.clone(),
            framework: framework.into(),
            executable,
            arguments,
            package_dir: record.install_dir,
        })
    }
    pub fn logs(&self, id: &str) -> Result<Vec<LogRecord>, PluginError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?
            .logs
            .get(id)
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default())
    }

    pub fn publish_event(&self, event: RedactedEvent) -> Result<(), PluginError> {
        let input = serde_json::to_vec(&event)?;
        if input.len() > MAX_JSON_BYTES {
            return Err(PluginError::Capacity);
        }
        let targets = {
            let mut state = self
                .inner
                .lock()
                .map_err(|_| PluginError::NotReady("service lock poisoned".into()))?;
            if !state.started {
                return Err(PluginError::NotReady("service is not started".into()));
            }
            state.events.push(event.clone());
            state
                .registry
                .records()
                .filter(|record| record.enabled && !record.stale)
                .filter_map(|record| {
                    let manifest =
                        Manifest::parse(&fs::read(record.install_dir.join(MANIFEST_FILE)).ok()?)
                            .ok()?;
                    let commands = manifest
                        .events
                        .into_iter()
                        .filter(|spec| spec.on == event.kind)
                        .map(|spec| (spec.on, spec.command))
                        .collect::<Vec<_>>();
                    (!commands.is_empty())
                        .then(|| (record.id.clone(), record.install_dir.clone(), commands))
                })
                .collect::<Vec<_>>()
        };
        let service = self.clone();
        std::thread::spawn(move || {
            for (id, root, commands) in targets {
                let Ok(lease) = service.acquire_lease(&id) else {
                    continue;
                };
                for (entrypoint, argv) in commands {
                    let Ok(environment) = service.environment(&id, &root, &entrypoint, None) else {
                        continue;
                    };
                    let _ = service.run_bounded(
                        &id,
                        CommandRequest {
                            argv,
                            cwd: root.clone(),
                            stdin: input.clone(),
                            environment,
                            deadline: STARTUP_EVENT_DEADLINE,
                        },
                    );
                }
                drop(lease);
            }
        });
        Ok(())
    }

    pub fn drain_events(&self) -> Result<(Vec<RedactedEvent>, u64), PluginError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| PluginError::NotReady("service lock poisoned".into()))?
            .events
            .drain())
    }
    pub fn config_dir(&self, id: &str) -> Result<PathBuf, PluginError> {
        if self.inspect(id).is_err() {
            return Err(PluginError::NotFound(id.into()));
        }
        let path = self.state_dir()?.join("plugins/config").join(id);
        fs::create_dir_all(&path)?;
        Ok(path)
    }
    pub fn scheduler_snapshot(&self, id: &str) -> Result<SchedulerSnapshot, PluginError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?
            .scheduler
            .snapshot(id))
    }
    pub fn collect_obsolete(&self, id: &str) -> Result<(), PluginError> {
        let record = self.inspect(id)?;
        let root = self.state_dir()?.join("plugins/installs").join(id);
        if !root.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry.path() == record.install_dir {
                continue;
            }
            let mut retained = false;
            for (digest, revision) in &record.revisions {
                if revision.install_dir == entry.path()
                    && (revision.active_leases != 0 || self.lease_count(id, digest)? != 0)
                {
                    retained = true;
                    break;
                }
            }
            if !retained {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
        Ok(())
    }
    fn run_bounded(&self, id: &str, request: CommandRequest) -> Result<CommandResult, PluginError> {
        let (slot, guardian) = {
            let state = self
                .inner
                .lock()
                .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?;
            let slot = state.scheduler.try_acquire(id).map_err(|error| {
                if matches!(error, CommandError::Busy) {
                    PluginError::Busy
                } else {
                    PluginError::Command(error)
                }
            })?;
            (slot, state.guardian.clone())
        };
        let result = run_supervised_command(&guardian, request);
        let (stream_text, error_text) = match &result {
            Ok(result) => (
                String::from_utf8_lossy(&result.stdout).into_owned(),
                String::from_utf8_lossy(&result.stderr).into_owned(),
            ),
            Err(error) => (String::new(), error.to_string()),
        };
        if let Ok(mut state) = self.inner.lock() {
            let logs = state.logs.entry(id.into()).or_default();
            logs.push_back(LogRecord {
                at_ms: now_ms(),
                invocation_id: format!("invocation-{}", now_ms()),
                stream: "stdout".into(),
                text: stream_text,
            });
            if !error_text.is_empty() {
                logs.push_back(LogRecord {
                    at_ms: now_ms(),
                    invocation_id: format!("invocation-{}", now_ms()),
                    stream: "stderr".into(),
                    text: error_text,
                });
            }
            while logs.len() > MAX_LOG_RECORDS {
                logs.pop_front();
            }
        }
        drop(slot);
        result.map_err(PluginError::Command)
    }
    fn environment(
        &self,
        id: &str,
        root: &Path,
        entrypoint_id: &str,
        workspace: Option<PathBuf>,
    ) -> Result<CommandEnvironment, PluginError> {
        let settings = self.settings(id)?;
        self.environment_with_settings(
            id,
            root,
            entrypoint_id,
            workspace,
            &settings.document.settings_sha256,
            &settings.document.package_sha256,
        )
    }
    fn environment_with_settings(
        &self,
        id: &str,
        root: &Path,
        entrypoint_id: &str,
        workspace: Option<PathBuf>,
        settings_sha256: &str,
        package_sha256: &str,
    ) -> Result<CommandEnvironment, PluginError> {
        let state_dir = self.state_dir()?;
        let config_dir = self.selected_settings(id, package_sha256, settings_sha256)?;
        let plugin_state_dir = state_dir.join("plugins/state").join(id);
        fs::create_dir_all(&plugin_state_dir)?;
        Ok(CommandEnvironment {
            plugin_id: id.into(),
            plugin_root: root.to_path_buf(),
            config_dir,
            state_dir: plugin_state_dir,
            entrypoint_id: entrypoint_id.into(),
            path: std::env::var_os("PATH"),
            workspace,
        })
    }
    fn staging_path(&self, label: &str) -> PathBuf {
        let root = self
            .inner
            .lock()
            .ok()
            .map(|state| state.registry.state_dir().join("plugins/staging"))
            .unwrap_or_else(|| std::env::temp_dir().join("hagency-plugin-staging"));
        let path = root.join(format!("{label}-{}-{}", std::process::id(), now_ms()));
        let _ = fs::create_dir_all(&path);
        path
    }
    fn validate_live_revision(&self, record: &PluginRecord) -> Result<(), PluginError> {
        if matches!(&record.source, PluginSource::Linked { .. }) {
            let (_, _, snapshot) = package_manifest(&record.install_dir)?;
            if snapshot.digest != record.active_package_sha256 {
                return Err(PluginError::Untrusted);
            }
        }
        Ok(())
    }
    fn lease_count(&self, id: &str, digest: &str) -> Result<u32, PluginError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| PluginError::NotReady("manager lock poisoned".into()))?
            .leases
            .get(&(id.into(), digest.into()))
            .copied()
            .unwrap_or(0))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvocationResult {
    pub invocation_id: String,
    pub value: serde_json::Value,
    pub stderr: String,
}

fn validate_compatibility(manifest: &Manifest) -> Result<(), PluginError> {
    let current =
        semver::Version::parse(CURRENT_HAGENCY_VERSION).map_err(|_| PluginError::Incompatible)?;
    if current < manifest.min_hagency_version
        || !manifest
            .platforms
            .iter()
            .any(|platform| platform == current_platform())
    {
        return Err(PluginError::Incompatible);
    }
    Ok(())
}
fn current_platform() -> &'static str {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x86_64"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "linux-aarch64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "macos-x86_64"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "macos-arm64"
    } else {
        "windows-x86_64"
    }
}
fn sha256(bytes: &[u8]) -> String {
    digest_bytes(bytes)
}
pub(crate) fn digest_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "plugin install refuses symlinks",
            ));
        }
        if metadata.is_dir() {
            copy_tree(&source_path, &destination_path)?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &destination_path)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "plugin tree contains unsupported entry",
            ));
        }
    }
    Ok(())
}

fn sync_tree(root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            sync_tree(&path)?;
        } else if metadata.is_file() {
            fs::File::open(&path)?.sync_all()?;
        }
    }
    fs::File::open(root)?.sync_all()
}
