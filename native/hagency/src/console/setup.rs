//! ADR-189: the console's setup page.
//!
//! `GET /console/api/setup` reports each step: the coding agents found on
//! this machine (path, version, whether and how they are signed in), whether
//! the runtime is configured, and the Palpo connection. `POST
//! /console/api/setup/check` detects again and, when a signed-in Codex is
//! found and no runtime is configured yet, writes and validates
//! `fleet-runtime.json` with `hagency setup`'s code. Hagency never signs a
//! coding agent in; the page asks the user to.
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

/// The model and reasoning pairs Hagency qualifies for Codex
/// (`role-capacity.json`): only these are published to Palpo.
fn codex_choices() -> Vec<serde_json::Value> {
    let profile = hagency_core::qualification::ModelProfile {
        framework: "codex".into(),
        model: String::new(),
        provider: Some("openai".into()),
        reasoning: None,
    };
    hagency_core::qualification::configuration_choices(&profile)
        .unwrap_or_default()
        .into_iter()
        .map(|c| json!({"model": c.model, "reasoning": c.reasoning, "roles": c.roles}))
        .collect()
}

/// The seat a fleet resource must name: the `local_codex` block's, else the
/// default `setup` writes.
fn runtime_seat(state: &std::path::Path) -> String {
    std::fs::read(state.join("fleet-runtime.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|v| v["local_codex"]["seat"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "local_codex_seat".into())
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
    // A configured runtime that no longer names the detected Codex (an
    // update changed the binary) is stale: serve refuses it until rewritten.
    let stale = runtime
        && agents
            .iter()
            .find(|a| a.kind == "codex")
            .and_then(|a| a.path.as_deref())
            .is_some_and(|path| !crate::setup::runtime_matches(live.state_dir(), path));
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
            .map(|rows| rows.len())
            .unwrap_or(0),
        None => 0,
    };
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
            "choices": codex_choices(),
            "resources": resources,
            "sourceResources": sources,
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
    let agents = vec![crate::setup::detect_codex().await];
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
    let codex = crate::setup::detect_codex().await;
    let state = live.state_dir().to_owned();
    let present = state.join("fleet-runtime.json").is_file();
    let stale = present
        && codex
            .path
            .as_deref()
            .is_some_and(|path| !crate::setup::runtime_matches(&state, path));
    let configured_now = if (!present || stale) && codex.found && codex.signed_in {
        let options = crate::setup::Options {
            state_dir: state,
            listen: address,
            codex: codex.path.clone(),
            codex_home: None,
            no_local_codex: false,
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
        report(
            &live,
            domain(depot).as_ref(),
            vec![codex],
            configured_now,
            stale,
        )
        .await,
    ));
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
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
    let qualified = codex_choices()
        .iter()
        .any(|c| c["model"] == json!(input.model) && c["reasoning"] == json!(input.reasoning));
    let tokens = input.tokens.unwrap_or(20_000_000);
    if !qualified || tokens == 0 {
        refusal(res, StatusCode::BAD_REQUEST, "setup_unqualified_model");
        return;
    }
    let resource = serde_json::from_value::<hagency_core::project::Resource>(json!({
        "presetId": "local_codex",
        "seatId": runtime_seat(live.state_dir()),
        "framework": "codex",
        "model": input.model,
        "provider": "openai",
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
