//! Operator-only management. Authenticated Matrix admission will have its own
//! adapter; this API does not accept room observation or approval booleans.
use crate::{App, refusal};
use hagency_core::project::{Resource, Seat};
use hagency_store::{DomainStore, Error};
use salvo::prelude::*;
use serde::de::DeserializeOwned;
use std::time::Duration;

pub(crate) fn router() -> Router {
    Router::new()
        .push(
            Router::with_path("resources")
                .get(catalog)
                .post(put_resource),
        )
        .push(Router::with_path("resources/{id}/budget").get(budget))
        .push(Router::with_path("resource-configurations").get(configurations))
        .push(Router::with_path("seats").get(seats).post(put_seat))
        .push(Router::with_path("seats/{seatId}").delete(delete_seat))
        .push(Router::with_path("engagements").get(engagements))
        .push(Router::with_path("roles").get(role_publications))
        .push(Router::with_path("roles/{role}/publication").post(publish_role))
        // Task #19 TS parity: GET/PUT /api/offers, the whitelist CRUD, and
        // DELETE /api/framework-presets/:id + agent definitions. Success JSON
        // keeps the TS shapes (backend-v2.js:15330-15385, 15823-15833, 15960).
        .push(Router::with_path("offers").get(offers))
        .push(Router::with_path("offers/{role}").put(put_offer))
        .push(
            Router::with_path("whitelist")
                .get(whitelist)
                .post(post_whitelist),
        )
        .push(Router::with_path("whitelist/{roomId}").delete(remove_whitelist))
        .push(Router::with_path("framework-presets/{id}").delete(delete_resource))
        .push(
            Router::with_path("framework-presets/{id}/agents")
                .get(definitions)
                .post(edit_definition),
        )
        .push(
            Router::with_path("framework-presets/{id}/agents/{definitionId}")
                .put(edit_definition)
                .delete(edit_definition),
        )
}
pub(super) fn domain(depot: &Depot, res: &mut Response) -> Option<DomainStore> {
    let store = depot.get_typed::<App>().ok().and_then(|a| a.domain.clone());
    if store.is_none() {
        refusal(res, StatusCode::SERVICE_UNAVAILABLE, "domain_unavailable");
    }
    store
}
pub(super) fn failure(res: &mut Response, error: Error) {
    let (status, code) = match error {
        Error::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_domain_command"),
        Error::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        Error::Conflict => (StatusCode::CONFLICT, "idempotency_conflict"),
        Error::State => (StatusCode::CONFLICT, "state_conflict"),
        Error::Busy => (StatusCode::SERVICE_UNAVAILABLE, "busy"),
        Error::OutcomeUnknown => (StatusCode::GATEWAY_TIMEOUT, "outcome_unknown"),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "domain_unavailable"),
    };
    refusal(res, status, code);
}
pub(super) async fn body<T: DeserializeOwned>(
    req: &mut Request,
    depot: &Depot,
    res: &mut Response,
) -> Option<T> {
    let app = depot.get_typed::<App>().ok()?;
    let Ok(_permit) = app.requests.clone().try_acquire_owned() else {
        refusal(res, StatusCode::SERVICE_UNAVAILABLE, "busy");
        return None;
    };
    if req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(str::trim)
        != Some("application/json")
    {
        refusal(res, StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_required");
        return None;
    }
    let bytes =
        match tokio::time::timeout(Duration::from_secs(2), req.payload_with_max_size(64 * 1024))
            .await
        {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(_)) => {
                refusal(res, StatusCode::PAYLOAD_TOO_LARGE, "body_rejected");
                return None;
            }
            Err(_) => {
                refusal(res, StatusCode::REQUEST_TIMEOUT, "body_timeout");
                return None;
            }
        };
    match serde_json::from_slice(bytes) {
        Ok(value) => Some(value),
        Err(_) => {
            refusal(res, StatusCode::BAD_REQUEST, "invalid_domain_command");
            None
        }
    }
}
#[handler]
async fn put_resource(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(store) = domain(depot, res) else {
        return;
    };
    let Some(input) = body::<serde_json::Value>(req, depot, res).await else {
        return;
    };
    if input.get("roles").is_some() {
        refusal(res, StatusCode::BAD_REQUEST, "roles_are_model_derived");
        return;
    }
    let publication = input.get("published").and_then(|v| v.as_bool());
    let value = match serde_json::from_value::<Resource>(input) {
        Ok(value) => value,
        Err(_) => {
            refusal(res, StatusCode::BAD_REQUEST, "invalid_domain_command");
            return;
        }
    };
    match store.edit_resource(value, publication).await {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}
#[handler]
async fn put_seat(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(store) = domain(depot, res) else {
        return;
    };
    let Some(value) = body::<Seat>(req, depot, res).await else {
        return;
    };
    match store.put_seat(value).await {
        Ok(()) => res.render(Json(serde_json::json!({"saved":true}))),
        Err(error) => failure(res, error),
    }
}
fn page(req: &Request) -> Result<(String, usize), Error> {
    let after = req.query::<String>("after").unwrap_or_default();
    if after.len() > 256 {
        return Err(hagency_core::InvalidInput("invalid page cursor").into());
    }
    let limit = match req.query::<String>("limit") {
        Some(n) => n
            .parse()
            .map_err(|_| hagency_core::InvalidInput("invalid page limit"))?,
        None => 100,
    };
    Ok((after, limit))
}
#[handler]
async fn catalog(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(store) = domain(depot, res) else {
        return;
    };
    let (after, limit) = match page(req) {
        Ok(page) => page,
        Err(error) => {
            failure(res, error);
            return;
        }
    };
    match store.catalog(after, limit).await {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}
#[handler]
async fn configurations(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(store) = domain(depot, res) else {
        return;
    };
    let (after, limit) = match page(req) {
        Ok(page) => page,
        Err(error) => {
            failure(res, error);
            return;
        }
    };
    match store.resource_configurations(after, limit).await {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}
#[handler]
async fn role_publications(depot: &mut Depot, res: &mut Response) {
    let Some(store) = domain(depot, res) else {
        return;
    };
    match store.role_publications().await {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}
#[handler]
async fn publish_role(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Publication {
        published: bool,
    }
    let Some(store) = domain(depot, res) else {
        return;
    };
    let Some(role) = req.param::<String>("role") else {
        refusal(res, StatusCode::BAD_REQUEST, "role_required");
        return;
    };
    let Some(input) = body::<Publication>(req, depot, res).await else {
        return;
    };
    match store.set_role_publication(role, input.published).await {
        Ok(()) => res.render(Json(serde_json::json!({"saved":true}))),
        Err(error) => failure(res, error),
    }
}
#[handler]
async fn seats(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(store) = domain(depot, res) else {
        return;
    };
    let (after, limit) = match page(req) {
        Ok(page) => page,
        Err(error) => {
            failure(res, error);
            return;
        }
    };
    match store.seats(after, limit).await {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}
#[handler]
async fn engagements(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(store) = domain(depot, res) else {
        return;
    };
    let (after, limit) = match page(req) {
        Ok(page) => page,
        Err(error) => {
            failure(res, error);
            return;
        }
    };
    match store.engagements(after, limit).await {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}
#[handler]
async fn budget(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(store) = domain(depot, res) else {
        return;
    };
    let Some(id) = req.param::<String>("id") else {
        refusal(res, StatusCode::BAD_REQUEST, "resource_required");
        return;
    };
    match store.resource_budget(id).await {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}

/// TS GET /api/offers (backend-v2.js:15330-15345): every role, offered or not,
/// `catalogPublished` derived. The store shapes each row; this route wraps it
/// the way TS wraps `engagementStore.listOffers()`.
#[handler]
async fn offers(depot: &mut Depot, res: &mut Response) {
    let Some(store) = domain(depot, res) else {
        return;
    };
    match store.offers().await {
        Ok(rows) => res.render(Json(serde_json::json!({"offers":rows}))),
        Err(error) => failure(res, error),
    }
}

/// TS PUT /api/offers/:role (backend-v2.js:15347-15361).
#[handler]
async fn put_offer(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    struct Input {
        count: Option<f64>,
        budget_cap_per_engagement: Option<f64>,
        rate_cap: Option<f64>,
        published: Option<bool>,
    }
    let Some(store) = domain(depot, res) else {
        return;
    };
    let Some(role) = req.param::<String>("role") else {
        refusal(res, StatusCode::BAD_REQUEST, "role_required");
        return;
    };
    let Some(input) = body::<Input>(req, depot, res).await else {
        return;
    };
    // TS: `published === true` — anything but an explicit boolean true means
    // false, and an unknown role is a 400, not a 404.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0);
    // TS posInt (engagement-store.js:148-170): FLOOR FIRST, then validate.
    // 3.5 is accepted and stored as 3; 0/-1/>2^53 are refused. The i64 cast
    // here is exact for every finite f64 below 2^53 after flooring.
    let floor = |v: f64| -> Result<i64, Error> {
        if !v.is_finite() || v > 9_007_199_254_740_992.0 {
            return Err(Error::Invalid(hagency_core::InvalidInput(
                "cap must be a positive integer below 2^53",
            )));
        }
        let floored = v.floor() as i64;
        if floored <= 0 {
            return Err(Error::Invalid(hagency_core::InvalidInput(
                "cap must be a positive integer below 2^53",
            )));
        }
        Ok(floored)
    };
    let (count, cap_budget, cap_rate) =
        match (input.count, input.budget_cap_per_engagement, input.rate_cap) {
            (Some(c), _, _)
                if !(c.is_finite() && c.floor() > 0.0 && c <= 9_007_199_254_740_992.0) =>
            {
                failure(
                    res,
                    Error::Invalid(hagency_core::InvalidInput(
                        "cap must be a positive integer below 2^53",
                    )),
                );
                return;
            }
            (_, Some(b), _)
                if !(b.is_finite() && b.floor() > 0.0 && b <= 9_007_199_254_740_992.0) =>
            {
                failure(
                    res,
                    Error::Invalid(hagency_core::InvalidInput(
                        "cap must be a positive integer below 2^53",
                    )),
                );
                return;
            }
            (_, _, Some(r))
                if !(r.is_finite() && r.floor() > 0.0 && r <= 9_007_199_254_740_992.0) =>
            {
                failure(
                    res,
                    Error::Invalid(hagency_core::InvalidInput(
                        "cap must be a positive integer below 2^53",
                    )),
                );
                return;
            }
            _ => (
                input.count.map(|v| floor(v).expect("validated above")),
                input
                    .budget_cap_per_engagement
                    .map(|v| floor(v).expect("validated above")),
                input.rate_cap.map(|v| floor(v).expect("validated above")),
            ),
        };
    match store
        .set_offer(
            role,
            count,
            cap_budget,
            cap_rate,
            input.published == Some(true),
            now,
        )
        .await
    {
        Ok(offer) => res.render(Json(serde_json::json!({"ok":true,"offer":offer}))),
        Err(error) => failure(res, error),
    }
}

/// TS GET /api/whitelist (backend-v2.js:15363-15366).
#[handler]
async fn whitelist(depot: &mut Depot, res: &mut Response) {
    let Some(store) = domain(depot, res) else {
        return;
    };
    match store.whitelist().await {
        Ok(rows) => res.render(Json(serde_json::json!({"whitelist":rows}))),
        Err(error) => failure(res, error),
    }
}

/// TS POST /api/whitelist (backend-v2.js:15367-15375).
#[handler]
async fn post_whitelist(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    struct Input {
        project_room_id: String,
        display_name: Option<String>,
        added_by: Option<String>,
    }
    let Some(store) = domain(depot, res) else {
        return;
    };
    let Some(input) = body::<Input>(req, depot, res).await else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0);
    match store
        .add_whitelist(
            input.project_room_id,
            input.display_name,
            input.added_by,
            now,
        )
        .await
    {
        Ok(entry) => res.render(Json(serde_json::json!({"ok":true,"entry":entry}))),
        Err(error) => failure(res, error),
    }
}

/// TS DELETE /api/whitelist/:roomId (backend-v2.js:15377-15388): removal is
/// future-only and reports `stillActive`.
#[handler]
async fn remove_whitelist(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(store) = domain(depot, res) else {
        return;
    };
    let Some(room) = req.param::<String>("roomId") else {
        refusal(res, StatusCode::BAD_REQUEST, "room_required");
        return;
    };
    match store.remove_whitelist(room).await {
        Ok((project_room_id, still_active)) => res.render(Json(
            serde_json::json!({"ok":true,"projectRoomId":project_room_id,"stillActive":still_active}),
        )),
        Err(error) => failure(res, error),
    }
}

/// TS DELETE /api/framework-presets/:id (backend-v2.js:15960-15971).
#[handler]
async fn delete_resource(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(store) = domain(depot, res) else {
        return;
    };
    let Some(id) = req.param::<String>("id") else {
        refusal(res, StatusCode::BAD_REQUEST, "resource_required");
        return;
    };
    match store.delete_resource(id).await {
        Ok(removed) => res.render(Json(serde_json::json!({"ok":true,"preset":removed}))),
        Err(error) => failure(res, error),
    }
}

/// TS agent definitions list shape is served through the resource GET; the
/// edit routes all share one handler (backend-v2.js:15823-15833).
#[handler]
async fn definitions(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(store) = domain(depot, res) else {
        return;
    };
    let Some(id) = req.param::<String>("id") else {
        refusal(res, StatusCode::BAD_REQUEST, "resource_required");
        return;
    };
    match store.agent_definitions(id).await {
        Ok(rows) => res.render(Json(serde_json::json!({"agentDefinitions":rows}))),
        Err(error) => failure(res, error),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentDefinitionWrite {
    name: Option<String>,
    role: Option<String>,
    enabled: Option<bool>,
    #[serde(default)]
    harness_template_values: Option<std::collections::BTreeMap<String, serde_json::Value>>,
    #[serde(default)]
    harness_id: Option<String>,
}

fn plugin_failure(res: &mut Response, error: hagency_plugin::PluginError) {
    let (status, code) = match error {
        hagency_plugin::PluginError::ConfigurationRequired(_) => {
            (StatusCode::CONFLICT, "plugin_configuration_required")
        }
        hagency_plugin::PluginError::Untrusted => (StatusCode::FORBIDDEN, "plugin_untrusted"),
        hagency_plugin::PluginError::SettingsInvalid(_)
        | hagency_plugin::PluginError::Settings(_) => {
            (StatusCode::BAD_REQUEST, "plugin_settings_invalid")
        }
        hagency_plugin::PluginError::Disabled(_) => (StatusCode::CONFLICT, "plugin_disabled"),
        hagency_plugin::PluginError::Stale(_) => (StatusCode::CONFLICT, "plugin_stale"),
        hagency_plugin::PluginError::RestartRequired(_) => {
            (StatusCode::CONFLICT, "plugin_restart_required")
        }
        hagency_plugin::PluginError::NotFound(_) => (StatusCode::NOT_FOUND, "plugin_not_found"),
        hagency_plugin::PluginError::Incompatible => (StatusCode::CONFLICT, "plugin_incompatible"),
        hagency_plugin::PluginError::Capacity => (StatusCode::PAYLOAD_TOO_LARGE, "plugin_capacity"),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "plugin_unavailable"),
    };
    refusal(res, status, code);
}

/// TS mutateResourceAgentDefinition (backend-v2.js:15822-15833): POST creates,
/// PUT updates, DELETE removes — one handler for all three.
#[handler]
async fn edit_definition(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(store) = domain(depot, res) else {
        return;
    };
    let Some(id) = req.param::<String>("id") else {
        refusal(res, StatusCode::BAD_REQUEST, "resource_required");
        return;
    };
    let definition_id = req.param::<String>("definitionId");
    let input = if req.method() == salvo::http::Method::DELETE {
        None
    } else {
        let Some(value) = body::<serde_json::Value>(req, depot, res).await else {
            return;
        };
        let write = match serde_json::from_value::<AgentDefinitionWrite>(value) {
            Ok(write) => write,
            Err(_) => {
                refusal(res, StatusCode::BAD_REQUEST, "invalid_domain_command");
                return;
            }
        };
        let resource = match store.resource_configuration(id.to_owned()).await {
            Ok(resource) => resource,
            Err(error) => {
                failure(res, error);
                return;
            }
        };
        let template = match resource.harness_plugin.as_ref() {
            Some(identity)
                if definition_id.is_none()
                    || write.harness_id.is_some()
                    || write.harness_template_values.is_some() =>
            {
                let Some(manager) = depot.get_typed::<App>().ok().and_then(|app| app.plugins())
                else {
                    refusal(res, StatusCode::SERVICE_UNAVAILABLE, "plugin_unavailable");
                    return;
                };
                let harness = hagency_plugin::HarnessPluginRef {
                    plugin_id: identity.plugin_id.clone(),
                    harness_id: write
                        .harness_id
                        .clone()
                        .unwrap_or_else(|| identity.harness_id.clone()),
                    package_sha256: identity.package_sha256.clone(),
                    settings_sha256: identity.settings_sha256.clone(),
                };
                let values = write.harness_template_values.clone().unwrap_or_default();
                match manager.resolve_harness_template(&harness, &values) {
                    Ok(template) => Some(template),
                    Err(error) => {
                        plugin_failure(res, error);
                        return;
                    }
                }
            }
            Some(_) => None,
            None => {
                if write.harness_id.is_some() || write.harness_template_values.is_some() {
                    refusal(res, StatusCode::CONFLICT, "plugin_configuration_required");
                    return;
                }
                None
            }
        };
        Some(serde_json::json!({
            "name": write.name,
            "role": write.role,
            "enabled": write.enabled,
            "harnessTemplate": template,
        }))
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0);
    match store
        .edit_agent_definition(id, definition_id, input, now)
        .await
    {
        Ok(definition) => res.render(Json(serde_json::json!({"ok":true,"definition":definition}))),
        Err(error) => failure(res, error),
    }
}

/// TS DELETE /api/seats/:seatId (backend-v2.js:15802-15810): 404 when no
/// declaration exists, `{ok:true, seatId}` on success.
#[handler]
async fn delete_seat(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(store) = domain(depot, res) else {
        return;
    };
    let seat_id = req
        .param::<String>("seatId")
        .or_else(|| req.param::<String>("id"));
    let Some(seat_id) = seat_id else {
        refusal(res, StatusCode::BAD_REQUEST, "seat_required");
        return;
    };
    match store.delete_seat(seat_id.clone()).await {
        Ok(()) => res.render(Json(serde_json::json!({"ok":true,"seatId":seat_id}))),
        Err(error) => failure(res, error),
    }
}
