//! ADR-189: the console's setup page.
//!
//! `GET /console/api/setup` reports each step: the coding agents found on
//! this machine (path, version, whether and how they are signed in), whether
//! the runtime is configured, and the Palpo connection. `POST
//! /console/api/setup/check` detects again and, when a signed-in Codex, a
//! Claude Code (ADR-192) or an Octos with a qualified profile (ADR-193) is
//! found and the runtime is missing or stale, writes and validates
//! `fleet-runtime.json` with `hagency setup`'s code. Hagency never signs a
//! coding agent in; the page asks the user to sign Codex in, and assumes
//! Claude Code is signed in (ADR-192 decision 6) and Octos has its keys
//! (ADR-193 decision 8).
use super::engagements::check_lifecycle;
use super::{failed, recheck};
use crate::refusal;
use salvo::prelude::*;
use serde_json::json;

pub(super) fn router() -> Router {
    Router::with_path("setup")
        .get(status)
        .push(Router::with_path("check").post(check))
        .push(Router::with_path("resource").post(offer))
}

/// The provider each framework's resources name.
fn provider(framework: &str) -> &'static str {
    if framework == "claude" {
        "anthropic"
    } else {
        "openai"
    }
}

/// The model and reasoning pairs Hagency qualifies for one framework
/// (`role-capacity.json`): only these are published to Palpo. Claude models
/// carry no reasoning setting (ADR-192 decision 8).
fn choices(framework: &'static str) -> Vec<serde_json::Value> {
    let profile = hagency_core::qualification::ModelProfile {
        framework: framework.into(),
        model: String::new(),
        provider: Some(provider(framework).into()),
        reasoning: None,
    };
    hagency_core::qualification::configuration_choices(&profile)
        .unwrap_or_default()
        .into_iter()
        .map(|c| {
            json!({"framework": framework, "model": c.model,
                "reasoning": c.reasoning, "roles": c.roles})
        })
        .collect()
}

/// ADR-193: an Octos resource runs one of the user's profiles, so its choices
/// are the qualified profiles themselves, each with its primary model. With a
/// runtime, only the profiles it allows are offered.
fn octos_choices(
    profiles: &[crate::setup::OctosProfile],
    allowed: Option<&[String]>,
) -> Vec<serde_json::Value> {
    profiles
        .iter()
        .filter(|p| p.qualified() && allowed.is_none_or(|allowed| allowed.contains(&p.id)))
        .filter_map(|p| {
            let profile = hagency_core::qualification::ModelProfile {
                framework: "octos".into(),
                model: p.model.clone()?,
                provider: p.family.clone(),
                reasoning: None,
            };
            let roles: Vec<_> = hagency_core::qualification::roles()
                .filter(|role| hagency_core::qualification::qualifies(&profile, role, None))
                .collect();
            Some(json!({"framework": "octos", "model": profile.model,
                "provider": profile.provider, "profile": p.id, "reasoning": null,
                "roles": roles}))
        })
        .collect()
}

