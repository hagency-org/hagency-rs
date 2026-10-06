//! The engagement retire surface (ADR-150, spec `task-rust-engagement-retire`):
//! the operator's retirement decision and its cleanup retry, both console
//! mutations under the console's existing `Scope::AgentLifecycle` — the same
//! finite scope the agents routes gate on (`agents.rs:139-160`). The store owns
//! every guarantee (ADR-150 §"keep the store's own guarantees"): the state
//! guard (`domain.rs:1294-1300`), the decision digest and replay
//! (`:1289-1292`, `:359-381`), the same-transaction provision-cancel and
//! retire-schedule (`:1308-1314`), the `engagement_ends` stamp (`:1324-1328`)
//! and the retry's failed-only reset (`:1274-1280`). The route carries the
//! operator's `commandId` verbatim — idempotency is the store's, never the
//! route's — and invents no outcome, no sweep and no timer: there is NO
//! automatic driver anywhere; a failed retirement waits for this operator act.
//!
//! Reads stay scope-free (the engagements read lives in `usage.rs`); only the
//! mutations consult the authority, and the missing-scope word
//! (`agent_lifecycle_scope_required`) is served before any store job, so a
//! read-only session changes nothing. The wire answer is the bounded decision
//! receipt — `id`, `state`, `cleanup` — never the full engagement
//! row and no project, room, resource or token field.
use super::{Error, Session, body, console, failed, recheck, usage::query};
use crate::{refusal, refusal_explained, resources::domain};
use hagency_core::project::{Engagement, EngagementState, identifier};
use hagency_store::DomainStore;
use salvo::prelude::*;
use serde::Deserialize;

pub(super) fn router() -> Router {
    Router::new()
        .push(Router::with_path("engagements/{id}/retire").post(retire))
        .push(Router::with_path("engagements/{id}/cleanup-retry").post(cleanup_retry))
        .push(Router::with_path("engagements/{id}/candidates").get(candidates))
        .push(Router::with_path("engagements/{id}/approve").post(approve))
        .push(Router::with_path("engagements/{id}/allocation").post(allocation))
        .push(
            Router::with_path("engagements/{id}/settlement")
                .get(settlement)
                .post(settle),
        )
        .push(Router::with_path("engagements/audit").get(audit))
}

#[handler]
async fn settlement(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if query(req, &[], 0).is_err() {
        failed(res, Error::Invalid);
        return;
    }
    let id = req.param::<String>("id").unwrap_or_default();
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.coordinator_settlement(id).await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    match result {
        Ok(value) => res.render(Json(value)),
        Err(error) => store_error(res, error),
    }
}
#[handler]
async fn settle(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let prepared = async {
        query(req, &[], 0)?;
        let session = depot
            .get_typed::<Session>()
            .map_err(|_| Error::Unauthorized)?;
        let access = console(depot)?;
        if !access.0.authority.can_configure(session)? {
            return Err(Error::ConfigurationForbidden);
        }
        let raw = body(req, 4096).await?;
        let input: hagency_store::coordinator::FinalUsage =
            serde_json::from_slice(&raw).map_err(|_| Error::Invalid)?;
        if req.param::<String>("id").as_deref() != Some(input.agent_allocation_id.as_str()) {
            return Err(Error::Invalid);
        }
        access.0.authority.settlement(session, input)
    }
    .await;
    let command = match prepared {
        Ok(v) => v,
        Err(e) => {
            failed(res, e);
            return;
        }
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.settle_coordinator_agent(command).await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    match result {
        Ok(mut value) => {
            value.as_object_mut().unwrap().remove("evidenceReference");
            res.render(Json(value));
        }
        Err(error) => store_error(res, error),
    }
}

fn verdict_store_error(res: &mut Response, error: hagency_store::Error) {
    match error {
        hagency_store::Error::State => refusal(res, StatusCode::CONFLICT, "engagement_not_pending"),
        hagency_store::Error::NotFound => refusal(res, StatusCode::NOT_FOUND, "not_found"),
        hagency_store::Error::Conflict => refusal(res, StatusCode::CONFLICT, "decision_conflict"),
        hagency_store::Error::Generation => {
            refusal(res, StatusCode::CONFLICT, "registration_generation")
        }
        hagency_store::Error::Unqualified => {
            refusal(res, StatusCode::CONFLICT, "agent_unavailable")
        }
        hagency_store::Error::InsufficientCapacity => {
            refusal(res, StatusCode::CONFLICT, "insufficient_capacity")
        }
        hagency_store::Error::NoCeiling => refusal(res, StatusCode::CONFLICT, "no_ceiling"),
        // ADR-191: the engagement's coordinator decides this request in Rinx.
        hagency_store::Error::LocalAuthority => {
            refusal(res, StatusCode::CONFLICT, "coordinator_managed")
        }
        // ADR-186 §A2: the refusal carries the store's own explanation of
        // the binding limit, which the console shows beside the amount.
        hagency_store::Error::OverCommit { message } => {
            refusal_explained(res, StatusCode::CONFLICT, "over_commit", &message)
        }
        hagency_store::Error::Invalid(_) => refusal(res, StatusCode::BAD_REQUEST, "invalid"),
        error => store_error(res, error),
    }
}

fn store_error(res: &mut Response, error: hagency_store::Error) {
    let (status, code) = match error {
        hagency_store::Error::State => (StatusCode::CONFLICT, "engagement_not_live"),
        hagency_store::Error::Conflict => (StatusCode::CONFLICT, "command_conflict"),
        hagency_store::Error::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        hagency_store::Error::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_engagement_id"),
        hagency_store::Error::Busy => (StatusCode::SERVICE_UNAVAILABLE, "busy"),
        hagency_store::Error::OutcomeUnknown => (StatusCode::GATEWAY_TIMEOUT, "outcome_unknown"),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "engagement_unavailable"),
    };
    refusal(res, status, code);
}

