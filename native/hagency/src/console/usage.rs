use super::{Error, failed, recheck};
use crate::{refusal, resources::domain};
use hagency_core::project::{CleanupState, EngagementState, identifier};
use salvo::prelude::*;
use serde::Serialize;

/// The words `?state=` may name (board #60 item 3): the store's own
/// engagement-state vocabulary (`domain.sql` `engagements.state` CHECK), so
/// an unknown word is a 400 rather than a silently empty page.
const ENGAGEMENT_STATES: [&str; 6] = [
    "pending", "reserved", "active", "rejected", "revoked", "failed",
];
pub(super) fn router() -> Router {
    Router::new()
        .push(Router::with_path("engagements").get(engagements))
        .push(Router::with_path("engagements/{id}/usage").get(report))
        .push(Router::with_path("usage/totals").get(totals))
}
pub(super) fn query(req: &Request, allowed: &[&str], limit: usize) -> Result<(), Error> {
    if req
        .uri()
        .query()
        .is_some_and(|q| q.len() > limit || q.contains('%') || q.contains('+'))
    {
        return Err(Error::Invalid);
    }
    let values = req.queries();
    for (key, _) in values.iter() {
        if !allowed.contains(&key.as_str())
            || values
                .get_vec(key)
                .is_none_or(|v| v.len() != 1 || v[0].is_empty())
        {
            return Err(Error::Invalid);
        }
    }
    Ok(())
}
pub(super) fn selection_query(req: &Request) -> Result<(), Error> {
    query(req, &["engagement_id"], 160)?;
    if let Some(value) = req.query::<String>("engagement_id") {
        identifier(&value, 128).map_err(|_| Error::Invalid)?;
    }
    Ok(())
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Label {
    coordinator_managed: bool,
    id: String,
    agent_name: String,
    project_name: Option<String>,
    role: String,
    /// Requested allocation in tokens — the console engagements page's token
    /// column (brief 16). Raw number, never compacted.
    requested_tokens: u64,
    state: EngagementState,
    cleanup: CleanupState,
    /// What is LEFT on the resource behind the agent (board #60 item 3; TS
    /// `:14974` `agentRemainingTokens`): the same `min` of the non-null
    /// limits the admission decision uses, so the queue shows over-commitment
    /// before the decision. Null when no ceiling is declared.
    agent_remaining_tokens: Option<u64>,
    /// TS `:14972`: a pending request with no verified owner binding yet.
    owner_binding_required: bool,
    /// The request's own observation instant, and the terminal instant for an
    /// ended engagement. Both null when unknown — never an invented clock.
    created_at_ms: Option<u64>,
    ended_at_ms: Option<u64>,
    /// ADR-186 §A4: what the engagement holds — the granted amount, raised
    /// by any top-up, else the request.
    allocated_tokens: u64,
    /// ADR-186 §B: known fresh spend; null while unknown, never zero.
    spent_tokens: Option<u64>,
    /// ADR-186 §B2: an open quota hold, shown as "paused: quota".
    quota_paused: bool,
}
#[handler]
async fn engagements(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    // Board #60 item 3: the retained list takes `?state=` (`:14965`), so the
    // allowlist admits it alongside the cursor and the page size.
    if query(req, &["after", "limit", "state"], 224).is_err() {
        failed(res, Error::Invalid);
        return;
    }
    let after = req.query::<String>("after").unwrap_or_default();
    let limit = match req.query::<String>("limit") {
        None => 16,
        Some(v) if v.bytes().all(|c| c.is_ascii_digit()) => v.parse::<usize>().unwrap_or(0),
        _ => 0,
    };
    // A state value must name a real engagement state; an unknown word is a
    // bad request rather than a silently empty page.
    let state = req.query::<String>("state");
    if state
        .as_deref()
        .is_some_and(|value| !ENGAGEMENT_STATES.contains(&value))
    {
        failed(res, Error::Invalid);
        return;
    }
    if !(1..=16).contains(&limit) || (!after.is_empty() && identifier(&after, 128).is_err()) {
        failed(res, Error::Invalid);
        return;
    }
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.engagement_labels(after, state, limit + 1).await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    match result {
        Ok(mut rows) => {
            let next_after = (rows.len() > limit).then(|| rows[limit - 1].id.clone());
            rows.truncate(limit);
            let labels: Vec<_> = rows
                .into_iter()
                .map(|e| Label {
                    coordinator_managed: e.coordinator_managed,
                    id: e.id,
                    agent_name: e.agent_name,
                    project_name: e.project_name,
                    role: e.role,
                    requested_tokens: e.requested_tokens,
                    state: e.state,
                    cleanup: e.cleanup,
                    agent_remaining_tokens: e.agent_remaining_tokens,
                    owner_binding_required: e.owner_binding_required,
                    created_at_ms: e.created_at_ms,
                    ended_at_ms: e.ended_at_ms,
                    allocated_tokens: e.allocated_tokens,
                    spent_tokens: e.spent_tokens,
                    quota_paused: e.quota_paused,
                })
                .collect();
            res.render(Json(
                serde_json::json!({"engagements":labels,"next_after":next_after}),
            ));
        }
        Err(error) => store_error(res, error),
    }
}
#[handler]
async fn report(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if query(req, &["at_ms"], 128).is_err() {
        failed(res, Error::Invalid);
        return;
    }
    let at = match req.query::<String>("at_ms") {
        None => None,
        Some(v) if v.bytes().all(|c| c.is_ascii_digit()) => match v.parse::<u64>() {
            Ok(v) => Some(v),
            Err(_) => {
                failed(res, Error::Invalid);
                return;
            }
        },
        _ => {
            failed(res, Error::Invalid);
            return;
        }
    };
    let Some(id) = req.param::<String>("id") else {
        failed(res, Error::Invalid);
        return;
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.usage_report(id, at).await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    match result {
        Ok(value) => res.render(Json(value)),
        Err(error) => store_error(res, error),
    }
}
fn store_error(res: &mut Response, error: hagency_store::Error) {
    let (status, code) = match error {
        hagency_store::Error::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_usage_query"),
        hagency_store::Error::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        hagency_store::Error::Busy => (StatusCode::SERVICE_UNAVAILABLE, "busy"),
        hagency_store::Error::OutcomeUnknown => (StatusCode::GATEWAY_TIMEOUT, "outcome_unknown"),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "usage_unavailable"),
    };
    refusal(res, status, code);
}

/// The retained `GET /api/usage` fleet `totals` block (backend-v2.js:15700-15720):
/// the measured figures native actually holds (per-engagement known high
/// water), summed, with the denominator attached — the numerator never
/// travels without it, and null means "not known", never zero. Read class,
/// no scope, no selection, same as the engagements list beside it.
#[handler]
async fn totals(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if query(req, &[], 0).is_err() {
        failed(res, Error::Invalid);
        return;
    }
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.usage_totals().await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    match result {
        Ok(fleet) => res.render(Json(serde_json::json!({
            "ok": true,
            "totals": fleet,
            // The retained totals block also carries busySec and tasks
            // (backend-v2.js:15706-15708), summed from the agents' busy
            // clocks and the task store. Native holds neither figure — the
            // per-engagement evidence slice has no busy-time accounting —
            // so the columns are named here the way the project-sides read
            // names its own gaps: unknown is never zero and never invented.
            "unavailable": ["busy_sec", "tasks"],
        }))),
        Err(error) => store_error(res, error),
    }
}
