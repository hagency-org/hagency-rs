//! The read-only agent roster (ADR-126, widened by board #22): a bounded
//! observation of the engagement projections, mounted under the API
//! sub-router's `authenticate` hoop with NO scope — the same read class as
//! `engagements` and the resources list: scope facts are a payload on
//! reads, never a gate (only the mutations consult a scope). The wire
//! item carries EXACTLY nine scalar keys derived at the store
//! (`DomainRepository::agent_roster`): no credential home, no workdir,
//! no state dir, no workspace path, no tmux target, no pane buffer, no
//! token — and no nested object at all, so nothing can hide inside one.
//! The client validator refuses a tenth key, so a future widening
//! fails the whole read instead of leaking silently (fail-closed).
//!
//! One row per AGENT, the TS roster's shape (`backend-v2.js:11696`
//! serializes every agent record): an agent whose engagements all ended
//! still appears. `online` is REAL worker state — a live dispatch
//! (`leased`/`started`/`parked`) in one of the agent's sessions — and
//! `last_seen_ms` is the newest `runner_attempts.created_at` the agent
//! produced: null, never zero, when it never attempted. `last_activity_ms`
//! keeps "last dispatch activity" of the representative engagement.
//! `unavailable` is server-owned, like the alert transition map: the
//! page renders whatever the server names, so a future source turns a
//! column on by removing its name here, not by a client edit.
use super::resources::failure;
use super::{Error, Session, body, console, failed, recheck, usage::query};
use crate::{App, refusal, resources::domain};
use hagency_core::project::{AgentName, EngagementState, identifier};
use salvo::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(super) fn router() -> Router {
    Router::with_path("agents")
        .get(list)
        .push(
            Router::with_path("{name}")
                .get(super::agent_detail::detail)
                .delete(delete),
        )
        .push(Router::with_path("{id}/start").post(start))
        .push(Router::with_path("{id}/stop").post(stop))
        .push(Router::with_path("{id}/preset").put(preset))
        .push(Router::with_path("{id}/recover-dispatch").post(recover_dispatch))
        .push(Router::with_path("{id}/stopped-dispatches").get(stopped_dispatches))
        .push(
            Router::with_path("{id}/stopped-dispatches/{dispatch}/inspection")
                .get(stopped_dispatch_inspection),
        )
        .push(
            Router::with_path("{id}/stopped-dispatches/{dispatch}/inspect")
                .post(begin_outcome_inspection),
        )
        .push(Router::with_path("{id}/resolve-stopped-dispatch").post(resolve_stopped_dispatch))
        .push(Router::with_path("{id}/continue-stopped-dispatch").post(continue_stopped_dispatch))
        .push(Router::with_path("{id}/refuse").post(refuse))
}

/// Exactly twelve keys, in the ADR-126 order extended by board #22, board
/// #60 and ADR-186 (`quota_paused`). Every key except `name`, `framework`, `role`, `state`,
/// `engagement_id`, `requested_tokens`, `online` and `seat` is nullable at
/// the source; `null` means "unknown", rendered as such. `consumed` is
/// `null` when nothing was measured — unknown, never zero. One row per
/// AGENT (TS parity: `backend-v2.js:11696` serializes every agent record).
#[derive(Serialize)]
struct RosterItem {
    name: String,
    framework: String,
    role: String,
    /// The ENGAGEMENT LIFECYCLE word (pending/reserved/active/…), which is
    /// what the store's projection carries.
    state: EngagementState,
    engagement_id: String,
    requested_tokens: u64,
    online: bool,
    last_seen_ms: Option<u64>,
    last_activity_ms: Option<u64>,
    /// The LIVE DISPATCH's word, separate from `state` by construction
    /// (board #60 item 2; TS `:6872` reads `machine.state`, a liveness
    /// value). Null when native's dispatch record shows no live dispatch.
    /// Carries TS's `manualDown` as `stopped` (`backend-v2.js:6847`), which is
    /// the word the console offers Start for — so the wire keeps its exact
    /// eleven keys and the client validator is untouched.
    liveness: Option<String>,
    /// Tokens observed consumed by the agent's engagements, summed the way
    /// the usage report sums it. Null when nothing was measured.
    consumed: Option<u64>,
    /// ADR-186 §B2: the agent's engagement holds an open quota hold — its
    /// allocation is used up and nothing new is dispatched until a top-up.
    /// Separate from `liveness`, because the running turn still finishes.
    quota_paused: bool,
}