/// The lifecycle gate shared with the agents mutations: any authenticated
/// session may read, only `Scope::AgentLifecycle` may decide a retirement.
pub(super) fn check_lifecycle(depot: &Depot, res: &mut Response) -> bool {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Command {
    command_id: String,
}

/// The approval body (ADR-186 §A1): the command id and, optionally, the
/// amount the operator grants. Absent means the requested amount.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ApproveCommand {
    command_id: String,
    #[serde(default)]
    allocated_tokens: Option<u64>,
}

/// The engagement's own body shapes, each carrying the operator's command id.
trait CommandBody: serde::de::DeserializeOwned {
    fn command_id(&self) -> &str;
}
impl CommandBody for Command {
    fn command_id(&self) -> &str {
        &self.command_id
    }
}
impl CommandBody for ApproveCommand {
    fn command_id(&self) -> &str {
        &self.command_id
    }
}

/// The top-up body (ADR-186 §C1): the command id and the tokens to add.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AllocationCommand {
    command_id: String,
    add_tokens: u64,
}
impl CommandBody for AllocationCommand {
    fn command_id(&self) -> &str {
        &self.command_id
    }
}

/// Validate path and body, returning (engagement id, command id) or having
/// already served the refusal. The command id is the operator's idempotency
/// key: the store replays it by digest, so a re-POST with the same key and the
/// same act is the retained product's re-POST (backend-v2.js:15233-15234,
/// "The decision is already durable. Retry only detachment"), and a reused key
/// with different content is a conflict — both decided by the store, never by
/// this route.
async fn decision(
    req: &mut Request,
    depot: &Depot,
    res: &mut Response,
) -> Option<(String, String)> {
    decision_body::<Command>(req, depot, res)
        .await
        .map(|(id, input)| (id, input.command_id))
}

/// `decision` for a body that carries more than the command id: the same
/// path, scope, size and command-id checks, and the parsed body returned.
async fn decision_body<T: CommandBody>(
    req: &mut Request,
    depot: &Depot,
    res: &mut Response,
) -> Option<(String, T)> {
    if req.uri().query().is_some() {
        failed(res, Error::Invalid);
        return None;
    }
    let id = match req.param::<String>("id") {
        Some(id) => id,
        None => {
            failed(res, Error::Invalid);
            return None;
        }
    };
    if identifier(&id, 128).is_err() {
        failed(res, Error::Invalid);
        return None;
    }
    if !check_lifecycle(depot, res) {
        return None;
    }
    let raw = match body(req, 512).await {
        Ok(raw) => raw,
        Err(_) => {
            failed(res, Error::Invalid);
            return None;
        }
    };
    let input: T = match serde_json::from_slice(&raw) {
        Ok(input) => input,
        Err(_) => {
            failed(res, Error::Invalid);
            return None;
        }
    };
    if identifier(input.command_id(), 512).is_err() {
        failed(res, Error::Invalid);
        return None;
    }
    Some((id, input))
}