fn runtime(state: &std::path::Path) -> Option<serde_json::Value> {
    std::fs::read(state.join("fleet-runtime.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
}

/// The coding agents `fleet-runtime.json` configures: Codex, Claude Code and
/// Octos, in any combination (ADR-192, ADR-193).
fn runtime_frameworks(state: &std::path::Path) -> Vec<&'static str> {
    let Some(value) = runtime(state) else {
        return Vec::new();
    };
    let mut frameworks = Vec::new();
    if value["executable"].is_string() {
        frameworks.push("codex");
    }
    if value["claude"]["executable"].is_string() {
        frameworks.push("claude");
    }
    if value["octos"]["executable"].is_string() {
        frameworks.push("octos");
    }
    frameworks
}

/// The Octos profiles the runtime allows and the Octos home it reads them
/// from (ADR-193 decision 7).
fn runtime_octos(state: &std::path::Path) -> Option<(Vec<String>, std::path::PathBuf)> {
    let value = runtime(state)?;
    let block = &value["octos"]["local_octos"];
    let allowed = block["profiles"]
        .as_array()?
        .iter()
        .map(|id| id.as_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()?;
    Some((allowed, block["octos_home"].as_str()?.into()))
}

/// The preset and seat a fleet resource must name: the local binding's,
/// else the defaults `setup` writes.
fn runtime_seat(state: &std::path::Path, framework: &str) -> (String, String) {
    let value = runtime(state).unwrap_or_default();
    let block = match framework {
        "claude" => &value["claude"]["local_claude"],
        "octos" => &value["octos"]["local_octos"],
        _ => &value["local_codex"],
    };
    let field =
        |name: &str, default: String| block[name].as_str().map(str::to_owned).unwrap_or(default);
    (
        field("preset", format!("local_{framework}")),
        field("seat", format!("local_{framework}_seat")),
    )
}

/// The executables Setup would configure: Codex when found, Claude Code when
/// found with its own folder (its sign-in is assumed), Octos when found with
/// a qualified profile (its keys are assumed). A found Codex is named even
/// before it is signed in, so signing out never rewrites it away.
fn found(agents: &[crate::setup::AgentStatus], kind: &str) -> Option<std::path::PathBuf> {
    agents
        .iter()
        .find(|a| a.kind == kind && a.found && (kind == "codex" || a.signed_in))
        .and_then(|a| a.path.clone())
}

/// The Octos profiles found with a model Hagency qualifies: the ones Setup
/// allows (ADR-193 decision 8).
fn qualified_octos(agents: &[crate::setup::AgentStatus]) -> Vec<String> {
    listed_octos(agents)
        .iter()
        .filter(|p| p.qualified())
        .map(|p| p.id.clone())
        .collect()
}

fn listed_octos(agents: &[crate::setup::AgentStatus]) -> &[crate::setup::OctosProfile] {
    agents
        .iter()
        .find(|a| a.kind == "octos")
        .and_then(|a| a.profiles.as_deref())
        .unwrap_or_default()
}

/// Octos's profiles are read only on an imported fleet, where Setup applies.
async fn detect(fleet_state: Option<&std::path::Path>) -> Vec<crate::setup::AgentStatus> {
    let (codex, claude, octos) = tokio::join!(
        crate::setup::detect_codex(),
        crate::setup::detect_claude(),
        crate::setup::detect_octos(fleet_state)
    );
    vec![codex, claude, octos]
}

fn domain(depot: &Depot) -> Option<hagency_store::DomainStore> {
    depot
        .get_typed::<crate::App>()
        .ok()
        .and_then(|app| app.domain.clone())
}

fn live(depot: &Depot) -> Option<crate::bootstrap::palpo::Live> {
    depot
        .get_typed::<crate::App>()
        .ok()
        .and_then(|app| app.palpo_live().cloned())
}

async fn plugin_runtimes(
    plugins: Option<&hagency_plugin::PluginService>,
    domain: Option<&hagency_store::DomainStore>,
) -> Vec<serde_json::Value> {
    let Some(plugins) = plugins else {
        return Vec::new();
    };
    let choices = match domain {
        Some(store) => store.plugin_runtime_choices_all().await.unwrap_or_default(),
        None => Vec::new(),
    };
    let records = plugins.list().unwrap_or_default();
    let mut rows = Vec::new();
    for record in records.into_iter().filter(|record| {
        record
            .capabilities
            .iter()
            .any(|capability| capability == "runtime.v1")
    }) {
        let manifest = match plugins.manifest(&record.id) {
            Ok(manifest) => manifest,
            Err(_) => {
                rows.push(json!({
                    "pluginId": record.id,
                    "pluginName": record.id,
                    "adapterId": null,
                    "framework": null,
                    "state": "unavailable",
                    "qualifications": [],
                }));
                continue;
            }
        };
        for runtime in manifest.runtimes {
            let qualifications: Vec<_> = choices
                .iter()
                .filter(|choice| {
                    choice.plugin_id == record.id
                        && choice.adapter_id == runtime.id
                        && choice.package_sha256 == record.package_sha256
                        && choice.settings_sha256 == record.settings_sha256
                        && choice.framework == runtime.framework
                })
                .map(|choice| {
                    json!({
                        "model": choice.model,
                        "provider": choice.provider,
                        "reasoning": choice.reasoning,
                        "roles": choice.roles,
                    })
                })
                .collect();
            let state = if !record.enabled {
                "disabled"
            } else if record.stale {
                "stale"
            } else if record.restart_required {
                "restart_required"
            } else if !record.settings_ready {
                "configuration_required"
            } else if record.readiness != "ready" {
                "unavailable"
            } else if qualifications.is_empty() {
                "unqualified"
            } else {
                "ready"
            };
            rows.push(json!({
                "pluginId": record.id,
                "pluginName": manifest.name,
                "adapterId": runtime.id,
                "framework": runtime.framework,
                "state": state,
                "qualifications": qualifications,
                "packageSha256": record.package_sha256,
                "settingsSha256": record.settings_sha256,
            }));
        }
    }
    rows.sort_by(|left, right| {
        left["pluginId"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["pluginId"].as_str().unwrap_or_default())
            .then_with(|| {
                left["adapterId"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["adapterId"].as_str().unwrap_or_default())
            })
    });
    rows
}

async fn report(
    live: &crate::bootstrap::palpo::Live,
    domain: Option<&hagency_store::DomainStore>,
    plugins: Option<&hagency_plugin::PluginService>,
    agents: Vec<crate::setup::AgentStatus>,
    configured_now: Option<Result<(), String>>,
    replaced: bool,
) -> serde_json::Value {
    let plugin_runtimes = plugin_runtimes(plugins, domain).await;
    let plugin_runtime_ready = plugin_runtimes
        .iter()
        .any(|row| row["state"].as_str() == Some("ready"));
    let runtime = live.state_dir().join("fleet-runtime.json").is_file();
    // A configured runtime that no longer names exactly the detected agents
    // (an update changed a binary, or an agent was installed) is stale:
    // serve refuses a changed binary until the file is rewritten.
    let octos = found(&agents, "octos");
    let allowed = qualified_octos(&agents);
    let stale = runtime
        && !crate::setup::runtime_matches(
            live.state_dir(),
            found(&agents, "codex").as_deref(),
            found(&agents, "claude").as_deref(),
            octos.as_deref().map(|octos| (octos, allowed.as_slice())),
        );
    // The choices of the agents the runtime runs; before it exists (when
    // the page cannot offer yet) those of every agent Hagency supports.
    let mut frameworks = runtime_frameworks(live.state_dir());
    if frameworks.is_empty() {
        frameworks = vec!["codex", "claude", "octos"];
    }
    let mut offered: Vec<_> = frameworks
        .iter()
        .copied()
        .filter(|framework| *framework != "octos")
        .flat_map(choices)
        .collect();
    if frameworks.contains(&"octos") {
        let configured = runtime_octos(live.state_dir());
        offered.extend(octos_choices(
            listed_octos(&agents),
            configured.as_ref().map(|(allowed, _)| allowed.as_slice()),
        ));
    }
    let resources = match domain {
        Some(store) => store
            .catalog(String::new(), 64)
            .await
            .map(|rows| rows.len())
            .unwrap_or(0),
        None => 0,
    };
    let sources = match domain {
        Some(store) => store
            .resource_configurations(String::new(), 64)
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    // The coding agents that already have a source; the page offers the others.
    let source_frameworks: std::collections::BTreeSet<_> = sources
        .iter()
        .map(|source| source.config.framework.clone())
        .collect();
    let mut value = json!({
        "ok": true,
        "applicable": live.fleet_address().is_some(),
        "agents": agents,
        "runtimeConfigured": runtime && !stale,
        "runtimeStale": stale,
        "pluginRuntimes": plugin_runtimes,
        "pluginRuntimeReady": plugin_runtime_ready,
        "palpo": {
            "imported": live.is_imported(),
            "transport": live.status().get(),
        },
        "offer": {
            "choices": offered,
            "resources": resources,
            "sourceResources": sources.len(),
            "sourceFrameworks": source_frameworks,
        },
    });
    if let Some(result) = configured_now {
        // The fleet service loads fleet-runtime.json when it starts; a file
        // rewritten while it runs (a stale one) is used after a restart.
        value["restartNeeded"] = json!(result.is_ok() && replaced);
        value["configured"] = json!(result.is_ok());
        if let Err(problem) = result {
            value["problem"] = json!(problem);
        }
    }
    value
}

#[handler]
async fn status(depot: &mut Depot, res: &mut Response) {
    // Setup applies to an imported fleet only. Elsewhere the page and its
    // banner have nothing to show, which is an answer, not a failure.
    let Some(live) = live(depot) else {
        res.render(Json(json!({
            "ok": true,
            "applicable": false,
            "agents": [],
            "pluginRuntimes": [],
            "pluginRuntimeReady": false,
        })));
        return;
    };
    let fleet_state = live
        .fleet_address()
        .is_some()
        .then(|| live.state_dir().to_owned());
    let agents = detect(fleet_state.as_deref()).await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    let plugins = depot
        .get_typed::<crate::App>()
        .ok()
        .and_then(|app| app.plugins());
    res.render(Json(
        report(
            &live,
            domain(depot).as_ref(),
            plugins.as_ref(),
            agents,
            None,
            false,
        )
        .await,
    ));
}

#[handler]
async fn check(depot: &mut Depot, res: &mut Response) {
    // Writing the runtime configuration is a lifecycle write, like a Palpo
    // import.
    if !check_lifecycle(depot, res) {
        return;
    }
    let Some(live) = live(depot) else {
        refusal(res, StatusCode::SERVICE_UNAVAILABLE, "setup_unavailable");
        return;
    };
    let Some(address) = live.fleet_address() else {
        // A coordinator install configures its runtime through agent-driver.json.
        refusal(res, StatusCode::CONFLICT, "setup_not_fleet");
        return;
    };
    let state = live.state_dir().to_owned();
    let agents = detect(Some(&state)).await;
    let present = state.join("fleet-runtime.json").is_file();
    let (codex, claude) = (found(&agents, "codex"), found(&agents, "claude"));
    let octos = found(&agents, "octos");
    let allowed = qualified_octos(&agents);
    let stale = present
        && !crate::setup::runtime_matches(
            &state,
            codex.as_deref(),
            claude.as_deref(),
            octos.as_deref().map(|octos| (octos, allowed.as_slice())),
        );
    // A signed-in Codex, a found Claude Code or an Octos with a qualified
    // profile is enough to configure; each found agent is written (ADR-192
    // decision 7, ADR-193 decision 8).
    let usable = agents
        .iter()
        .any(|a| a.found && a.signed_in && matches!(a.kind, "codex" | "claude" | "octos"));
    let configured_now = if (!present || stale) && usable {
        let options = crate::setup::Options {
            state_dir: state,
            listen: address,
            no_codex: codex.is_none(),
            codex,
            codex_home: None,
            no_local_codex: false,
            no_claude: claude.is_none(),
            claude,
            claude_config_dir: None,
            no_octos: octos.is_none(),
            octos,
            // The home the runtime already names, else `$OCTOS_HOME` or
            // `~/.octos`: the one detection listed.
            octos_home: None,
            // Rewrite a stale file (the old one is kept as a .bak copy).
            force: stale,
        };
        let result = tokio::task::spawn_blocking(move || crate::setup::configure(&options))
            .await
            .map_err(|_| "configuration did not finish".to_owned())
            .and_then(|r| r.map(|_| ()));
        Some(result)
    } else {
        None
    };
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    let plugins = depot
        .get_typed::<crate::App>()
        .ok()
        .and_then(|app| app.plugins());
    res.render(Json(
        report(
            &live,
            domain(depot).as_ref(),
            plugins.as_ref(),
            agents,
            configured_now,
            stale,
        )
        .await,
    ));
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    /// `codex` (the default), `claude` (ADR-192) or `octos` (ADR-193).
    #[serde(default)]
    framework: Option<String>,
    model: String,
    reasoning: Option<String>,
    /// The Octos profile the resource runs; Octos only.
    #[serde(default)]
    profile: Option<String>,
    /// Plugin runtime identity; mutually exclusive with framework+profile.
    #[serde(default)]
    plugin_id: Option<String>,
    #[serde(default)]
    adapter_id: Option<String>,
    #[serde(default)]
    package_sha256: Option<String>,
    #[serde(default)]
    settings_sha256: Option<String>,
    /// Monthly token ceiling; 20 million when omitted.
    #[serde(default)]
    tokens: Option<u64>,
}

/// Prepare the first local source. It is not published and has no server
/// allocation until the operator reviews a resource configuration separately.
#[handler]
async fn offer(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !check_lifecycle(depot, res) {
        return;
    }
    let Some(live) = live(depot) else {
        refusal(res, StatusCode::SERVICE_UNAVAILABLE, "setup_unavailable");
        return;
    };
    let Some(store) = domain(depot) else {
        refusal(res, StatusCode::SERVICE_UNAVAILABLE, "domain_unavailable");
        return;
    };
    if live.fleet_address().is_none() {
        refusal(res, StatusCode::CONFLICT, "setup_not_fleet");
        return;
    }
    if !live.state_dir().join("fleet-runtime.json").is_file() {
        refusal(res, StatusCode::CONFLICT, "setup_runtime_missing");
        return;
    }
    let raw = match super::body(req, 1024).await {
        Ok(raw) => raw,
        Err(error) => {
            failed(res, error);
            return;
        }
    };
    let Ok(input) = serde_json::from_slice::<Offer>(&raw) else {
        refusal(res, StatusCode::BAD_REQUEST, "invalid_domain_command");
        return;
    };
    // Plugin runtime path: plugin_id + adapter_id identifies the runtime.
    if let (Some(plugin_id), Some(adapter_id)) =
        (input.plugin_id.as_deref(), input.adapter_id.as_deref())
    {
        let package_sha256 = input.package_sha256.as_deref().unwrap_or_default();
        let settings_sha256 = input.settings_sha256.as_deref().unwrap_or_default();
        if package_sha256.len() != 64 || settings_sha256.len() != 64 {
            refusal(res, StatusCode::BAD_REQUEST, "setup_invalid_plugin_digest");
            return;
        }
        // Fetch plugin runtimes to validate qualification.
        let Some(plugins) = depot
            .get_typed::<crate::App>()
            .ok()
            .and_then(|app| app.plugins())
        else {
            refusal(
                res,
                StatusCode::SERVICE_UNAVAILABLE,
                "setup_plugins_unavailable",
            );
            return;
        };
        let plugin_runtimes = plugin_runtimes(Some(&plugins), Some(&store)).await;
        let qualification = plugin_runtimes
            .iter()
            .find(|rt| {
                rt["pluginId"].as_str() == Some(plugin_id)
                    && rt["adapterId"].as_str() == Some(adapter_id)
                    && rt["state"].as_str() == Some("ready")
            })
            .and_then(|rt| {
                rt["qualifications"].as_array().and_then(|quals| {
                    quals.iter().find(|q| {
                        q["model"].as_str() == Some(&input.model)
                            && q["reasoning"].as_str() == input.reasoning.as_deref()
                    })
                })
            });
        let Some(qual) = qualification else {
            refusal(res, StatusCode::BAD_REQUEST, "setup_unqualified_model");
            return;
        };
        let tokens = input.tokens.unwrap_or(20_000_000);
        if tokens == 0 {
            refusal(res, StatusCode::BAD_REQUEST, "setup_unqualified_model");
            return;
        }
        let framework = qual["framework"].as_str().unwrap_or("plugin");
        let preset = format!("{plugin_id}_{adapter_id}_{}", input.model.replace('/', "_"));
        let seat = "plugin";
        let provider = qual["provider"].as_str().unwrap_or("plugin");
        let runtime_plugin = Some(json!({
            "pluginId": plugin_id,
            "adapterId": adapter_id,
            "packageSha256": package_sha256,
            "settingsSha256": settings_sha256,
        }));
        let resource = serde_json::from_value::<hagency_core::project::Resource>(json!({
            "presetId": preset,
            "seatId": seat,
            "framework": framework,
            "model": input.model,
            "provider": provider,
            "reasoning": input.reasoning,
            "runtimePlugin": runtime_plugin,
            "ceiling": {"tokens": tokens, "period": "monthly"},
            "published": false,
        }));
        let Ok(resource) = resource else {
            refusal(res, StatusCode::BAD_REQUEST, "invalid_domain_command");
            return;
        };
        let result = store.create_resource_source(resource).await;
        if let Err(error) = recheck(depot) {
            failed(res, error);
            return;
        }
        match result {
            Ok(resource) => res.render(Json(json!({"ok": true, "resource": resource}))),
            Err(hagency_store::Error::Conflict) => {
                refusal(res, StatusCode::CONFLICT, "setup_source_exists")
            }
            Err(_) => refusal(res, StatusCode::BAD_REQUEST, "setup_resource_refused"),
        }
        return;
    }
    // Built-in framework path (codex, claude, octos).
    let framework = match input.framework.as_deref().unwrap_or("codex") {
        "codex" => "codex",
        "claude" => "claude",
        "octos" => "octos",
        _ => {
            refusal(res, StatusCode::BAD_REQUEST, "setup_unqualified_model");
            return;
        }
    };
    // An Octos source names the profile it runs; no other source names one.
    if (framework == "octos") != input.profile.is_some() {
        refusal(res, StatusCode::BAD_REQUEST, "setup_unqualified_model");
        return;
    }
    // A source names a coding agent the runtime actually runs.
    if !runtime_frameworks(live.state_dir()).contains(&framework) {
        refusal(res, StatusCode::CONFLICT, "setup_runtime_missing");
        return;
    }
    // An Octos source runs one allowed profile whose primary model is still
    // the qualified one offered; its provider is the profile's family
    // (ADR-193 decision 7).
    let octos_choice = match input.profile.as_deref() {
        Some(profile) => {
            let found = runtime_octos(live.state_dir()).and_then(|(allowed, home)| {
                let profiles = crate::setup::octos_profiles(&home).ok()?;
                octos_choices(&profiles, Some(&allowed))
                    .into_iter()
                    .find(|c| c["profile"] == json!(profile))
            });
            if found.is_none() {
                refusal(res, StatusCode::BAD_REQUEST, "setup_unqualified_model");
                return;
            }
            found
        }
        None => None,
    };
    let qualified = match &octos_choice {
        Some(choice) => choice["model"] == json!(input.model) && input.reasoning.is_none(),
        None => choices(framework)
            .iter()
            .any(|c| c["model"] == json!(input.model) && c["reasoning"] == json!(input.reasoning)),
    };
    let tokens = input.tokens.unwrap_or(20_000_000);
    if !qualified || tokens == 0 {
        refusal(res, StatusCode::BAD_REQUEST, "setup_unqualified_model");
        return;
    }
    let (mut preset, seat) = runtime_seat(live.state_dir(), framework);
    let mut provider = json!(provider(framework));
    if let Some(choice) = &octos_choice {
        // One source per profile: the preset names it.
        preset = format!("{preset}_{}", input.profile.as_deref().unwrap_or_default());
        provider = choice["provider"].clone();
    }
    let resource = serde_json::from_value::<hagency_core::project::Resource>(json!({
        "presetId": preset,
        "seatId": seat,
        "framework": framework,
        "model": input.model,
        "provider": provider,
        "reasoning": input.reasoning,
        "octosProfile": input.profile,
        "ceiling": {"tokens": tokens, "period": "monthly"},
        "published": false,
    }));
    let Ok(resource) = resource else {
        refusal(res, StatusCode::BAD_REQUEST, "invalid_domain_command");
        return;
    };
    let result = store.create_resource_source(resource).await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    match result {
        Ok(resource) => res.render(Json(json!({"ok": true, "resource": resource}))),
        Err(hagency_store::Error::Conflict) => {
            refusal(res, StatusCode::CONFLICT, "setup_source_exists")
        }
        Err(_) => refusal(res, StatusCode::BAD_REQUEST, "setup_resource_refused"),
    }
}