/// Board #60 item 2. The columns that were printed as `unknown` are now
/// ANSWERED from native state — `consumed` (`usage_sources.latest_counts`)
/// and `liveness` (the live dispatch row, separate from the engagement
/// `state` word) — so they render as columns and nothing is left to name.
///
/// The four that cannot be answered are DROPPED rather than printed as
/// "unknown", and `seat` joins them: TS's own roster serializer
/// (`backend-v2.js:6822-6916`) carries no seat, and this codebase treats the
/// seat id as private (`console/accounts.rs` deliberately withholds
/// `seat_id`; the roster fixture asserts a seat name never reaches the
/// wire). Filling it would invent a field TS does not have, so the honest
/// reading of "fill what native can answer" is that `seat` is not one of
/// them. `tmux`/`pane`/`credential_home`/`workspace_path` are unanswerable
/// by design (ADR-126 keeps panes and paths off the wire, and the store
/// holds no such column).
///
/// The list is kept, empty, because it is the server-owned mechanism the
/// page renders verbatim: a future column with no source turns itself on by
/// being named here.
const UNAVAILABLE: [&str; 0] = [];

/// The fleet's own view of one engagement's worker: `Some(true)` while that
/// worker is alive — not one of the settled terminal/failure words, the same
/// rule `/health` applies (`lib.rs:456`: "a fleet agent is online while its
/// worker is alive"). TS derives an agent's `online` the same way
/// (`backend-v2.js:6783-6814` `getAgentDeliveryState` -> `machine.online`),
/// NOT from whether a dispatch happens to be live: an agent whose worker is up
/// and between dispatches (`receiving`/`no_work`) is online, which is exactly
/// the live run that reported every agent Offline (board #106).
///
/// `None` when this process owns no fleet (an asset-only or non-factory host)
/// or the fleet holds no row for the engagement — then the caller keeps the
/// store's own live-dispatch rule rather than inventing a verdict.
fn fleet_alive(snapshot: Option<&serde_json::Value>, engagement: &str) -> Option<bool> {
    let row = snapshot
        .and_then(|value| value.get("agents"))
        .and_then(|agents| agents.as_array())
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["engagement_id"].as_str() == Some(engagement))
        })?;
    // The same settled terminal/failure words `/health` excludes.
    Some(!matches!(
        row["status"]["state"].as_str(),
        Some("closed" | "unavailable" | "outcome_unknown" | "stopped")
    ))
}

