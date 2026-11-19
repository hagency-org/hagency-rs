use crate::source::PluginSource;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

pub const REGISTRY_PROFILE: &str = "hagency_plugin_registry_v2";
const LEGACY_REGISTRY_PROFILE: &str = "hagency_plugin_registry_v1";

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("plugin registry is not ready: {0}")]
    NotReady(String),
    #[error("plugin registry IO failed: {0}")]
    Io(#[from] io::Error),
    #[error("plugin registry JSON is malformed: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRevision {
    pub package_sha256: String,
    pub source_sha256: String,
    pub manifest_version: String,
    pub install_dir: PathBuf,
    pub capabilities: Vec<String>,
    pub activated_at_ms: u64,
    #[serde(default)]
    pub active_leases: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRecord {
    pub id: String,
    pub source: PluginSource,
    pub enabled: bool,
    pub active_package_sha256: String,
    pub revisions: BTreeMap<String, PluginRevision>,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub restart_required: bool,
    // Active-revision projection retained for existing execution callers. The
    // immutable source of truth is `revisions[active_package_sha256]`.
    pub manifest_version: String,
    pub source_sha256: String,
    pub package_sha256: String,
    pub install_dir: PathBuf,
    pub capabilities: Vec<String>,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub active_leases: u32,
    #[serde(default)]
    pub readiness: String,
    #[serde(default)]
    pub settings_sha256: String,
    #[serde(default)]
    pub settings_revision: u64,
    #[serde(default)]
    pub settings_ready: bool,
    #[serde(default)]
    pub missing_required: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryFile {
    profile: String,
    records: BTreeMap<String, PluginRecord>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyPluginRecord {
    id: String,
    source: PluginSource,
    manifest_version: String,
    source_sha256: String,
    package_sha256: String,
    install_dir: PathBuf,
    enabled: bool,
    capabilities: Vec<String>,
    updated_at_ms: u64,
    #[serde(default)]
    active_leases: u32,
    #[serde(default)]
    readiness: String,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacyRegistryFile {
    profile: String,
    records: BTreeMap<String, LegacyPluginRecord>,
}

#[derive(Debug, Clone)]
pub struct Registry {
    state_dir: PathBuf,
    path: PathBuf,
    inner: RegistryFile,
}

impl Registry {
    pub fn open(state_dir: impl AsRef<Path>) -> Result<Self, RegistryError> {
        let state_dir = state_dir.as_ref().to_path_buf();
        let plugins = state_dir.join("plugins");
        fs::create_dir_all(plugins.join("installs"))?;
        fs::create_dir_all(plugins.join("config"))?;
        fs::create_dir_all(plugins.join("state"))?;
        let path = plugins.join("registry.json");
        let (mut inner, mut migrated) = if path.exists() {
            let bytes = fs::read(&path)?;
            let value: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|error| RegistryError::NotReady(error.to_string()))?;
            let profile = value.get("profile").and_then(serde_json::Value::as_str);
            match profile {
                Some(REGISTRY_PROFILE) => (serde_json::from_value(value)?, false),
                Some(LEGACY_REGISTRY_PROFILE) => {
                    let legacy: LegacyRegistryFile = serde_json::from_value(value)?;
                    if legacy.profile != LEGACY_REGISTRY_PROFILE {
                        return Err(RegistryError::NotReady(
                            "unsupported registry profile".into(),
                        ));
                    }
                    let records = legacy
                        .records
                        .into_iter()
                        .map(|(id, old)| {
                            let revision = PluginRevision {
                                package_sha256: old.package_sha256.clone(),
                                source_sha256: old.source_sha256.clone(),
                                manifest_version: old.manifest_version.clone(),
                                install_dir: old.install_dir.clone(),
                                capabilities: old.capabilities.clone(),
                                activated_at_ms: old.updated_at_ms,
                                active_leases: old.active_leases,
                            };
                            let record = PluginRecord {
                                id: old.id,
                                source: old.source,
                                enabled: old.enabled,
                                active_package_sha256: old.package_sha256.clone(),
                                revisions: BTreeMap::from([(old.package_sha256.clone(), revision)]),
                                stale: false,
                                restart_required: false,
                                manifest_version: old.manifest_version,
                                source_sha256: old.source_sha256,
                                package_sha256: old.package_sha256,
                                install_dir: old.install_dir,
                                capabilities: old.capabilities,
                                updated_at_ms: old.updated_at_ms,
                                active_leases: old.active_leases,
                                readiness: old.readiness,
                                settings_sha256: String::new(),
                                settings_revision: 0,
                                settings_ready: true,
                                missing_required: Vec::new(),
                            };
                            (id, record)
                        })
                        .collect();
                    (
                        RegistryFile {
                            profile: REGISTRY_PROFILE.into(),
                            records,
                        },
                        true,
                    )
                }
                _ => {
                    return Err(RegistryError::NotReady(
                        "unsupported registry profile".into(),
                    ));
                }
            }
        } else {
            (
                RegistryFile {
                    profile: REGISTRY_PROFILE.into(),
                    records: BTreeMap::new(),
                },
                true,
            )
        };
        for record in inner.records.values_mut() {
            if record.active_leases != 0 {
                record.active_leases = 0;
                migrated = true;
            }
            for revision in record.revisions.values_mut() {
                if revision.active_leases != 0 {
                    revision.active_leases = 0;
                    migrated = true;
                }
            }
        }
        let registry = Self {
            state_dir,
            path,
            inner,
        };
        if migrated {
            registry.persist()?;
        }
        Ok(registry)
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn records(&self) -> impl Iterator<Item = &PluginRecord> {
        self.inner.records.values()
    }

    pub fn get(&self, id: &str) -> Option<&PluginRecord> {
        self.inner.records.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut PluginRecord> {
        self.inner.records.get_mut(id)
    }

    pub fn insert(&mut self, mut record: PluginRecord) -> Result<(), RegistryError> {
        let mut inner = self.inner.clone();
        if let Some(previous) = inner.records.get(&record.id).cloned() {
            let changed = previous.active_package_sha256 != record.package_sha256;
            record.enabled = previous.enabled;
            record.restart_required = previous.enabled && changed;
            record.stale = false;
            record.readiness = if record.restart_required {
                "restart_required".into()
            } else if record.enabled {
                "ready".into()
            } else {
                "disabled".into()
            };
            record.revisions.extend(previous.revisions);
        }
        record
            .revisions
            .entry(record.package_sha256.clone())
            .or_insert_with(|| PluginRevision {
                package_sha256: record.package_sha256.clone(),
                source_sha256: record.source_sha256.clone(),
                manifest_version: record.manifest_version.clone(),
                install_dir: record.install_dir.clone(),
                capabilities: record.capabilities.clone(),
                activated_at_ms: record.updated_at_ms,
                active_leases: 0,
            });
        record.active_package_sha256 = record.package_sha256.clone();
        inner.records.insert(record.id.clone(), record);
        persist_file(&self.path, &inner)?;
        self.inner = inner;
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Result<Option<PluginRecord>, RegistryError> {
        let mut inner = self.inner.clone();
        let removed = inner.records.remove(id);
        persist_file(&self.path, &inner)?;
        self.inner = inner;
        Ok(removed)
    }

    pub fn persist(&self) -> Result<(), RegistryError> {
        persist_file(&self.path, &self.inner)
    }
}

fn persist_file(path: &Path, inner: &RegistryFile) -> Result<(), RegistryError> {
    let bytes = serde_json::to_vec_pretty(inner)?;
    let temp = path.with_extension(format!("json.tmp-{}-{}", std::process::id(), now_ms()));
    {
        use std::io::Write;
        let mut file = fs::File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temp, path)?;
    sync_parent(path)?;
    Ok(())
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn sync_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        let file = fs::File::open(parent)?;
        file.sync_all()?;
    }
    Ok(())
}
