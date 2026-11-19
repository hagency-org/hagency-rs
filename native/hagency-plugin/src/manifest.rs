use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Component, Path};
use thiserror::Error;

pub const MANIFEST_FILE: &str = "hagency-plugin.toml";
pub const SCHEMA: &str = "hagency.plugin/v1";
pub const RUNTIME_PROTOCOL: &str = "hagency.runtime/v1";
pub const HARNESS_PROTOCOL: &str = "hagency.harness/v1";
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
pub const MAX_ENTRYPOINTS_PER_KIND: usize = 16;
pub const MAX_ENTRYPOINTS_TOTAL: usize = 64;
pub const MAX_ARGV_ITEMS: usize = 64;
pub const MAX_ARGV_BYTES: usize = 8 * 1024;
pub const MAX_SCHEMA_BYTES: usize = 1024 * 1024;
pub const MAX_SETTINGS: usize = 32;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ManifestError {
    #[error("manifest exceeds its capacity limit")]
    Capacity,
    #[error("manifest is not valid TOML: {0}")]
    Toml(String),
    #[error("manifest has an unsupported or missing field: {0}")]
    Field(String),
    #[error("manifest has an invalid {field}: {reason}")]
    Invalid { field: String, reason: String },
    #[error("manifest entrypoint escapes the package root: {0}")]
    PathEscape(String),
    #[error("manifest entrypoint is not a relative regular file: {0}")]
    Entrypoint(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingKind {
    Text,
    Textarea,
    Integer,
    Boolean,
    Select,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingSpec {
    pub id: String,
    pub label: String,
    pub description: String,
    pub kind: SettingKind,
    pub required: bool,
    pub default: Option<Value>,
    pub options: Vec<String>,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub max_length: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub schema: String,
    pub id: String,
    pub name: String,
    pub version: Version,
    pub min_hagency_version: Version,
    pub platforms: Vec<String>,
    pub builds: Vec<BuildSpec>,
    pub startup: Vec<StartupSpec>,
    pub actions: Vec<ActionSpec>,
    pub events: Vec<EventSpec>,
    pub tools: Vec<ToolSpec>,
    pub runtimes: Vec<RuntimeSpec>,
    pub harnesses: Vec<HarnessSpec>,
    pub panes: Vec<PaneSpec>,
    pub settings: Vec<SettingSpec>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuildSpec {
    pub command: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupSpec {
    pub id: String,
    pub command: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionSpec {
    pub id: String,
    pub title: String,
    pub command: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct EventSpec {
    pub on: String,
    pub command: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpec {
    pub id: String,
    pub title: String,
    pub description: String,
    pub input_schema: Value,
    pub command: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSpec {
    pub id: String,
    pub framework: String,
    pub command: Vec<String>,
    pub protocol: String,
    /// An optional read-only readiness probe the host runs before it starts a
    /// dispatch: exit 0 means the runtime's provider is ready. It must have no
    /// side effects; a refusal requeues the dispatch as never started.
    pub readiness: Option<Vec<String>>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessSpec {
    pub id: String,
    pub command: Vec<String>,
    pub protocol: String,
    pub configuration: Vec<SettingSpec>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneSpec {
    pub id: String,
    pub title: String,
    pub harness: String,
    pub command: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    schema: String,
    id: String,
    name: String,
    version: String,
    min_hagency_version: String,
    platforms: Vec<String>,
    #[serde(default)]
    build: Vec<RawCommand>,
    #[serde(default)]
    startup: Vec<RawStartup>,
    #[serde(default)]
    actions: Vec<RawAction>,
    #[serde(default)]
    events: Vec<RawEvent>,
    #[serde(default)]
    tools: Vec<RawTool>,
    #[serde(default)]
    runtimes: Vec<RawRuntime>,
    #[serde(default)]
    harnesses: Vec<RawHarness>,
    #[serde(default)]
    panes: Vec<RawPane>,
    #[serde(default)]
    settings: Vec<RawSetting>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    command: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStartup {
    id: String,
    command: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAction {
    id: String,
    title: String,
    command: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvent {
    on: String,
    command: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTool {
    id: String,
    title: String,
    description: String,
    input_schema: Value,
    command: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRuntime {
    id: String,
    framework: String,
    command: Vec<String>,
    protocol: String,
    #[serde(default)]
    readiness: Option<Vec<String>>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHarness {
    id: String,
    command: Vec<String>,
    protocol: String,
    #[serde(default)]
    configuration: Vec<RawSetting>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPane {
    id: String,
    title: String,
    harness: String,
    command: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSetting {
    id: String,
    label: String,
    #[serde(default)]
    description: String,
    kind: SettingKind,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    default: Option<Value>,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    min: Option<i64>,
    #[serde(default)]
    max: Option<i64>,
    #[serde(default)]
    max_length: Option<usize>,
}

impl Manifest {
    /// Parse a complete manifest and reject unknown fields before any command runs.
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::Capacity);
        }
        let raw: RawManifest =
            toml::from_slice(bytes).map_err(|error| ManifestError::Toml(error.to_string()))?;
        if raw.schema != SCHEMA {
            return Err(ManifestError::Field("schema".into()));
        }
        let id = validate_id(&raw.id, "id", 128)?;
        let name = nonempty(raw.name, "name")?;
        let version = parse_version(&raw.version, "version")?;
        let min_hagency_version = parse_version(&raw.min_hagency_version, "min_hagency_version")?;
        if raw.platforms.is_empty() || raw.platforms.len() > 16 {
            return Err(ManifestError::Invalid {
                field: "platforms".into(),
                reason: "must contain 1..16 values".into(),
            });
        }
        let mut platforms = Vec::with_capacity(raw.platforms.len());
        for platform in raw.platforms {
            let platform = nonempty(platform, "platform")?;
            if !matches!(
                platform.as_str(),
                "linux-x86_64"
                    | "linux-aarch64"
                    | "macos-x86_64"
                    | "macos-arm64"
                    | "windows-x86_64"
            ) {
                return Err(ManifestError::Invalid {
                    field: "platforms".into(),
                    reason: format!("unsupported platform {platform}"),
                });
            }
            if platforms.contains(&platform) {
                return Err(ManifestError::Invalid {
                    field: "platforms".into(),
                    reason: "duplicate platform".into(),
                });
            }
            platforms.push(platform);
        }

        let builds = raw
            .build
            .into_iter()
            .map(|v| {
                Ok(BuildSpec {
                    command: validate_command(v.command, "build", false)?,
                })
            })
            .collect::<Result<Vec<_>, ManifestError>>()?;
        let startup = raw
            .startup
            .into_iter()
            .map(|v| {
                Ok(StartupSpec {
                    id: validate_entry_id(&v.id)?,
                    command: validate_command(v.command, "startup", true)?,
                })
            })
            .collect::<Result<Vec<_>, ManifestError>>()?;
        let actions = raw
            .actions
            .into_iter()
            .map(|v| {
                Ok(ActionSpec {
                    id: validate_entry_id(&v.id)?,
                    title: nonempty(v.title, "actions.title")?,
                    command: validate_command(v.command, "actions.command", true)?,
                })
            })
            .collect::<Result<Vec<_>, ManifestError>>()?;
        let events = raw
            .events
            .into_iter()
            .map(|v| {
                Ok(EventSpec {
                    on: validate_event(&v.on)?,
                    command: validate_command(v.command, "events.command", true)?,
                })
            })
            .collect::<Result<Vec<_>, ManifestError>>()?;
        let tools = raw
            .tools
            .into_iter()
            .map(|v| {
                let schema = v.input_schema;
                let encoded = serde_json::to_vec(&schema).map_err(|e| ManifestError::Invalid {
                    field: "tools.input_schema".into(),
                    reason: e.to_string(),
                })?;
                if encoded.len() > MAX_SCHEMA_BYTES {
                    return Err(ManifestError::Capacity);
                }
                Ok(ToolSpec {
                    id: validate_entry_id(&v.id)?,
                    title: nonempty(v.title, "tools.title")?,
                    description: bounded_text(
                        v.description,
                        "tools.description",
                        MAX_SCHEMA_BYTES,
                    )?,
                    input_schema: schema,
                    command: validate_command(v.command, "tools.command", true)?,
                })
            })
            .collect::<Result<Vec<_>, ManifestError>>()?;
        let runtimes = raw
            .runtimes
            .into_iter()
            .map(|v| {
                if v.protocol != RUNTIME_PROTOCOL {
                    return Err(ManifestError::Field("runtimes.protocol".into()));
                }
                Ok(RuntimeSpec {
                    id: validate_entry_id(&v.id)?,
                    framework: nonempty(v.framework, "runtimes.framework")?,
                    command: validate_command(v.command, "runtimes.command", true)?,
                    protocol: v.protocol,
                    readiness: v
                        .readiness
                        .map(|argv| validate_command(argv, "runtimes.readiness", true))
                        .transpose()?,
                })
            })
            .collect::<Result<Vec<_>, ManifestError>>()?;
        let harnesses = raw
            .harnesses
            .into_iter()
            .map(|v| {
                if v.protocol != HARNESS_PROTOCOL {
                    return Err(ManifestError::Field("harnesses.protocol".into()));
                }
                let id = validate_entry_id(&v.id)?;
                if v.configuration.len() > MAX_SETTINGS {
                    return Err(ManifestError::Invalid {
                        field: format!("harnesses.{id}.configuration"),
                        reason: "at most 32 entries".into(),
                    });
                }
                let configuration = v
                    .configuration
                    .into_iter()
                    .map(|setting| {
                        validate_setting(setting, &format!("harnesses.{id}.configuration"))
                    })
                    .collect::<Result<Vec<_>, ManifestError>>()?;
                ensure_unique(
                    &format!("harnesses.{id}.configuration"),
                    configuration.iter().map(|setting| setting.id.as_str()),
                )?;
                Ok(HarnessSpec {
                    id,
                    command: validate_command(v.command, "harnesses.command", true)?,
                    protocol: v.protocol,
                    configuration,
                })
            })
            .collect::<Result<Vec<_>, ManifestError>>()?;

        let panes = raw
            .panes
            .into_iter()
            .map(|v| {
                Ok(PaneSpec {
                    id: validate_entry_id(&v.id)?,
                    title: nonempty(v.title, "panes.title")?,
                    harness: validate_entry_id(&v.harness)?,
                    command: validate_command(v.command, "panes.command", true)?,
                })
            })
            .collect::<Result<Vec<_>, ManifestError>>()?;

        let settings = raw
            .settings
            .into_iter()
            .map(|setting| validate_setting(setting, "settings"))
            .collect::<Result<Vec<_>, ManifestError>>()?;
        if settings.len() > MAX_SETTINGS {
            return Err(ManifestError::Invalid {
                field: "settings".into(),
                reason: "at most 32 entries".into(),
            });
        }
        ensure_unique("settings", settings.iter().map(|v| v.id.as_str()))?;

        for (label, count) in [
            ("build", builds.len()),
            ("startup", startup.len()),
            ("actions", actions.len()),
            ("events", events.len()),
            ("tools", tools.len()),
            ("runtimes", runtimes.len()),
            ("harnesses", harnesses.len()),
            ("panes", panes.len()),
        ] {
            if count > MAX_ENTRYPOINTS_PER_KIND {
                return Err(ManifestError::Invalid {
                    field: label.into(),
                    reason: "at most 16 entries".into(),
                });
            }
        }
        let total = builds.len()
            + startup.len()
            + actions.len()
            + events.len()
            + tools.len()
            + runtimes.len()
            + harnesses.len()
            + panes.len();
        if total > MAX_ENTRYPOINTS_TOTAL {
            return Err(ManifestError::Invalid {
                field: "entrypoints".into(),
                reason: "at most 64 entries".into(),
            });
        }
        ensure_unique("startup", startup.iter().map(|v| v.id.as_str()))?;
        ensure_unique("actions", actions.iter().map(|v| v.id.as_str()))?;
        ensure_unique("tools", tools.iter().map(|v| v.id.as_str()))?;
        ensure_unique("runtimes", runtimes.iter().map(|v| v.id.as_str()))?;
        ensure_unique("harnesses", harnesses.iter().map(|v| v.id.as_str()))?;
        ensure_unique("panes", panes.iter().map(|v| v.id.as_str()))?;
        let mut commands = Vec::new();
        commands.extend(
            startup
                .iter()
                .map(|v| ("startup", v.id.as_str(), v.command.as_slice())),
        );
        commands.extend(
            actions
                .iter()
                .map(|v| ("action", v.id.as_str(), v.command.as_slice())),
        );
        commands.extend(
            events
                .iter()
                .map(|v| ("event", v.on.as_str(), v.command.as_slice())),
        );
        commands.extend(
            tools
                .iter()
                .map(|v| ("tool", v.id.as_str(), v.command.as_slice())),
        );
        commands.extend(
            runtimes
                .iter()
                .map(|v| ("runtime", v.id.as_str(), v.command.as_slice())),
        );
        commands.extend(
            harnesses
                .iter()
                .map(|v| ("harness", v.id.as_str(), v.command.as_slice())),
        );
        commands.extend(
            panes
                .iter()
                .map(|v| ("pane", v.id.as_str(), v.command.as_slice())),
        );
        for (kind, id, command) in commands {
            if let Some(first) = command.first() {
                if matches!(kind, "runtime" | "harness")
                    && (Path::new(first).is_absolute() || has_parent_component(Path::new(first)))
                {
                    return Err(ManifestError::PathEscape(first.clone()));
                }
                if first.is_empty() {
                    return Err(ManifestError::Entrypoint(format!("{kind}:{id}")));
                }
            }
        }
        Ok(Self {
            schema: SCHEMA.into(),
            id,
            name,
            version,
            min_hagency_version,
            platforms,
            builds,
            startup,
            actions,
            events,
            tools,
            runtimes,
            harnesses,
            panes,
            settings,
        })
    }

    pub fn capabilities(&self) -> Vec<String> {
        let mut capabilities = Vec::new();
        if !self.startup.is_empty() {
            capabilities.push("startup.v1".into());
        }
        if !self.actions.is_empty() {
            capabilities.push("action.v1".into());
        }
        if !self.events.is_empty() {
            capabilities.push("event.v1".into());
        }
        if !self.tools.is_empty() {
            capabilities.push("tool.v1".into());
        }
        if !self.runtimes.is_empty() {
            capabilities.push("runtime.v1".into());
        }
        if !self.harnesses.is_empty() {
            capabilities.push("harness.v1".into());
        }
        if !self.panes.is_empty() {
            capabilities.push("pane.v1".into());
        }
        if !self.settings.is_empty() {
            capabilities.push("settings.v1".into());
        }
        capabilities
    }

    pub fn validate_package_root(&self, root: &Path) -> Result<(), ManifestError> {
        for command in self
            .runtimes
            .iter()
            .map(|entry| &entry.command)
            .chain(
                self.runtimes
                    .iter()
                    .filter_map(|entry| entry.readiness.as_ref()),
            )
            .chain(self.harnesses.iter().map(|entry| &entry.command))
        {
            let path = root.join(&command[0]);
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|_| ManifestError::Entrypoint(command[0].clone()))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ManifestError::Entrypoint(command[0].clone()));
            }
        }
        Ok(())
    }
}

fn validate_setting(raw: RawSetting, field_prefix: &str) -> Result<SettingSpec, ManifestError> {
    let id = validate_entry_id(&raw.id)?;
    let field = format!("{field_prefix}.{id}");
    let label = bounded_text(raw.label, &format!("{field}.label"), 128)?;
    let description =
        bounded_text_allow_empty(raw.description, &format!("{field}.description"), 512)?;
    if raw.min.zip(raw.max).is_some_and(|(min, max)| min > max) {
        return Err(ManifestError::Invalid {
            field: format!("{field}.min_max"),
            reason: "min must not exceed max".into(),
        });
    }
    if raw.options.len() > 32 {
        return Err(ManifestError::Invalid {
            field: format!("{field}.options"),
            reason: "must contain at most 32 values".into(),
        });
    }
    for option in &raw.options {
        if option.is_empty() || option.len() > 128 || option.chars().any(char::is_control) {
            return Err(ManifestError::Invalid {
                field: format!("{field}.options"),
                reason: "options must be 1..128 bytes without control characters".into(),
            });
        }
    }
    ensure_unique(
        &format!("{field}.options"),
        raw.options.iter().map(String::as_str),
    )?;
    match raw.kind {
        SettingKind::Text => {
            if raw.max_length.is_some()
                || !raw.options.is_empty()
                || raw.min.is_some()
                || raw.max.is_some()
            {
                return Err(ManifestError::Invalid {
                    field,
                    reason: "text settings cannot declare max length, options, or numeric bounds"
                        .into(),
                });
            }
            if let Some(default) = &raw.default {
                let value = default.as_str().ok_or_else(|| ManifestError::Invalid {
                    field: format!("{field}.default"),
                    reason: "must be text".into(),
                })?;
                validate_text(value, &format!("{field}.default"))?;
            }
        }
        SettingKind::Textarea => {
            if !raw.options.is_empty() || raw.min.is_some() || raw.max.is_some() {
                return Err(ManifestError::Invalid {
                    field,
                    reason: "textarea settings cannot declare options or numeric bounds".into(),
                });
            }
            let max_length = raw.max_length.ok_or_else(|| ManifestError::Invalid {
                field: format!("{field}.max_length"),
                reason: "textarea settings require maxLength in 1..32768".into(),
            })?;
            if !(1..=32768).contains(&max_length) {
                return Err(ManifestError::Invalid {
                    field: format!("{field}.max_length"),
                    reason: "must be in 1..32768".into(),
                });
            }
            if let Some(default) = &raw.default {
                let value = default.as_str().ok_or_else(|| ManifestError::Invalid {
                    field: format!("{field}.default"),
                    reason: "must be text".into(),
                })?;
                validate_textarea(value, &format!("{field}.default"), max_length)?;
            }
        }
        SettingKind::Integer => {
            if raw.max_length.is_some() || !raw.options.is_empty() {
                return Err(ManifestError::Invalid {
                    field: format!("{field}.options"),
                    reason: "integer settings cannot declare max length or options".into(),
                });
            }
            if let Some(default) = &raw.default {
                let value = default.as_i64().ok_or_else(|| ManifestError::Invalid {
                    field: format!("{field}.default"),
                    reason: "must be a signed 64-bit integer".into(),
                })?;
                validate_integer(value, raw.min, raw.max, &format!("{field}.default"))?;
            }
        }
        SettingKind::Boolean => {
            if raw.max_length.is_some()
                || !raw.options.is_empty()
                || raw.min.is_some()
                || raw.max.is_some()
            {
                return Err(ManifestError::Invalid {
                    field,
                    reason:
                        "boolean settings cannot declare max length, options, or numeric bounds"
                            .into(),
                });
            }
            if raw
                .default
                .as_ref()
                .is_some_and(|default| !default.is_boolean())
            {
                return Err(ManifestError::Invalid {
                    field: format!("{field}.default"),
                    reason: "must be boolean".into(),
                });
            }
        }
        SettingKind::Select => {
            if raw.max_length.is_some()
                || raw.options.is_empty()
                || raw.min.is_some()
                || raw.max.is_some()
            {
                return Err(ManifestError::Invalid {
                    field,
                    reason: "select settings require options and cannot declare max length or numeric bounds".into(),
                });
            }
            if let Some(default) = &raw.default {
                let value = default.as_str().ok_or_else(|| ManifestError::Invalid {
                    field: format!("{field}.default"),
                    reason: "must be one of the options".into(),
                })?;
                if !raw.options.iter().any(|option| option == value) {
                    return Err(ManifestError::Invalid {
                        field: format!("{field}.default"),
                        reason: "must be one of the options".into(),
                    });
                }
            }
        }
    }
    Ok(SettingSpec {
        id,
        label,
        description,
        kind: raw.kind,
        required: raw.required,
        default: raw.default,
        options: raw.options,
        min: raw.min,
        max: raw.max,
        max_length: raw.max_length,
    })
}

fn validate_text(value: &str, field: &str) -> Result<(), ManifestError> {
    if value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(ManifestError::Invalid {
            field: field.into(),
            reason: "text must be at most 4096 bytes without control characters".into(),
        });
    }
    Ok(())
}

fn validate_textarea(value: &str, field: &str, max_length: usize) -> Result<(), ManifestError> {
    if value.len() > max_length
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(ManifestError::Invalid {
            field: field.into(),
            reason: format!(
                "textarea must be at most {max_length} bytes and may contain only newline, carriage return, and tab controls"
            ),
        });
    }
    Ok(())
}

fn validate_integer(
    value: i64,
    min: Option<i64>,
    max: Option<i64>,
    id: &str,
) -> Result<(), ManifestError> {
    if min.is_some_and(|bound| value < bound) || max.is_some_and(|bound| value > bound) {
        return Err(ManifestError::Invalid {
            field: format!("settings.{id}.default"),
            reason: "integer default is outside min/max".into(),
        });
    }
    Ok(())
}

fn nonempty(value: String, field: &str) -> Result<String, ManifestError> {
    if value.trim().is_empty() {
        Err(ManifestError::Invalid {
            field: field.into(),
            reason: "must not be empty".into(),
        })
    } else {
        Ok(value)
    }
}
fn bounded_text_allow_empty(
    value: String,
    _field: &str,
    max: usize,
) -> Result<String, ManifestError> {
    if value.len() > max {
        return Err(ManifestError::Capacity);
    }
    Ok(value)
}
fn bounded_text(value: String, field: &str, max: usize) -> Result<String, ManifestError> {
    let value = nonempty(value, field)?;
    if value.len() > max {
        return Err(ManifestError::Capacity);
    }
    Ok(value)
}
fn parse_version(value: &str, field: &str) -> Result<Version, ManifestError> {
    Version::parse(value).map_err(|_| ManifestError::Invalid {
        field: field.into(),
        reason: "must be SemVer".into(),
    })
}
fn validate_id(value: &str, field: &str, max: usize) -> Result<String, ManifestError> {
    if value.is_empty()
        || value.len() > max
        || !value.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        })
    {
        return Err(ManifestError::Invalid {
            field: field.into(),
            reason: "must be lowercase ASCII dot-separated segments".into(),
        });
    }
    Ok(value.into())
}
fn validate_entry_id(value: &str) -> Result<String, ManifestError> {
    if value.is_empty()
        || value.len() > 64
        || !value.as_bytes()[0].is_ascii_lowercase()
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return Err(ManifestError::Invalid {
            field: "entrypoint.id".into(),
            reason: "must match [a-z][a-z0-9_-]{0,63}".into(),
        });
    }
    Ok(value.into())
}
fn validate_event(value: &str) -> Result<String, ManifestError> {
    const EVENTS: [&str; 9] = [
        "agent.provisioned",
        "agent.retired",
        "dispatch.started",
        "dispatch.blocked",
        "dispatch.completed",
        "dispatch.failed",
        "approval.requested",
        "approval.resolved",
        "plugin.enabled",
    ];
    if !EVENTS.contains(&value) && value != "plugin.disabled" {
        return Err(ManifestError::Invalid {
            field: "events.on".into(),
            reason: "unsupported event".into(),
        });
    }
    Ok(value.into())
}
fn validate_command(
    command: Vec<String>,
    field: &str,
    relative_first: bool,
) -> Result<Vec<String>, ManifestError> {
    if command.is_empty() || command.len() > MAX_ARGV_ITEMS {
        return Err(ManifestError::Invalid {
            field: field.into(),
            reason: "argv must contain 1..64 elements".into(),
        });
    }
    let mut bytes = 0usize;
    for (index, value) in command.iter().enumerate() {
        if value.is_empty() || value.as_bytes().contains(&0) {
            return Err(ManifestError::Invalid {
                field: field.into(),
                reason: "argv elements must be nonempty and NUL-free".into(),
            });
        }
        bytes = bytes
            .checked_add(value.len() + 1)
            .ok_or(ManifestError::Capacity)?;
        if bytes > MAX_ARGV_BYTES {
            return Err(ManifestError::Capacity);
        }
        if relative_first
            && index == 0
            && (Path::new(value).is_absolute() || has_parent_component(Path::new(value)))
        {
            return Err(ManifestError::PathEscape(value.clone()));
        }
    }
    Ok(command)
}
fn has_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}
fn ensure_unique<'a>(
    kind: &str,
    values: impl Iterator<Item = &'a str>,
) -> Result<(), ManifestError> {
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(ManifestError::Invalid {
                field: kind.into(),
                reason: format!("duplicate entry {value}"),
            });
        }
    }
    Ok(())
}