#[handler]
async fn list(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    // The roster takes ONE selection — `view`, whose only meaningful value
    // is `names` (TS `backend-v2.js:11698`, the name-only arm the console's
    // pickers use). Every other query parameter is refused, the same
    // hygiene the alerts read applies to its own allowlist.
    if query(req, &["view"], 64).is_err() {
        failed(res, Error::Invalid);
        return;
    }
    let names_only = req
        .query::<String>("view")
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("names"));
    let Some(store) = domain(depot, res) else {
        return;
    };
    // The fleet's own worker state, when this host owns a fleet. Read before
    // the store call so the overlay and the rows describe one moment.
    let fleet = depot
        .get_typed::<App>()
        .ok()
        .and_then(|app| app.fleet.as_ref())
        .map(|fleet| fleet.snapshot());
    let result = store.agent_roster().await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    match result {
        Ok(rows) => {
            // TS answers `names` as a sorted bare array of non-empty name
            // strings, filtered to agents that are not retired
            // (`backend-v2.js:11699-11704`). Native has no agent-delete
            // route, so nothing carries a `retiredAt`; every roster row
            // qualifies — the same set the envelope below would carry.
            if names_only {
                let mut names: Vec<String> = rows
                    .into_iter()
                    .map(|row| row.name)
                    .filter(|name| !name.is_empty())
                    .collect();
                names.sort();
                res.render(Json(names));
                return;
            }
            // Statement time, as the alerts read does: the roster has no
            // clock parameter to honor.
            let at_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|d| u64::try_from(d.as_millis()).ok())
                .unwrap_or_default();
            let agents: Vec<_> = rows
                .into_iter()
                .map(|row| {
                    // TS derives `online` from `machine.online`, and a
                    // manual-down machine is NOT online
                    // (`lib/agent-state.js:71`: `online = state !== 'offline'
                    // && state !== 'manual_down'`). So the operator's stop
                    // outranks the fleet: a worker the stop has fenced but not
                    // yet reaped must not read Online.
                    //
                    // Otherwise the fleet is authoritative when it holds a row
                    // for this engagement (`/health` derives its own
                    // `onlineAgents` from exactly this snapshot, `lib.rs:451`);
                    // a worker alive between dispatches (`receiving`,
                    // `no_work`) is Online, which is the defect the live run
                    // reported (board #106).
                    let online = !row.manual_down
                        && fleet_alive(fleet.as_ref(), &row.engagement_id).unwrap_or(row.online);
                    RosterItem {
                        name: row.name,
                        framework: row.framework,
                        role: row.role,
                        state: row.state,
                        engagement_id: row.engagement_id,
                        requested_tokens: row.requested_tokens,
                        online,
                        last_seen_ms: row.last_seen_ms,
                        last_activity_ms: row.last_activity_ms,
                        liveness: row.liveness,
                        consumed: row.consumed,
                        quota_paused: row.quota_paused,
                    }
                })
                .collect();
            // CL-S2 (ADR-130): the lifecycle controls render ONLY from the
            // served boolean — a read-only session renders none enabled.
            let manage_lifecycle = match depot.get_typed::<Session>() {
                Ok(session) => {
                    match console(depot).and_then(|c| c.0.authority.can_lifecycle(session)) {
                        Ok(value) => value,
                        Err(error) => {
                            failed(res, error);
                            return;
                        }
                    }
                }
                Err(_) => {
                    failed(res, Error::Unauthorized);
                    return;
                }
            };
            res.render(Json(serde_json::json!({
                "at_ms": at_ms,
                "unavailable": UNAVAILABLE,
                "agents": agents,
                "permissions": {"manageLifecycle": manage_lifecycle},
            })));
        }
        Err(error) => store_error(res, error),
    }
}

/// The engagement id path parameter — an opaque identifier, never a path
/// component that can address a session, authority or filesystem.
fn engagement_id(req: &Request) -> Result<String, Error> {
    let id = req.param::<String>("id").ok_or(Error::Invalid)?;
    identifier(&id, 128).map_err(|_| Error::Invalid)?;
    Ok(id)
}

/// The lifecycle routes share one scope gate: a valid session whose
/// grant is `Scope::AgentLifecycle`. A read-only or other-scoped session is
/// refused with `agent_lifecycle_scope_required` before any store work.
fn check_lifecycle(depot: &Depot, res: &mut Response) -> bool {
    let session = match depot.get_typed::<Session>() {
        Ok(session) => session,
        Err(_) => {
            failed(res, Error::Unauthorized);
            return false;
        }
    };
    match console(depot).and_then(|c| c.0.authority.can_lifecycle(session)) {
        Ok(true) => true,
        Ok(false) => {
            failed(res, Error::LifecycleForbidden);
            false
        }
        Err(error) => {
            failed(res, error);
            false
        }
    }
}

/// Start — TS parity backend-v2.js:12712-12775: refuses an agent that never
/// stopped (the retained `agent already online`, 409), refuses while
/// lifecycle cleanup is pending (the retained `agent_lifecycle_busy`, 409),
/// and otherwise records the durable return to serving. The host's own
/// discovery picks the agent back up; this route never spawns a process.
#[handler]
async fn start(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if query(req, &[], 0).is_err() {
        failed(res, Error::Invalid);
        return;
    }
    let id = match engagement_id(req) {
        Ok(id) => id,
        Err(error) => {
            failed(res, error);
            return;
        }
    };
    if !check_lifecycle(depot, res) {
        return;
    }
    let Some(store) = domain(depot, res) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or_default();
    let result = store.start_agent(id, now).await;
    if result.is_ok() && recheck(depot).is_err() {
        failure(res, hagency_store::Error::OutcomeUnknown);
        return;
    }
    match result {
        Ok(()) => res.render(Json(serde_json::json!({"ok": true, "state": "launching"}))),
        Err(hagency_store::Error::NotFound) => refusal(res, StatusCode::NOT_FOUND, "not_found"),
        Err(hagency_store::Error::Conflict) => {
            // `agent already online` (backend-v2.js:12717).
            refusal(res, StatusCode::CONFLICT, "agent_already_online")
        }
        Err(hagency_store::Error::State) => {
            // `agent lifecycle cleanup is still pending` (backend-v2.js:12719).
            refusal(res, StatusCode::CONFLICT, "agent_lifecycle_busy")
        }
        Err(error) => failure(res, error),
    }
}

