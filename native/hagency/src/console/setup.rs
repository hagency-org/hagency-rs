//! ADR-189: the console's setup page.
//!
//! `GET /console/api/setup` reports each step: the coding agents found on
//! this machine (path, version, whether and how they are signed in), whether
//! the runtime is configured, and the Palpo connection. `POST
//! /console/api/setup/check` detects again and, when a signed-in Codex or a
//! Claude Code (ADR-192) is found and the runtime is missing or stale, writes
//! and validates `fleet-runtime.json` with `hagency setup`'s code. Hagency
//! never signs a coding agent in; the page asks the user to sign Codex in,
//! and assumes Claude Code is signed in (ADR-192 decision 6).
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

fn runtime(state: &std::path::Path) -> Option<serde_json::Value> {
    std::fs::read(state.join("fleet-runtime.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
}

/// The coding agents `fleet-runtime.json` configures: Codex, Claude Code or
/// both (ADR-192).
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
    frameworks
}

/// The preset and seat a fleet resource must name: the local binding's,
/// else the defaults `setup` writes.
fn runtime_seat(state: &std::path::Path, framework: &str) -> (String, String) {
    let value = runtime(state).unwrap_or_default();
    let block = if framework == "claude" {
        &value["claude"]["local_claude"]
    } else {
        &value["local_codex"]
    };
    let field =
        |name: &str, default: String| block[name].as_str().map(str::to_owned).unwrap_or(default);
    (
        field("preset", format!("local_{framework}")),
        field("seat", format!("local_{framework}_seat")),
    )
}

/// The executables Setup would configure: Codex when found, Claude Code when
/// found with its own folder (its sign-in is assumed). A found Codex is named
/// even before it is signed in, so signing out never rewrites it away.
fn found(agents: &[crate::setup::AgentStatus], kind: &str) -> Option<std::path::PathBuf> {
    agents
        .iter()
        .find(|a| a.kind == kind && a.found && (kind == "codex" || a.signed_in))
        .and_then(|a| a.path.clone())
}

async fn detect() -> Vec<crate::setup::AgentStatus> {
    let (codex, claude) = tokio::join!(crate::setup::detect_codex(), crate::setup::detect_claude());
    vec![codex, claude]
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

async fn report(
    live: &crate::bootstrap::palpo::Live,
    domain: Option<&hagency_store::DomainStore>,
    agents: Vec<crate::setup::AgentStatus>,
    configured_now: Option<Result<(), String>>,
    replaced: bool,
) -> serde_json::Value {
    let runtime = live.state_dir().join("fleet-runtime.json").is_file();
    // A configured runtime that no longer names exactly the detected agents
    // (an update changed a binary, or an agent was installed) is stale:
    // serve refuses a changed binary until the file is rewritten.
    let stale = runtime
        && !crate::setup::runtime_matches(
            live.state_dir(),
            found(&agents, "codex").as_deref(),
            found(&agents, "claude").as_deref(),
        );
    // The choices of the agents the runtime runs; before it exists (when
    // the page cannot offer yet) those of every agent Hagency supports.
    let mut frameworks = runtime_frameworks(live.state_dir());
    if frameworks.is_empty() {
        frameworks = vec!["codex", "claude"];
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
        "palpo": {
            "imported": live.is_imported(),
            "transport": live.status().get(),
        },
        "offer": {
            "choices": frameworks.into_iter().flat_map(choices).collect::<Vec<_>>(),
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
        res.render(Json(json!({"ok": true, "applicable": false, "agents": []})));
        return;
    };
    let agents = detect().await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    res.render(Json(
        report(&live, domain(depot).as_ref(), agents, None, false).await,
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
    let agents = detect().await;
    let state = live.state_dir().to_owned();
    let present = state.join("fleet-runtime.json").is_file();
    let (codex, claude) = (found(&agents, "codex"), found(&agents, "claude"));
    let stale =
        present && !crate::setup::runtime_matches(&state, codex.as_deref(), claude.as_deref());
    // A signed-in Codex or a found Claude Code is enough to configure; each
    // found agent is written (ADR-192 decision 7).
    let usable = agents
        .iter()
        .any(|a| a.found && a.signed_in && (a.kind == "codex" || a.kind == "claude"));
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
    res.render(Json(
        report(&live, domain(depot).as_ref(), agents, configured_now, stale).await,
    ));
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    /// `codex` (the default) or `claude` (ADR-192).
    #[serde(default)]
    framework: Option<String>,
    model: String,
    reasoning: Option<String>,
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
    let framework = match input.framework.as_deref().unwrap_or("codex") {
        "codex" => "codex",
        "claude" => "claude",
        _ => {
            refusal(res, StatusCode::BAD_REQUEST, "setup_unqualified_model");
            return;
        }
    };
    // A source names a coding agent the runtime actually runs.
    if !runtime_frameworks(live.state_dir()).contains(&framework) {
        refusal(res, StatusCode::CONFLICT, "setup_runtime_missing");
        return;
    }
    let qualified = choices(framework)
        .iter()
        .any(|c| c["model"] == json!(input.model) && c["reasoning"] == json!(input.reasoning));
    let tokens = input.tokens.unwrap_or(20_000_000);
    if !qualified || tokens == 0 {
        refusal(res, StatusCode::BAD_REQUEST, "setup_unqualified_model");
        return;
    }
    let (preset, seat) = runtime_seat(live.state_dir(), framework);
    let resource = serde_json::from_value::<hagency_core::project::Resource>(json!({
        "presetId": preset,
        "seatId": seat,
        "framework": framework,
        "model": input.model,
        "provider": provider(framework),
        "reasoning": input.reasoning,
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