fn receipt(res: &mut Response, result: Result<Engagement, hagency_store::Error>) {
    match result {
        Ok(engagement) => res.render(Json(serde_json::json!({
            "id": engagement.id,
            "state": engagement.state,
            "cleanup": engagement.cleanup,
        }))),
        Err(error) => store_error(res, error),
    }
}

/// The retirement decision: `DomainStore::revoke` → the store's `end(…, true)`
/// — the single writer of `revoked`. The store refuses an already-terminal
/// engagement (`Error::State`), replays an identical command id by digest, and
/// conflicts on a reused id with different content. The route adds no guard of
/// its own: a weaker route-side check could only diverge from the store's.
#[handler]
async fn retire(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some((id, command)) = decision(req, depot, res).await else {
        return;
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.revoke(command, id).await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    receipt(res, result);
}

/// The operator's cleanup retry: `DomainStore::retry_cleanup` resets exactly
/// one failed retire effect to pending with its outcome digest cleared
/// (`domain.rs:1274-1280`). Nothing else may re-drive a failed retirement —
/// no timer, no sweep, no startup pass — so this act is the only path back.
#[handler]
async fn cleanup_retry(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some((id, command)) = decision(req, depot, res).await else {
        return;
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.retry_cleanup(command, id).await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    receipt(res, result);
}

/// The engagement's candidate resources for the console approve form
/// (parity: backend-v2.js:15154-15159, `GET /api/engagements/:id/candidates`).
/// The native store fixes the resource at admission — the retained
/// `project-definition` choice — so the read names the stored row and its
/// headroom, never a second qualification path. `locked` is the retained
/// `Boolean(e.fulfillment) || e.state !== 'pending'` word: native has no
/// partial fulfillment record, so the state is the whole condition.
#[handler]
async fn candidates(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if req.uri().query().is_some() {
        failed(res, Error::Invalid);
        return;
    }
    let Some(id) = req.param::<String>("id") else {
        failed(res, Error::Invalid);
        return;
    };
    if identifier(&id, 128).is_err() {
        failed(res, Error::Invalid);
        return;
    }
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.engagement(id).await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    let engagement = match result {
        Ok(value) => value,
        Err(error) => {
            verdict_store_error(res, error);
            return;
        }
    };
    let resource = match store
        .resource_configuration(engagement.resource_id.clone())
        .await
    {
        Ok(value) => value,
        Err(error) => {
            if let Err(check) = recheck(depot) {
                failed(res, check);
            } else {
                verdict_store_error(res, error);
            }
            return;
        }
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or_default();
    // ADR-186 §A3: the same smallest of ceiling, seat and pool headroom the
    // approval is checked against, so "All remaining" can never be refused.
    let headroom = store
        .engagement_headroom(engagement.id.clone(), now)
        .await
        .ok()
        .flatten();
    let locked = engagement.state != EngagementState::Pending;
    res.render(Json(serde_json::json!({
        "candidates": [{
            "choice": {"kind": "project-definition"},
            "name": engagement.agent_name.as_str(),
            "resource": resource.preset_id,
            "framework": resource.framework,
            "model": resource.model,
            "reasoning": resource.reasoning,
            "provision": true,
            "remainingTokens": headroom,
        }],
        "locked": locked,
        "allocation": {"kind": "project-definition"},
    })));
}

/// The operator's approval from the console (parity: the `approve === true`
/// branch of backend-v2.js:15161-15179). Reaches the SAME store verdict the
/// Matrix intake reaches — `DomainStore::approve` — by rebuilding the
/// verified request from the admitted engagement's stored
/// `context`/`evidence` and re-running `verify_request` over it. The
/// observation structs carry no Deserialize by design (authority.rs:1-4), so
/// each field is re-read from the stored JSON; the observation time is this
/// act's, the same refresh the bootstrap adopt path applies
/// (bootstrap/provision.rs:519-523) — the facts stay the admission's own
/// verified evidence, and the store's digest, generation, pending-only and
/// budget guards bind unchanged.
#[handler]
async fn approve(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some((
        id,
        ApproveCommand {
            command_id: command,
            allocated_tokens,
        },
    )) = decision_body::<ApproveCommand>(req, depot, res).await
    else {
        return;
    };
    // ADR-186 §A1: a chosen amount is a positive integer.
    if allocated_tokens == Some(0) {
        failed(res, Error::Invalid);
        return;
    }
    let Some(store) = domain(depot, res) else {
        return;
    };
    let registration = match store
        .provisioning_registration_for_engagement(id.clone())
        .await
    {
        Ok(value) => value,
        Err(error) => {
            if let Err(check) = recheck(depot) {
                failed(res, check);
            } else {
                verdict_store_error(res, error);
            }
            return;
        }
    };
    let engagement = match store.engagement(id.clone()).await {
        Ok(value) => value,
        Err(error) => {
            if let Err(check) = recheck(depot) {
                failed(res, check);
            } else {
                verdict_store_error(res, error);
            }
            return;
        }
    };
    let evidence = match store
        .provisioning_request_evidence(registration.fleet_id.clone(), engagement.request_id.clone())
        .await
    {
        Ok(Some((context, stored, _))) => (context, stored),
        Ok(None) => {
            verdict_store_error(res, hagency_store::Error::NotFound);
            return;
        }
        Err(error) => {
            if let Err(check) = recheck(depot) {
                failed(res, check);
            } else {
                verdict_store_error(res, error);
            }
            return;
        }
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or_default();
    let Some(verified) =
        rebuild_verified(&registration, evidence, engagement.request_id.clone(), now)
    else {
        verdict_store_error(
            res,
            hagency_store::Error::Invalid(hagency_core::InvalidInput(
                "stored admission evidence failed re-verification",
            )),
        );
        return;
    };
    let granted = allocated_tokens.unwrap_or(u64::from(engagement.requested_tokens));
    let result = store
        .approve_allocating(command, verified, now, allocated_tokens)
        .await;
    if result.is_ok() && recheck(depot).is_err() {
        verdict_store_error(res, hagency_store::Error::OutcomeUnknown);
        return;
    }
    match result {
        Err(hagency_store::Error::InsufficientCapacity) => {
            capacity_refusal(res, &store, &engagement, granted, now).await
        }
        // A refused approval (over the remaining allocation, no ceiling, a
        // decided engagement) is a verdict refusal the console names, not an
        // unreadable engagement.
        Err(error) => verdict_store_error(res, error),
        ok => receipt(res, ok),
    }
}

/// The operator's top-up (ADR-186 §C): `DomainStore::raise_allocation`
/// raises a reserved or active engagement's allocation by `addTokens`,
/// checked like an approval against the headroom left after its current
/// allocation, idempotent by `commandId` (the store's decision receipts).
/// The store lifts a quota hold the new allocation clears, in the same
/// transaction; Palpo reads the new figure on its next status refresh. The
/// answer is the bounded decision receipt, like every engagement mutation.
#[handler]
async fn allocation(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some((
        id,
        AllocationCommand {
            command_id: command,
            add_tokens,
        },
    )) = decision_body::<AllocationCommand>(req, depot, res).await
    else {
        return;
    };
    if add_tokens == 0 {
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
        .raise_allocation(command, id.clone(), add_tokens, now)
        .await;
    if result.is_ok() && recheck(depot).is_err() {
        verdict_store_error(res, hagency_store::Error::OutcomeUnknown);
        return;
    }
    match result {
        Ok(engagement) => receipt(res, Ok(engagement)),
        Err(hagency_store::Error::InsufficientCapacity) => match store.engagement(id).await {
            Ok(engagement) => capacity_refusal(res, &store, &engagement, add_tokens, now).await,
            Err(_) => refusal(res, StatusCode::CONFLICT, "insufficient_capacity"),
        },
        // Only a reserved or active engagement holds an allocation to raise.
        Err(hagency_store::Error::State) => {
            refusal(res, StatusCode::CONFLICT, "engagement_not_live")
        }
        Err(error) => verdict_store_error(res, error),
    }
}

/// ADR-186 §A2 for the seat or pool side: the store keeps the bare
/// `insufficient_capacity` identity (no ceiling report binds), so the route
/// names the headroom the approval was checked against, read in a second
/// job. If that read fails the code alone is served — the refusal itself is
/// already decided.
async fn capacity_refusal(
    res: &mut Response,
    store: &DomainStore,
    engagement: &Engagement,
    granted: u64,
    now: u64,
) {
    match store.engagement_headroom(engagement.id.clone(), now).await {
        Ok(Some(left)) => {
            let message = format!(
                "{} (the shared seat or resource pool is the binding limit)",
                hagency_core::ceiling::over_commit_message(
                    engagement.agent_name.as_str(),
                    granted,
                    left,
                    None,
                )
            );
            refusal_explained(res, StatusCode::CONFLICT, "insufficient_capacity", &message)
        }
        _ => refusal(res, StatusCode::CONFLICT, "insufficient_capacity"),
    }
}

/// Field-by-field rebuild of the admission evidence into fresh observation
/// values, then the one `verify_request` — the same authority checks the
/// Matrix intake ran, over the same stored facts. The stored evidence was
/// serialized from `RequestObservation` with no serde rename, so every key is
/// snake_case. Returns None when any field is missing or verification
/// refuses.
fn rebuild_verified(
    registration: &hagency_core::authority::Registration,
    evidence: (String, String),
    request_id: String,
    now: u64,
) -> Option<hagency_core::authority::VerifiedRequest> {
    use hagency_core::authority::{RequestObservation, RoomObservation, SourceObservation};
    let request: hagency_core::authority::ProjectRequest =
        serde_json::from_str(&evidence.0).ok()?;
    if request.request_id != request_id {
        return None;
    }
    let observation: serde_json::Value = serde_json::from_str(&evidence.1).ok()?;
    let room = |value: &serde_json::Value| -> Option<RoomObservation> {
        Some(RoomObservation {
            room_id: value.get("room_id")?.as_str()?.to_owned(),
            joined: value
                .get("joined")?
                .as_array()?
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<std::collections::BTreeSet<_>>>()?,
            invite_only: value.get("invite_only")?.as_bool()?,
            encryption: value
                .get("encryption")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            powers: value
                .get("powers")?
                .as_object()?
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_i64()?)))
                .collect(),
            default_power: value.get("default_power")?.as_i64()?,
            invite_power: value.get("invite_power")?.as_i64()?,
            binding: value.get("binding").cloned(),
            name: value
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
        })
    };
    let observation = RequestObservation {
        registration_generation: observation.get("registration_generation")?.as_u64()?,
        // This act's time, the bootstrap adopt refresh: the room facts are
        // the admission's stored verified evidence, not a fresh network read
        // a console route cannot make.
        observed_at_ms: now,
        source: {
            let value = observation.get("source")?;
            SourceObservation {
                event_id: value.get("event_id")?.as_str()?.to_owned(),
                room_id: value.get("room_id")?.as_str()?.to_owned(),
                sender: value.get("sender")?.as_str()?.to_owned(),
                event_type: value.get("event_type")?.as_str()?.to_owned(),
                content: value.get("content")?.clone(),
            }
        },
        reception: room(observation.get("reception")?)?,
        project: room(observation.get("project")?)?,
        owner_room: room(observation.get("owner_room")?)?,
    };
    hagency_core::authority::verify_request(registration, request, observation).ok()
}

/// The console verdict audit (parity: backend-v2.js:14980-14983,
/// `GET /api/engagements/audit?limit=`): the newest decisions newest-first,
/// default limit 16, the retained entry shape `{type, at, ...detail}`. A
/// read — any authenticated session may read it; no lifecycle scope needed.
#[handler]
async fn audit(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if query(req, &["limit"], 24).is_err() {
        failed(res, Error::Invalid);
        return;
    }
    let limit = match req.query::<String>("limit") {
        None => 200,
        // The retained `Number(req.query.limit) || 200` word: a zero value
        // falls back to the default, never a failure state TS did not have;
        // the store clamps the upper bound to AUDIT_LIMIT (2000).
        Some(v) if v.bytes().all(|c| c.is_ascii_digit()) => match v.parse::<usize>() {
            Ok(value) => {
                if value == 0 {
                    200
                } else {
                    value
                }
            }
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
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.decisions_audit(limit).await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    match result {
        Ok(rows) => res.render(Json(serde_json::json!({"audit": rows}))),
        Err(hagency_store::Error::Invalid(_)) => failed(res, Error::Invalid),
        Err(error) => verdict_store_error(res, error),
    }
}