/// Stop — fence, never settle: the store resolves the named engagement's
/// dispatch (live set or unsettled stop row) and serves the five-key wire
/// object verbatim. Idempotent by construction (see `stop_dispatch_for_agent`).
#[handler]
async fn stop(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if query(req, &[], 0).is_err() {
        failed(res, Error::Invalid);
        return;
    }
    let id = match engagement_id(req) {
        Ok(id) => id,
        Err(error) => {
            failed(res, error);
            return;
        }
    };
    if !check_lifecycle(depot, res) {
        return;
    }
    let Some(store) = domain(depot, res) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or_default();
    let result = store.stop_agent(id, "console".to_owned(), now).await;
    if result.is_ok() && recheck(depot).is_err() {
        failure(res, hagency_store::Error::OutcomeUnknown);
        return;
    }
    match result {
        Ok(value) => res.render(Json(value)),
        Err(hagency_store::Error::NotFound) => refusal(res, StatusCode::NOT_FOUND, "not_found"),
        Err(error) => failure(res, error),
    }
}

/// Refuse — the operator's verdict against a pending engagement request
/// (parity: the retained `POST /api/engagements/:id/verdict` else-branch,
/// backend-v2.js:15160-15187 → lib/engagement-store.js:593-613). Reaches the
/// store's own refusal arm `DomainStore::reject` → `end(..., revoke = false)`
/// — never a second write path; the pending-only guard, decision idempotency
/// and the engagement_ends stamp stay the store's. Triggered ONLY by this
/// route, under the existing `Scope::AgentLifecycle` (same class as retire);
/// no timer, no sweep, and a refusal schedules no retirement work.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Refuse {
    command_id: String,
}

#[handler]
async fn refuse(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if query(req, &[], 0).is_err() {
        failed(res, Error::Invalid);
        return;
    }
    let id = match engagement_id(req) {
        Ok(id) => id,
        Err(error) => {
            failed(res, error);
            return;
        }
    };
    if !check_lifecycle(depot, res) {
        return;
    }
    let input: Refuse =
        match serde_json::from_slice::<Refuse>(&body(req, 512).await.unwrap_or_default()) {
            Ok(input) if identifier(&input.command_id, 128).is_ok() => input,
            _ => {
                failed(res, Error::Invalid);
                return;
            }
        };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.reject(input.command_id, id).await;
    if result.is_ok() && recheck(depot).is_err() {
        failure(res, hagency_store::Error::OutcomeUnknown);
        return;
    }
    match result {
        Ok(engagement) => res.render(Json(serde_json::json!({"engagement": engagement}))),
        Err(hagency_store::Error::State) => {
            // The pending-only guard (domain.rs:1294-1300): refusal of an
            // already-terminal or reserved/active engagement, conflict word as
            // the retained verdict route maps it (409).
            refusal(res, StatusCode::CONFLICT, "engagement_not_pending")
        }
        Err(hagency_store::Error::NotFound) => refusal(res, StatusCode::NOT_FOUND, "not_found"),
        Err(hagency_store::Error::Conflict) => {
            // Reused command id with a different decision digest
            // (replay_decision, domain.rs:359-381): a changed replay, never a
            // silent second write.
            refusal(res, StatusCode::CONFLICT, "decision_conflict")
        }
        // ADR-191: the engagement's coordinator decides this request in Rinx.
        Err(hagency_store::Error::LocalAuthority) => {
            refusal(res, StatusCode::CONFLICT, "coordinator_managed")
        }
        Err(error) => failure(res, error),
    }
}

