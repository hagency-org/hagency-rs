use crate::manifest::{SettingKind, SettingSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

pub const SETTINGS_SCHEMA: &str = "hagency.plugin.settings/v1";
const MAX_SETTINGS_JSON_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("plugin settings are invalid: {0}")]
    Invalid(String),
    #[error("plugin settings revision conflicts with the current revision")]
    Conflict { current: u64 },
    #[error("plugin settings IO failed: {0}")]
    Io(#[from] io::Error),
    #[error("plugin settings JSON is malformed: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginSettingsDocument {
    pub schema: String,
    pub plugin_id: String,
    pub package_sha256: String,
    pub revision: u64,
    pub settings_sha256: String,
    pub values: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSettingsState {
    pub definitions: Vec<SettingSpec>,
    pub document: PluginSettingsDocument,
    pub settings_ready: bool,
    pub missing_required: Vec<String>,
}

pub fn evaluate(
    plugin_id: &str,
    package_sha256: &str,
    revision: u64,
    definitions: &[SettingSpec],
    supplied: &BTreeMap<String, Value>,
    require_complete: bool,
) -> Result<PluginSettingsState, SettingsError> {
    let (values, missing_required) = resolve_values(definitions, supplied, require_complete)?;
    let settings_sha256 = digest(plugin_id, package_sha256, &values)?;
    let document = PluginSettingsDocument {
        schema: SETTINGS_SCHEMA.into(),
        plugin_id: plugin_id.into(),
        package_sha256: package_sha256.into(),
        revision,
        settings_sha256,
        values,
    };
    Ok(PluginSettingsState {
        definitions: definitions.to_vec(),
        document,
        settings_ready: missing_required.is_empty(),
        missing_required,
    })
}

pub fn resolve_values(
    definitions: &[SettingSpec],
    supplied: &BTreeMap<String, Value>,
    require_complete: bool,
) -> Result<(BTreeMap<String, Value>, Vec<String>), SettingsError> {
    let allowed: BTreeSet<&str> = definitions
        .iter()
        .map(|setting| setting.id.as_str())
        .collect();
    if let Some(key) = supplied.keys().find(|key| !allowed.contains(key.as_str())) {
        return Err(SettingsError::Invalid(format!("unknown setting {key}")));
    }
    let mut values = BTreeMap::new();
    let mut missing_required = Vec::new();
    for setting in definitions {
        let value = supplied
            .get(&setting.id)
            .cloned()
            .or_else(|| setting.default.clone());
        match value {
            Some(value) => {
                validate_value(setting, &value)?;
                values.insert(setting.id.clone(), value);
            }
            None if setting.required => missing_required.push(setting.id.clone()),
            None => {}
        }
    }
    if require_complete && !missing_required.is_empty() {
        return Err(SettingsError::Invalid(format!(
            "missing required settings: {}",
            missing_required.join(", ")
        )));
    }
    Ok((values, missing_required))
}

pub fn digest(
    plugin_id: &str,
    package_sha256: &str,
    values: &BTreeMap<String, Value>,
) -> Result<String, SettingsError> {
    let canonical = serde_json::json!({
        "packageSha256": package_sha256,
        "pluginId": plugin_id,
        "schema": SETTINGS_SCHEMA,
        "values": values,
    });
    let bytes = serde_json::to_vec(&canonical)?;
    Ok(crate::digest_bytes(&bytes))
}

pub fn validate_document(
    document: &PluginSettingsDocument,
    plugin_id: &str,
    package_sha256: &str,
    definitions: &[SettingSpec],
) -> Result<(), SettingsError> {
    if document.schema != SETTINGS_SCHEMA
        || document.plugin_id != plugin_id
        || document.package_sha256 != package_sha256
    {
        return Err(SettingsError::Invalid(
            "settings document identity mismatch".into(),
        ));
    }
    let state = evaluate(
        plugin_id,
        package_sha256,
        document.revision,
        definitions,
        &document.values,
        false,
    )?;
    if state.document.settings_sha256 != document.settings_sha256 {
        return Err(SettingsError::Invalid("settings digest mismatch".into()));
    }
    Ok(())
}

pub fn load_current(root: &Path) -> Result<Option<PluginSettingsDocument>, SettingsError> {
    let path = root.join("current.json");
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    if bytes.len() > MAX_SETTINGS_JSON_BYTES {
        return Err(SettingsError::Invalid(
            "settings document exceeds its capacity".into(),
        ));
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}

pub fn persist(root: &Path, document: &PluginSettingsDocument) -> Result<PathBuf, SettingsError> {
    let bytes = serde_json::to_vec_pretty(document)?;
    if bytes.len() > MAX_SETTINGS_JSON_BYTES {
        return Err(SettingsError::Invalid(
            "settings document exceeds its capacity".into(),
        ));
    }
    fs::create_dir_all(root.join("revisions"))?;
    private_dir(root)?;
    private_dir(&root.join("revisions"))?;
    let revision_dir = root.join("revisions").join(&document.settings_sha256);
    if revision_dir.exists() {
        let existing = fs::read(revision_dir.join("settings.json"))?;
        if existing != bytes {
            return Err(SettingsError::Invalid(
                "immutable settings revision changed".into(),
            ));
        }
    } else {
        fs::create_dir(&revision_dir)?;
        private_dir(&revision_dir)?;
        atomic_write(&revision_dir.join("settings.json"), &bytes)?;
    }
    atomic_write(&root.join("current.json"), &bytes)?;
    Ok(revision_dir)
}

pub fn revision_dir(root: &Path, settings_sha256: &str) -> PathBuf {
    root.join("revisions").join(settings_sha256)
}

fn validate_value(setting: &SettingSpec, value: &Value) -> Result<(), SettingsError> {
    match setting.kind {
        SettingKind::Text => {
            let value = value
                .as_str()
                .ok_or_else(|| SettingsError::Invalid(format!("{} must be text", setting.id)))?;
            if value.len() > 4096 || value.chars().any(char::is_control) {
                return Err(SettingsError::Invalid(format!(
                    "{} text is out of bounds",
                    setting.id
                )));
            }
        }
        SettingKind::Textarea => {
            let value = value
                .as_str()
                .ok_or_else(|| SettingsError::Invalid(format!("{} must be text", setting.id)))?;
            let max_length = setting.max_length.ok_or_else(|| {
                SettingsError::Invalid(format!("{} textarea has no maximum length", setting.id))
            })?;
            if value.len() > max_length
                || value.chars().any(|character| {
                    character.is_control() && !matches!(character, '\n' | '\r' | '\t')
                })
            {
                return Err(SettingsError::Invalid(format!(
                    "{} textarea is out of bounds",
                    setting.id
                )));
            }
        }
        SettingKind::Integer => {
            let value = value.as_i64().ok_or_else(|| {
                SettingsError::Invalid(format!("{} must be an integer", setting.id))
            })?;
            if setting.min.is_some_and(|min| value < min)
                || setting.max.is_some_and(|max| value > max)
            {
                return Err(SettingsError::Invalid(format!(
                    "{} integer is out of bounds",
                    setting.id
                )));
            }
        }
        SettingKind::Boolean => {
            if !value.is_boolean() {
                return Err(SettingsError::Invalid(format!(
                    "{} must be boolean",
                    setting.id
                )));
            }
        }
        SettingKind::Select => {
            let value = value.as_str().ok_or_else(|| {
                SettingsError::Invalid(format!("{} must be a selection", setting.id))
            })?;
            if !setting.options.iter().any(|option| option == value) {
                return Err(SettingsError::Invalid(format!(
                    "{} selection is invalid",
                    setting.id
                )));
            }
        }
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), io::Error> {
    let temp = path.with_extension(format!(
        "json.tmp-{}-{}",
        std::process::id(),
        crate::registry::now_ms()
    ));
    {
        use io::Write;
        let mut file = fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    private_file(&temp)?;
    fs::rename(&temp, path)?;
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn private_dir(path: &Path) -> Result<(), io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn private_file(path: &Path) -> Result<(), io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
