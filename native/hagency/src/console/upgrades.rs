//! ADR-197: the operator surface for engagement runtime upgrades. Status reads
//! are open to any console session; every mutation needs the same
//! `Scope::AgentLifecycle` gate as the retire routes. The store owns every
//! guarantee (idle boundary, qualification, canary, one-step rollback, audit);
//! the routes only carry the call, stamp the actor and the clock, and name a
//! refusal. A refused rebind or rollback is a committed audit record in the
//! store and is served here as 409 with the refusal word as the code.
use super::{Error, body, engagements::check_lifecycle, failed, recheck};
use crate::{refusal, refusal_explained, resources::domain};
use hagency_core::project::identifier;
use hagency_store::RebindResult;
use salvo::prelude::*;
use serde::Deserialize;

const ACTOR: &str = "console_operator";

pub(super) fn router() -> Router {
    Router::new()
        .push(Router::with_path("engagements/{id}/upgrade").get(upgrade_status))
        .push(Router::with_path("engagements/{id}/upgrade/rebind").post(rebind))
        .push(Router::with_path("engagements/{id}/upgrade/pin").post(pin))
        .push(Router::with_path("engagements/{id}/upgrade/unpin").post(unpin))
        .push(Router::with_path("engagements/{id}/upgrade/rollback").post(rollback))
        .push(Router::with_path("resources/{id}/rollout").get(rollout_status))
        .push(Router::with_path("resources/{id}/rollout/start").post(rollout_start))
        .push(Router::with_path("resources/{id}/rollout/step").post(rollout_step))
        .push(Router::with_path("resources/{id}/rollout/canary").post(rollout_canary))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or_default()
}

fn store_error(res: &mut Response, error: hagency_store::Error) {
    let (status, code) = match error {
        hagency_store::Error::State => (StatusCode::CONFLICT, "upgrade_state_conflict"),
        hagency_store::Error::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        hagency_store::Error::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_upgrade_request"),
        hagency_store::Error::Busy => (StatusCode::SERVICE_UNAVAILABLE, "busy"),
        hagency_store::Error::OutcomeUnknown => (StatusCode::GATEWAY_TIMEOUT, "outcome_unknown"),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "engagement_unavailable"),
    };
    refusal(res, status, code);
}

fn explain(word: &str) -> &'static str {
    match word {
        "not_marked" => "the engagement has no pending upgrade",
        "pinned" => "the engagement is pinned; unpin it first",
        "rollout_halted" => "the rollout is halted; start a new rollout",
        "canary_pending" => "wait for the canary engagement to pass",
        "engagement_not_active" => "the engagement is not active",
        "agent_fenced" => "the agent is fenced",
        "dispatch_leased" => "a dispatch is leased; try again when idle",
        "dispatch_started" => "a dispatch is running; try again when idle",
        "dispatch_parked" => "a dispatch is parked; try again when idle",
        "unresolved_dispatch" => "a dispatch is unresolved",
        "outgoing_custody_pending" => "an outgoing message is still pending",
        "plugin_mismatch" => "the target is a different plugin or adapter",
        "unqualified" => "the target package is not qualified for this agent",
        "no_previous" => "there is no previous package to roll back to",
        _ => "the upgrade was refused",
    }
}

/// Validate the `{id}` path parameter and the absence of a query.
fn path_id(req: &Request, res: &mut Response) -> Option<String> {
    if req.uri().query().is_some() {
        failed(res, Error::Invalid);
        return None;
    }
    match req.param::<String>("id") {
        Some(id) if identifier(&id, 128).is_ok() => Some(id),
        _ => {
            failed(res, Error::Invalid);
            None
        }
    }
}

/// The mutation preamble: path id, lifecycle scope, and an empty body.
async fn mutation(req: &mut Request, depot: &Depot, res: &mut Response) -> Option<String> {
    let id = path_id(req, res)?;
    if !check_lifecycle(depot, res) {
        return None;
    }
    match body(req, 16).await {
        Ok(raw) if raw.is_empty() => Some(id),
        _ => {
            failed(res, Error::Invalid);
            None
        }
    }
}

fn settled<T: serde::Serialize + Send>(
    depot: &Depot,
    res: &mut Response,
    result: Result<T, hagency_store::Error>,
) {
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    match result {
        Ok(value) => res.render(Json(value)),
        Err(error) => store_error(res, error),
    }
}

fn rebound(depot: &Depot, res: &mut Response, result: Result<RebindResult, hagency_store::Error>) {
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    match result {
        Ok(value) if value.applied => res.render(Json(value)),
        Ok(value) => {
            let word = value.refusal.map_or("upgrade_refused", |r| r.as_str());
            refusal_explained(res, StatusCode::CONFLICT, word, explain(word));
        }
        Err(error) => store_error(res, error),
    }
}

#[handler]
async fn upgrade_status(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(id) = path_id(req, res) else {
        return;
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.engagement_upgrade_status(id).await;
    settled(depot, res, result);
}

#[handler]
async fn rebind(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(id) = mutation(req, depot, res).await else {
        return;
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store
        .rebind_engagement_runtime(id, ACTOR.into(), now())
        .await;
    rebound(depot, res, result);
}

#[handler]
async fn rollback(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(id) = mutation(req, depot, res).await else {
        return;
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store
        .rollback_engagement_runtime(id, ACTOR.into(), now())
        .await;
    rebound(depot, res, result);
}

#[handler]
async fn pin(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(id) = mutation(req, depot, res).await else {
        return;
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.pin_engagement_runtime(id, ACTOR.into(), now()).await;
    settled(depot, res, result);
}

#[handler]
async fn unpin(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(id) = mutation(req, depot, res).await else {
        return;
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store
        .unpin_engagement_runtime(id, ACTOR.into(), now())
        .await;
    settled(depot, res, result);
}

#[handler]
async fn rollout_status(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(id) = path_id(req, res) else {
        return;
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.resource_rollout_status(id).await;
    settled(depot, res, result);
}

#[handler]
async fn rollout_start(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(id) = mutation(req, depot, res).await else {
        return;
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.start_resource_rollout(id, ACTOR.into(), now()).await;
    settled(depot, res, result);
}

#[handler]
async fn rollout_step(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(id) = mutation(req, depot, res).await else {
        return;
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.rollout_step(id, ACTOR.into(), now()).await;
    settled(depot, res, result);
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Canary {
    passed: bool,
    detail: Option<String>,
}

/// Record the operator's verdict on the canary engagement.
#[handler]
async fn rollout_canary(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(id) = path_id(req, res) else {
        return;
    };
    if !check_lifecycle(depot, res) {
        return;
    }
    let input: Canary = match body(req, 1024)
        .await
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
    {
        Some(input) => input,
        None => {
            failed(res, Error::Invalid);
            return;
        }
    };
    if input.detail.as_ref().is_some_and(|d| d.len() > 512) {
        failed(res, Error::Invalid);
        return;
    }
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store
        .record_canary_result(id, input.passed, ACTOR.into(), input.detail, now())
        .await;
    settled(depot, res, result);
}