/// Recover-dispatch — operator recovery and resume of an orphaned dispatch
/// (ADR-148). The operator names the crashed dispatch's replacement and the
/// evidence of what was inspected; the store enforces the orphan state, the
/// stop-row refusal and the evidence record. Triggered ONLY by this route,
/// under the existing `Scope::AgentLifecycle` — never automatic, never a sweep,
/// and no second clearer over the stop-fenced sibling's state.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RecoverDispatch {
    original: String,
    replacement: hagency_core::tasks::DispatchInput,
    evidence: String,
}

#[handler]
async fn recover_dispatch(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if query(req, &[], 0).is_err() {
        failed(res, Error::Invalid);
        return;
    }
    let id = match engagement_id(req) {
        Ok(id) => id,
        Err(error) => {
            failed(res, error);
            return;
        }
    };
    if !check_lifecycle(depot, res) {
        return;
    }
    let raw = match body(req, 8192).await {
        Ok(raw) => raw,
        Err(_) => {
            failed(res, Error::Invalid);
            return;
        }
    };
    let input: RecoverDispatch = match serde_json::from_slice(&raw) {
        Ok(input) => input,
        Err(_) => {
            failed(res, Error::Invalid);
            return;
        }
    };
    if input.replacement.validate().is_err()
        || identifier(&input.original, 128).is_err()
        || input.evidence.is_empty()
        || input.evidence.chars().count() > 4096
    {
        failed(res, Error::Invalid);
        return;
    }
    let Some(store) = domain(depot, res) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or_default();
    let result = store
        .recover_dispatch(
            id,
            input.original.clone(),
            input.replacement,
            input.evidence,
            now,
        )
        .await;
    if result.is_ok() && recheck(depot).is_err() {
        failure(res, hagency_store::Error::OutcomeUnknown);
        return;
    }
    match result {
        Ok(()) => res.render(Json(serde_json::json!({"ok": true}))),
        Err(hagency_store::Error::State) => {
            // The orphan-state guard or the stop-row refusal (execution.rs:1044-1050).
            refusal(res, StatusCode::CONFLICT, "dispatch_not_recoverable")
        }
        Err(hagency_store::Error::NotFound) => refusal(res, StatusCode::NOT_FOUND, "not_found"),
        // The original was already recovered, or the replacement id is taken:
        // the recovery happened once and a replay mints nothing (live proof
        // 2026-09-22 answered this with the resources page's revision word).
        Err(hagency_store::Error::Conflict) => {
            refusal(res, StatusCode::CONFLICT, "recovery_conflict")
        }
        Err(error) => failure(res, error),
    }
}

