//! Resource-owner setup. These routes reserve owned capacity; project and agent
//! human decisions remain in the authenticated Palpo/Rinx coordinator workflow.
use super::{Error, Session, body, console, failed, recheck, resources::bounded, usage::query};
use crate::resources::domain;
use hagency_store::coordinator::ResourceGrant;
use salvo::prelude::*;
use serde_json::json;

pub(super) fn router() -> Router {
    Router::with_path("server-engagements")
        .get(list)
        .push(Router::with_path("{id}/decisions").get(decisions))
        .push(
            Router::with_path("{id}/resources")
                .get(resources)
                .put(contribute),
        )
}
#[handler]
async fn decisions(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let (after, limit) = match page(req) {
        Ok(v) => v,
        Err(e) => {
            failed(res, e);
            return;
        }
    };
    let id = req.param::<String>("id").unwrap_or_default();
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.coordinator_deliveries(id, after, limit).await;
    if let Err(e) = recheck(depot) {
        failed(res, e);
        return;
    }
    match result {
        Ok(rows) => bounded(
            res,
            &json!({"nextCursor":rows.last().map(|r|&r["id"]),"decisions":rows}),
        ),
        Err(e) => failure(res, e),
    }
}
fn page(req: &Request) -> Result<(String, usize), Error> {
    query(req, &["after", "limit"], 300)?;
    let after = req.query::<String>("after").unwrap_or_default();
    if !after.is_empty() {
        hagency_core::project::identifier(&after, 128).map_err(|_| Error::Invalid)?;
    }
    let limit = match req.query::<String>("limit") {
        None => 25,
        Some(s) if s.bytes().all(|b| b.is_ascii_digit()) => {
            s.parse().map_err(|_| Error::Invalid)?
        }
        Some(_) => return Err(Error::Invalid),
    };
    if limit == 0 || limit > 50 {
        return Err(Error::Invalid);
    }
    Ok((after, limit))
}
fn failure(res: &mut Response, error: hagency_store::Error) {
    match error {
        hagency_store::Error::InsufficientCapacity | hagency_store::Error::OverCommit { .. } => {
            crate::refusal(res, StatusCode::CONFLICT, "contribution_capacity_exceeded")
        }
        hagency_store::Error::Generation => {
            crate::refusal(res, StatusCode::CONFLICT, "engagement_authority_changed")
        }
        hagency_store::Error::NoCeiling => {
            crate::refusal(res, StatusCode::CONFLICT, "resource_ceiling_required")
        }
        hagency_store::Error::Capacity => {
            crate::refusal(res, StatusCode::BAD_REQUEST, "contribution_limit_exceeded")
        }
        e => super::resources::failure(res, e),
    }
}
#[handler]
async fn list(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let (after, limit) = match page(req) {
        Ok(v) => v,
        Err(e) => {
            failed(res, e);
            return;
        }
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.server_engagements(after, limit).await;
    if let Err(e) = recheck(depot) {
        failed(res, e);
        return;
    }
    match result {
        Ok(rows) => bounded(
            res,
            &json!({"nextCursor":rows.last().map(|r|&r["id"]),"engagements":rows}),
        ),
        Err(e) => failure(res, e),
    }
}
#[handler]
async fn resources(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let (after, limit) = match page(req) {
        Ok(v) => v,
        Err(e) => {
            failed(res, e);
            return;
        }
    };
    let id = req.param::<String>("id").unwrap_or_default();
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.server_engagement_resources(id, after, limit).await;
    if let Err(e) = recheck(depot) {
        failed(res, e);
        return;
    }
    match result {
        Ok(rows) => bounded(
            res,
            &json!({"nextCursor":rows.last().map(|r|&r["id"]),"resources":rows}),
        ),
        Err(e) => failure(res, e),
    }
}
#[handler]
async fn contribute(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let prepared = async {
        query(req, &[], 0)?;
        let session = depot
            .get_typed::<Session>()
            .map_err(|_| Error::Unauthorized)?;
        let access = console(depot)?;
        if !access.0.authority.can_configure(session)? {
            return Err(Error::ConfigurationForbidden);
        }
        let raw = body(req, 64 * 1024).await?;
        let grant: ResourceGrant = serde_json::from_slice(&raw).map_err(|_| Error::Invalid)?;
        if req.param::<String>("id").as_deref() != Some(grant.server_engagement_id.as_str()) {
            return Err(Error::Invalid);
        }
        access.0.authority.contribution(session, grant)
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
    let result = store.contribute_resource(command).await;
    if let Err(e) = recheck(depot) {
        failed(res, e);
        return;
    }
    match result {
        Ok(()) => res.render(Json(json!({"ok":true}))),
        Err(e) => failure(res, e),
    }
}
