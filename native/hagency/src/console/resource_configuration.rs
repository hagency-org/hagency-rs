//! Closed browser configuration data; private account association stays in the writer.
use super::{
    Error, Session, body, console, failed, recheck,
    resources::{ResourceRow, bounded, failure, resource_id},
    usage::query,
};
use crate::resources::domain;
use hagency_core::{
    project::{Resource, identifier},
    qualification::{self, ConfigurationChoice, Tier},
};
use hagency_store::{CeilingChange, ProfileChange, ResourceEngagementChange};
use salvo::prelude::*;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

pub(super) fn router() -> Router {
    Router::new().push(
        Router::with_path("resources/{id}/configuration")
            .get(observation)
            .patch(edit),
    )
}
pub(super) fn selection_query(req: &Request) -> Result<(), Error> {
    query(req, &["resource_id", "source_resource_id"], 192)?;
    let resource = req.query::<String>("resource_id");
    let source = req.query::<String>("source_resource_id");
    if resource.is_some() && source.is_some() {
        return Err(Error::Invalid);
    }
    if let Some(id) = resource.or(source) {
        identifier(&id, 128).map_err(|_| Error::Invalid)?;
    }
    Ok(())
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    resource: ResourceRow,
    choices: Vec<ConfigurationChoice>,
    model_tier: Option<Tier>,
    model_roles: Vec<String>,
}
impl Observation {
    fn from(resource: Resource) -> Result<Self, hagency_store::Error> {
        let profile = resource.profile();
        Ok(Self {
            choices: qualification::configuration_choices(&profile)?,
            model_tier: qualification::model(&profile).0,
            model_roles: qualification::roles()
                .filter(|r| qualification::qualifies(&profile, r, None))
                .map(str::to_owned)
                .collect(),
            resource: ResourceRow::from_resource(resource)?,
        })
    }
}
#[handler]
async fn observation(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let id = match resource_id(req).and_then(|id| query(req, &[], 0).map(|_| id)) {
        Ok(id) => id,
        Err(error) => {
            failed(res, error);
            return;
        }
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = async {
        let mut value = Observation::from(store.resource_configuration(id.clone()).await?)?;
        value.resource.engagement_resources = store.resource_engagements(id).await?;
        Ok::<_, hagency_store::Error>(value)
    }
    .await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    match result {
        Ok(value) => bounded(res, &value),
        Err(error) => super::server_engagements::failure(res, error),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Create {
    source_resource_id: String,
    expected_revision: String,
    profile_change: ProfileChange,
    ceiling_change: CeilingChange,
    engagement: Option<ResourceEngagementChange>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Edit {
    expected_revision: String,
    profile_change: ProfileChange,
    ceiling_change: CeilingChange,
    engagement: Option<ResourceEngagementChange>,
}
pub(super) struct PreparedInput {
    pub resource: String,
    pub revision: String,
    pub create: bool,
    pub profile: ProfileChange,
    pub ceiling: CeilingChange,
    pub engagement: Option<ResourceEngagementChange>,
}
#[handler]
pub(super) async fn create(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    write(req, depot, res, true).await;
}
#[handler]
async fn edit(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    write(req, depot, res, false).await;
}
async fn write(req: &mut Request, depot: &mut Depot, res: &mut Response, creating: bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    let prepared = async {
        query(req, &[], 0)?;
        if req.headers().get_all("content-type").iter().count() != 1
            || req
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(';').next())
                .map(str::trim)
                != Some("application/json")
        {
            return Err(Error::Invalid);
        }
        let bytes = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            body(req, 24 * 1024),
        )
        .await
        .map_err(|_| Error::Unavailable)??;
        let input = if creating {
            let input: Create = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
            PreparedInput {
                resource: input.source_resource_id,
                revision: input.expected_revision,
                create: true,
                profile: input.profile_change,
                ceiling: input.ceiling_change,
                engagement: input.engagement,
            }
        } else {
            let input: Edit = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
            PreparedInput {
                resource: resource_id(req)?,
                revision: input.expected_revision,
                create: false,
                profile: input.profile_change,
                ceiling: input.ceiling_change,
                engagement: input.engagement,
            }
        };
        let session = depot
            .get_typed::<Session>()
            .map_err(|_| Error::Unauthorized)?;
        console(depot)?
            .0
            .authority
            .configuration(session, input, deadline)
    }
    .await;
    let command = match prepared {
        Ok(c) => c,
        Err(error) => {
            failed(res, error);
            return;
        }
    };
    let Some(store) = domain(depot, res) else {
        return;
    };
    let result = store.configure_resource(command).await;
    if result.is_ok() && recheck(depot).is_err() {
        failure(res, hagency_store::Error::OutcomeUnknown);
        return;
    }
    match result {
        Ok(value) => bounded(res, &value),
        Err(error) => super::server_engagements::failure(res, error),
    }
}