/// Bounded private discovery; inspection availability grants no resolution.
#[handler]
async fn stopped_dispatches(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !check_lifecycle(depot, res) {
        return;
    }
    let prepared = (|| {
        query(req, &["after"], 160)?;
        let agent = engagement_id(req)?;
        let after = req.query::<String>("after").unwrap_or_default();
        if !after.is_empty() {
            identifier(&after, 128).map_err(|_| Error::Invalid)?;
        }
        Ok::<_, Error>((agent, after))
    })();
    let (agent, after) = match prepared {
        Ok(value) => value,
        Err(error) => {
            failed(res, error);
            return;
        }
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.stopped_dispatches_for_agent(agent, after).await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    outcome_response(res, result);
}

/// The historical content inventory is private operator data, even on GET.
#[handler]
async fn stopped_dispatch_inspection(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !check_lifecycle(depot, res) {
        return;
    }
    let prepared = (|| {
        query(req, &[], 0)?;
        let agent = engagement_id(req)?;
        let id = req.param::<String>("dispatch").ok_or(Error::Invalid)?;
        identifier(&id, 128).map_err(|_| Error::Invalid)?;
        Ok::<_, Error>((agent, id))
    })();
    let (agent, id) = match prepared {
        Ok(value) => value,
        Err(error) => {
            failed(res, error);
            return;
        }
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.stopped_dispatch_inspection(agent, id).await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    match result {
        Ok(value) => res.render(Json(value)),
        Err(hagency_store::Error::NotFound) => refusal(res, StatusCode::NOT_FOUND, "not_found"),
        Err(error) => failure(res, error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ContinueStoppedDispatch {
    original: String,
    fence: u64,
    inspection_digest: String,
    replacement: hagency_core::tasks::DispatchInput,
    evidence: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct OutcomeInspection {
    #[serde(default = "inspection_lifetime")]
    ttl_ms: u64,
}
fn inspection_lifetime() -> u64 {
    900_000
}

fn outcome_response(res: &mut Response, result: Result<serde_json::Value, hagency_store::Error>) {
    match result {
        Ok(value) => res.render(Json(value)),
        Err(hagency_store::Error::NotFound) => refusal(res, StatusCode::NOT_FOUND, "not_found"),
        Err(hagency_store::Error::Invalid(_)) => failed(res, Error::Invalid),
        Err(hagency_store::Error::Conflict) => {
            refusal(res, StatusCode::CONFLICT, "resolution_conflict")
        }
        Err(
            hagency_store::Error::State
            | hagency_store::Error::Quarantined
            | hagency_store::Error::RunnerAuthority
            | hagency_store::Error::Generation,
        ) => refusal(res, StatusCode::CONFLICT, "dispatch_not_resolvable"),
        Err(error) => failure(res, error),
    }
}

#[handler]
async fn begin_outcome_inspection(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !check_lifecycle(depot, res) {
        return;
    }
    let prepared = async {
        query(req, &[], 0)?;
        let agent = engagement_id(req)?;
        let id = req.param::<String>("dispatch").ok_or(Error::Invalid)?;
        identifier(&id, 128).map_err(|_| Error::Invalid)?;
        let input: OutcomeInspection =
            serde_json::from_slice(&body(req, 512).await?).map_err(|_| Error::Invalid)?;
        Ok::<_, Error>((agent, id, input))
    }
    .await;
    let (agent, id, input) = match prepared {
        Ok(value) => value,
        Err(error) => {
            failed(res, error);
            return;
        }
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store
        .begin_outcome_inspection(agent, id, input.ttl_ms)
        .await;
    if result.is_ok() && recheck(depot).is_err() {
        failure(res, hagency_store::Error::OutcomeUnknown);
        return;
    }
    outcome_response(res, result);
}

#[handler]
async fn resolve_stopped_dispatch(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !check_lifecycle(depot, res) {
        return;
    }
    let prepared = async {
        query(req, &[], 0)?;
        let agent = engagement_id(req)?;
        let input: hagency_store::OutcomeResolution =
            serde_json::from_slice(&body(req, 16 * 1024).await?).map_err(|_| Error::Invalid)?;
        Ok::<_, Error>((agent, input))
    }
    .await;
    let (agent, input) = match prepared {
        Ok(value) => value,
        Err(error) => {
            failed(res, error);
            return;
        }
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.resolve_stopped_dispatch(agent, input).await;
    if result.is_ok() && recheck(depot).is_err() {
        failure(res, hagency_store::Error::OutcomeUnknown);
        return;
    }
    outcome_response(res, result);
}

#[handler]
async fn continue_stopped_dispatch(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !check_lifecycle(depot, res) {
        return;
    }
    let prepared = async {
        query(req, &[], 0)?;
        let agent = engagement_id(req)?;
        let input: ContinueStoppedDispatch =
            serde_json::from_slice(&body(req, 16 * 1024).await?).map_err(|_| Error::Invalid)?;
        Ok::<_, Error>((agent, input))
    }
    .await;
    let (agent, input) = match prepared {
        Ok(value) => value,
        Err(error) => {
            failed(res, error);
            return;
        }
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store
        .continue_stopped_dispatch(
            agent,
            input.original,
            (input.fence, input.inspection_digest),
            input.replacement,
            input.evidence,
        )
        .await;
    if result.is_ok() && recheck(depot).is_err() {
        failure(res, hagency_store::Error::OutcomeUnknown);
        return;
    }
    match result {
        Ok(()) => res.render(Json(serde_json::json!({"ok":true}))),
        Err(hagency_store::Error::NotFound) => refusal(res, StatusCode::NOT_FOUND, "not_found"),
        Err(hagency_store::Error::Invalid(_)) => failed(res, Error::Invalid),
        Err(hagency_store::Error::Conflict) => {
            refusal(res, StatusCode::CONFLICT, "continuation_conflict")
        }
        Err(
            hagency_store::Error::State
            | hagency_store::Error::Quarantined
            | hagency_store::Error::RunnerAuthority
            | hagency_store::Error::Generation,
        ) => refusal(res, StatusCode::CONFLICT, "dispatch_not_continuable"),
        Err(error) => failure(res, error),
    }
}

/// Preset (resource) rebind — TS parity `PUT /api/agents/:name/preset`
/// (backend-v2.js:11484-11522): the binding, the ceiling and the profile
/// move together; the next dispatch the host claims runs on the new
/// resource (the claim selector reads the provision effect's resource
/// payload, which the store rewrites in the same transaction).
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Preset {
    preset_id: String,
}

#[handler]
async fn preset(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let prepared = async {
        query(req, &[], 0)?;
        let id = engagement_id(req)?;
        let raw = body(req, 4096).await?;
        let input: Preset = serde_json::from_slice(&raw).map_err(|_| Error::Invalid)?;
        // Absent and empty both mean unbind in TS; native refuses — an
        // engagement cannot exist without a resource.
        if input.preset_id.trim().is_empty() {
            return Err(Error::Invalid);
        }
        identifier(&input.preset_id, 128).map_err(|_| Error::Invalid)?;
        Ok::<_, Error>((id, input.preset_id.trim().to_owned()))
    }
    .await;
    let (id, requested) = match prepared {
        Ok(value) => value,
        Err(error) => {
            failed(res, error);
            return;
        }
    };
    if !check_lifecycle(depot, res) {
        return;
    }
    let Some(store) = domain(depot, res) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or_default();
    let result = store.rebind_agent_resource(id, requested, now).await;
    if result.is_ok() && recheck(depot).is_err() {
        failure(res, hagency_store::Error::OutcomeUnknown);
        return;
    }
    match result {
        Ok(value) => res.render(Json(value)),
        Err(hagency_store::Error::NotFound) => refusal(res, StatusCode::NOT_FOUND, "not_found"),
        Err(hagency_store::Error::Invalid(_)) => {
            refusal(res, StatusCode::BAD_REQUEST, "unknown_preset")
        }
        Err(error) => failure(res, error),
    }
}

/// `DELETE /api/agents/:name` — the retained soft/force delete
/// (`backend-v2.js:12164-12307`).
///
/// SOFT (the default) is TS's reversible act: it reports
/// `{ok, deprecated, message}` and changes nothing that an `undelete`
/// would have to undo. TS marks its agent record inactive; native has no
/// agent record (an agent is DERIVED from its engagements), so there is
/// nothing to mark — and deliberately NOTHING is revoked, which is the
/// distinction TS's own comment makes: a soft delete is reversible and
/// `revoke` has no inverse, so only `?force=true` may touch commitments.
///
/// FORCE really deletes: TS revokes the agent's ACTIVE engagements and
/// reports the released ids, because a commitment outlives its agent
/// otherwise (`backend-v2.js:12206-12225`). Native's `revoke` is that same
/// act and releases the budget by construction (a commitment IS the
/// engagement's state), so force maps onto the existing retirement path
/// rather than a second write path — each revocation carries its own
/// deterministic command id, so a retried delete replays instead of
/// double-acting.
///
/// What native does NOT do, and does not pretend to: TS also leaves the
/// customer's project rooms and its groups, and kills the agent's tmux
/// session. Native has no group concept and no room-withdrawal or session
/// kill at this layer (the retirement the revoke schedules owns worker
/// cleanup), so those are reported empty/false exactly as TS reports them
/// for a deployment that has none — never invented. See the report.
#[handler]
async fn delete(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    // `?force=true` is TS's trigger, compared as the STRING 'true' — any
    // other value (or none) is the soft, reversible path.
    if query(req, &["force"], 64).is_err() {
        failed(res, Error::Invalid);
        return;
    }
    let force = req.query::<String>("force").as_deref() == Some("true");
    // The same name shape `agent_detail` accepts — validated here because
    // that module's helper is private to it (a delete must not address a
    // name the detail read would refuse).
    let Some(name) = req.param::<String>("name") else {
        failed(res, Error::Invalid);
        return;
    };
    if AgentName::try_from(name.clone()).is_err() {
        failed(res, Error::Invalid);
        return;
    }
    // Deleting is a lifecycle act: the same finite scope `retire` requires,
    // refused before any store read.
    if !check_lifecycle(depot, res) {
        return;
    }
    let Some(store) = domain(depot, res) else {
        return;
    };
    let detail = store.agent_detail(&name).await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    let detail = match detail {
        Ok(Some(detail)) => detail,
        // TS: 404 `{error:'agent not found'}` when no record names it.
        Ok(None) => {
            res.status_code(StatusCode::NOT_FOUND);
            res.render(Json(serde_json::json!({
                "error": "agent not found",
                "code": "agent_not_found",
            })));
            return;
        }
        Err(error) => {
            failure(res, error);
            return;
        }
    };

    if !force {
        res.render(Json(serde_json::json!({
            "ok": true,
            "deprecated": true,
            // TS's message, verbatim.
            "message": "unregister is disabled; agent marked inactive. Use ?force=true to permanently delete.",
            "agent": detail,
        })));
        return;
    }

    // FORCE: release this agent's active engagements. A failure here stops
    // the delete rather than proceeding — removing the agent while its
    // commitment stands is the very leak this closes (TS's
    // `engagement_release_failed`, `backend-v2.js:12238-12252`).
    let active = match store.agent_active_engagements(name.clone()).await {
        Ok(active) => active,
        Err(error) => {
            failure(res, error);
            return;
        }
    };
    let mut released = Vec::with_capacity(active.len());
    for id in active {
        let command = delete_command_id(&name, &id);
        match store.revoke(command, id.clone()).await {
            Ok(_) => released.push(id),
            Err(error) => {
                if let Err(error) = recheck(depot) {
                    failed(res, error);
                    return;
                }
                // TS names the code and the agent it could not remove.
                refusal(
                    res,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "engagement_release_failed",
                );
                let _ = error;
                return;
            }
        }
    }
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    // The tombstone `undelete` reverses: TS writes
    // `deletedAgentTombstones[name] = { deletedAt, reason: 'force-delete' }`
    // (`persistForceDeletedAgentState`, `backend-v2.js:4342-4359`) as the
    // durable record that re-registration must clear, and a persistence
    // failure stops the delete with 503 (`:12282`) rather than reporting an
    // agent as removed that undelete could never restore.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or_default();
    if let Err(error) = store
        .record_agent_tombstone(name.clone(), "force-delete".into(), now)
        .await
    {
        if let Err(error) = recheck(depot) {
            failed(res, error);
            return;
        }
        res.status_code(StatusCode::SERVICE_UNAVAILABLE);
        res.render(Json(serde_json::json!({
            "error": "agent force-delete persistence failed",
        })));
        let _ = error;
        return;
    }
    res.render(Json(serde_json::json!({
        "ok": true,
        "deleted": true,
        "name": name,
        // Native has no tmux/session kill at this layer: the revoke above
        // schedules the retirement that owns worker cleanup. Reported
        // false rather than claimed.
        "sessionKilled": false,
        "releasedEngagements": released,
        "leftGroups": Vec::<String>::new(),
        "leftProjectRooms": Vec::<String>::new(),
    })));
}

/// A deterministic command id for one force-delete revocation, so a retried
/// `DELETE ?force=true` replays the store's decision instead of acting twice
/// (the store replays an identical command id by digest).
fn delete_command_id(name: &str, engagement: &str) -> String {
    let digest = Sha256::digest(format!("delete_agent\u{0}{name}\u{0}{engagement}").as_bytes());
    format!(
        "delete_{}",
        digest[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

fn store_error(res: &mut Response, error: hagency_store::Error) {
    let (status, code) = match error {
        hagency_store::Error::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_roster_query"),
        hagency_store::Error::Busy => (StatusCode::SERVICE_UNAVAILABLE, "busy"),
        hagency_store::Error::OutcomeUnknown => (StatusCode::GATEWAY_TIMEOUT, "outcome_unknown"),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "roster_unavailable"),
    };
    refusal(res, status, code);
}
