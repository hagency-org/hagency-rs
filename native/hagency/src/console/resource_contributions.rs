//! Finite resource delegation starts in the authenticated Hagency console.
use super::{
    Error, Session, body, console, failed, recheck,
    resources::{bounded, resource_id},
    usage::query,
};
use crate::{refusal, resources::domain};
use hagency_core::{
    project::identifier,
    project_grants::{GrantLimits, ResourceDelegation},
};
use hagency_store::ContributionMutation;
use salvo::prelude::*;
use serde::Deserialize;
use serde_json::json;
use std::time::{Duration, Instant};

pub(super) fn router() -> Router {
    Router::new()
        .push(Router::with_path("palpo/contribution-targets").get(targets))
        .push(
            Router::with_path("resources/{id}/contributions")
                .get(list)
                .post(create),
        )
        .push(Router::with_path("resources/{id}/contributions/{contribution}/revoke").post(revoke))
}
#[handler]
async fn targets(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let after = req.query::<String>("after").unwrap_or_default();
    if query(req, &["after"], 160).is_err()
        || (!after.is_empty() && identifier(&after, 128).is_err())
    {
        failed(res, Error::Invalid);
        return;
    }
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.contribution_targets(after, 17).await;
    if let Err(e) = recheck(depot) {
        failed(res, e);
        return;
    }
    match result {
        Ok(mut rows) => {
            let next_after = (rows.len() > 16).then(|| rows[15].fleet_id.clone());
            rows.truncate(16);
            bounded(res, &json!({"targets":rows,"nextAfter":next_after}));
        }
        Err(e) => failure(res, e),
    }
}
fn failure(res: &mut Response, error: hagency_store::Error) {
    use hagency_store::Error as E;
    let (status, code) = match error {
        E::GrantExpired => (StatusCode::CONFLICT, "contribution_expired"),
        E::GrantRevoked => (StatusCode::CONFLICT, "contribution_revoked"),
        E::GrantAuthority | E::Generation => {
            (StatusCode::CONFLICT, "connection_verification_required")
        }
        E::InsufficientCapacity | E::OverCommit { .. } | E::NoCeiling => {
            (StatusCode::CONFLICT, "insufficient_capacity")
        }
        E::Unqualified => (StatusCode::CONFLICT, "resource_unavailable"),
        other => {
            super::resources::failure(res, other);
            return;
        }
    };
    refusal(res, status, code);
}
#[handler]
async fn list(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let input = resource_id(req).and_then(|id| {
        query(req, &["after"], 160)?;
        let after = req.query::<String>("after").unwrap_or_default();
        if !after.is_empty() {
            identifier(&after, 128).map_err(|_| Error::Invalid)?;
        }
        Ok((id, after))
    });
    let (resource, after) = match input {
        Ok(v) => v,
        Err(e) => {
            failed(res, e);
            return;
        }
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store
        .resource_contributions(resource.clone(), after, 17)
        .await;
    if let Err(e) = recheck(depot) {
        failed(res, e);
        return;
    }
    match result {
        Ok(mut rows) => {
            let next_after = (rows.len() > 16).then(|| rows[15].grant.id.clone());
            rows.truncate(16);
            bounded(
                res,
                &json!({"resourceId":resource,"contributions":rows,"nextAfter":next_after}),
            );
        }
        Err(e) => failure(res, e),
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Create {
    request_id: String,
    expected_resource_revision: String,
    fleet_id: String,
    registration_generation: u64,
    limits: GrantLimits,
    expires_at_ms: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Revoke {
    expected_resource_revision: String,
    expected_revision: u64,
}

#[handler]
async fn create(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    write(req, depot, res, false).await;
}
#[handler]
async fn revoke(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    write(req, depot, res, true).await;
}
async fn write(req: &mut Request, depot: &mut Depot, res: &mut Response, revoking: bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    let Some(store) = domain(depot, res) else {
        return;
    };
    let prepared = async {
        query(req, &[], 0)?;
        let resource = resource_id(req)?;
        if req.headers().get_all("content-type").iter().count() != 1
            || req
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.split(';').next())
                .map(str::trim)
                != Some("application/json")
        {
            return Err(Error::Invalid);
        }
        let bytes =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), body(req, 4096))
                .await
                .map_err(|_| Error::Unavailable)??;
        let (revision, mutation) = if revoking {
            let input: Revoke = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
            let id = req.param::<String>("contribution").ok_or(Error::Invalid)?;
            identifier(&id, 128).map_err(|_| Error::Invalid)?;
            (
                input.expected_resource_revision,
                ContributionMutation::Revoke {
                    id,
                    expected_revision: input.expected_revision,
                },
            )
        } else {
            let input: Create = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
            identifier(&input.request_id, 96).map_err(|_| Error::Invalid)?;
            identifier(&input.fleet_id, 128).map_err(|_| Error::Invalid)?;
            input.limits.validate().map_err(|_| Error::Invalid)?;
            let registration = store
                .provisioning_registration(input.fleet_id)
                .await
                .map_err(|_| Error::Invalid)?;
            if registration.generation != input.registration_generation {
                return Err(Error::Invalid);
            }
            let grant = ResourceDelegation {
                v: 1,
                id: format!("contribution_{}", input.request_id),
                revision: 1,
                fleet_id: registration.fleet_id,
                registration_generation: registration.generation,
                issuer: registration.server_name,
                resource_id: resource.clone(),
                limits: input.limits,
                expires_at_ms: input.expires_at_ms,
            };
            (
                input.expected_resource_revision,
                ContributionMutation::Contribute { grant },
            )
        };
        let session = depot
            .get_typed::<Session>()
            .map_err(|_| Error::Unauthorized)?;
        console(depot)?
            .0
            .authority
            .contribution(session, resource, revision, mutation, deadline)
    }
    .await;
    let command = match prepared {
        Ok(v) => v,
        Err(e) => {
            failed(res, e);
            return;
        }
    };
    let result = store.contribute_resource(command).await;
    if result.is_ok() && recheck(depot).is_err() {
        failure(res, hagency_store::Error::OutcomeUnknown);
        return;
    }
    match result {
        Ok(value) => bounded(res, &value),
        Err(e) => failure(res, e),
    }
}
