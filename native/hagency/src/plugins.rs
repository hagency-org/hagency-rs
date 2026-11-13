use crate::{App, refusal, resources};
use hagency_plugin::PluginToolApproval;
use hagency_plugin::{
    ApprovalDecision, PluginError, PluginService, PluginSource, RuntimeQualification,
};
use hagency_store::PluginRuntimeQualification;
use salvo::prelude::*;
use serde::Deserialize;
use serde_json::Value;

pub(crate) fn router() -> Router {
    Router::with_path("plugins")
        .get(list)
        .push(Router::with_path("preview").post(preview))
        .push(Router::with_path("install").post(install))
        .push(Router::with_path("update").post(update))
        .push(Router::with_path("link").post(link))
        .push(Router::with_path("{id}/manifest").get(manifest))
        .push(
            Router::with_path("{id}/settings")
                .get(settings)
                .put(update_settings),
        )
        .push(Router::with_path("{id}/qualifications").get(qualifications))
        .push(Router::with_path("invocations/{invocation_id}").get(invocation))
        .push(Router::with_path("{id}").get(inspect))
        .push(Router::with_path("{id}/enable").post(enable))
        .push(Router::with_path("{id}/disable").post(disable))
        .push(Router::with_path("{id}/uninstall").post(uninstall))
        .push(Router::with_path("{id}/tools").get(tools))
        .push(Router::with_path("{id}/tool/prepare").post(prepare_tool))
        .push(Router::with_path("{id}/tool/invoke").post(invoke_tool))
        .push(Router::with_path("{id}/qualify").post(qualify))
        .push(Router::with_path("{id}/action").post(action))
        .push(Router::with_path("{id}/logs").get(logs))
        .push(Router::with_path("{id}/config-dir").get(config_dir))
}

fn manager(depot: &Depot, res: &mut Response) -> Option<PluginService> {
    let Some(app) = depot.get_typed::<App>().ok() else {
        refusal(res, StatusCode::SERVICE_UNAVAILABLE, "plugin_unavailable");
        return None;
    };
    let Some(manager) = app.plugins() else {
        refusal(res, StatusCode::SERVICE_UNAVAILABLE, "plugin_unavailable");
        return None;
    };
    Some(manager)
}

fn id(req: &Request, res: &mut Response) -> Option<String> {
    let Some(id) = req.param::<String>("id") else {
        refusal(res, StatusCode::BAD_REQUEST, "plugin_id_required");
        return None;
    };
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        refusal(res, StatusCode::BAD_REQUEST, "plugin_id_invalid");
        return None;
    }
    Some(id)
}

fn failure(res: &mut Response, error: PluginError) {
    let (status, code) = match error {
        PluginError::NotFound(_) => (StatusCode::NOT_FOUND, "plugin_not_found"),
        PluginError::Stale(_) => (StatusCode::CONFLICT, "plugin_stale"),
        PluginError::RestartRequired(_) => (StatusCode::CONFLICT, "plugin_restart_required"),
        PluginError::Disabled(_) => (StatusCode::CONFLICT, "plugin_disabled"),
        PluginError::Incompatible => (StatusCode::CONFLICT, "plugin_incompatible"),
        PluginError::Untrusted => (StatusCode::FORBIDDEN, "plugin_untrusted"),
        PluginError::Busy => (StatusCode::CONFLICT, "plugin_busy"),
        PluginError::OutcomeUnknown => (StatusCode::GATEWAY_TIMEOUT, "plugin_outcome_unknown"),
        PluginError::Capacity => (StatusCode::PAYLOAD_TOO_LARGE, "plugin_capacity"),
        PluginError::BuildFailed(_) => (StatusCode::UNPROCESSABLE_ENTITY, "plugin_build_failed"),
        PluginError::Manifest(_) => (StatusCode::BAD_REQUEST, "plugin_invalid"),
        PluginError::Source(error) => {
            let code = if matches!(error, hagency_plugin::SourceError::GitUnavailable) {
                "plugin_git_unavailable"
            } else {
                "plugin_invalid"
            };
            (StatusCode::BAD_REQUEST, code)
        }
        PluginError::Registry(_)
        | PluginError::Command(_)
        | PluginError::Io(_)
        | PluginError::Json(_)
        | PluginError::NotReady(_) => (StatusCode::SERVICE_UNAVAILABLE, "plugin_unavailable"),
        PluginError::ConfigurationRequired(_) => {
            (StatusCode::CONFLICT, "plugin_configuration_required")
        }
        PluginError::SettingsInvalid(_) => (StatusCode::BAD_REQUEST, "plugin_settings_invalid"),
        PluginError::SettingsConflict(_) => (StatusCode::CONFLICT, "plugin_settings_conflict"),
        PluginError::Settings(_) => (StatusCode::SERVICE_UNAVAILABLE, "plugin_unavailable"),
    };
    refusal(res, status, code);
}

fn publish_lifecycle(manager: &PluginService, id: &str, enabled: bool) {
    let kind = if enabled {
        "plugin.enabled"
    } else {
        "plugin.disabled"
    };
    if let Ok(event) = hagency_plugin::RedactedEvent::new(
        kind,
        id,
        if enabled { "enabled" } else { "disabled" },
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    ) {
        let _ = manager.publish_event(event);
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewInput {
    source: PluginSource,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstallInput {
    source: PluginSource,
    trust_digest: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LinkInput {
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActionInput {
    action_id: String,
    #[serde(default)]
    input: Value,
    #[serde(default)]
    idempotency_key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ToolPrepareInput {
    framework: String,
    tool_id: String,
    #[serde(default)]
    input: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ToolInvokeInput {
    approval: PluginToolApproval,
    #[serde(default)]
    input: Value,
    decision: String,
    /// The call envelope's identity; a replay under it never launches twice.
    call_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettingsInput {
    expected_revision: u64,
    values: std::collections::BTreeMap<String, Value>,
}

#[handler]
async fn list(depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    match manager.list() {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}

#[handler]
async fn inspect(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(id) = id(req, res) else { return };
    match manager.inspect(&id) {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}

#[handler]
async fn manifest(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(id) = id(req, res) else { return };
    match manager.manifest(&id) {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}
#[handler]
async fn settings(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(id) = id(req, res) else { return };
    match manager.settings(&id) {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}

#[handler]
async fn update_settings(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(id) = id(req, res) else { return };
    let Some(input) = resources::body::<SettingsInput>(req, depot, res).await else {
        return;
    };
    match manager.update_settings(&id, input.expected_revision, input.values) {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}

#[handler]
async fn tools(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(id) = id(req, res) else { return };
    match manager.tool_catalog(&id) {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}

#[handler]
async fn prepare_tool(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(id) = id(req, res) else { return };
    let Some(input) = resources::body::<ToolPrepareInput>(req, depot, res).await else {
        return;
    };
    match manager.prepare_tool_approval(&input.framework, &id, &input.tool_id, &input.input) {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}

#[handler]
async fn invoke_tool(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(id) = id(req, res) else { return };
    let Some(input) = resources::body::<ToolInvokeInput>(req, depot, res).await else {
        return;
    };
    if input.approval.plugin_id != id {
        refusal(res, StatusCode::FORBIDDEN, "plugin_untrusted");
        return;
    }
    let decision = match input.decision.as_str() {
        "approve_once" => ApprovalDecision::ApproveOnce,
        "deny" => ApprovalDecision::Deny,
        _ => {
            refusal(res, StatusCode::BAD_REQUEST, "plugin_tool_decision_invalid");
            return;
        }
    };
    match manager.invoke_approved_tool(&input.approval, &input.input, decision, &input.call_id) {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}

#[handler]
async fn preview(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(input) = resources::body::<PreviewInput>(req, depot, res).await else {
        return;
    };
    match manager.preview_install(input.source) {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}

#[handler]
async fn qualifications(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(store) = resources::domain(depot, res) else {
        return;
    };
    let Some(id) = id(req, res) else { return };
    if let Err(error) = manager.inspect(&id) {
        failure(res, error);
        return;
    }
    match store.plugin_runtime_choices(id).await {
        Ok(value) => res.render(Json(value)),
        Err(error) => resources::failure(res, error),
    }
}

#[handler]
async fn install(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    apply_install(req, depot, res).await;
}

#[handler]
async fn update(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    apply_install(req, depot, res).await;
}

async fn apply_install(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(store) = resources::domain(depot, res) else {
        return;
    };
    let Some(input) = resources::body::<InstallInput>(req, depot, res).await else {
        return;
    };
    match manager.apply_install(input.source, &input.trust_digest) {
        Ok(value) => match store
            .revoke_plugin_runtime_qualifications(value.id.clone())
            .await
        {
            Ok(()) => res.render(Json(value)),
            Err(error) => {
                let _ = manager.mark_stale(&value.id);
                resources::failure(res, error);
            }
        },
        Err(error) => failure(res, error),
    }
}

#[handler]
async fn link(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(store) = resources::domain(depot, res) else {
        return;
    };
    let Some(input) = resources::body::<LinkInput>(req, depot, res).await else {
        return;
    };
    let path = std::path::PathBuf::from(input.path);
    match manager.link(path) {
        Ok(value) => match store
            .revoke_plugin_runtime_qualifications(value.id.clone())
            .await
        {
            Ok(()) => res.render(Json(value)),
            Err(error) => {
                let _ = manager.mark_stale(&value.id);
                resources::failure(res, error);
            }
        },
        Err(error) => failure(res, error),
    }
}

#[handler]
async fn enable(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    change_enabled(req, depot, res, true).await;
}

#[handler]
async fn disable(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    change_enabled(req, depot, res, false).await;
}

async fn change_enabled(req: &mut Request, depot: &mut Depot, res: &mut Response, enabled: bool) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(store) = resources::domain(depot, res) else {
        return;
    };
    let Some(id) = id(req, res) else { return };
    let result = if enabled {
        manager.enable(&id)
    } else {
        manager.disable(&id)
    };
    match result {
        Ok(value) if !enabled => match store.revoke_plugin_runtime_qualifications(id.clone()).await
        {
            Ok(()) => {
                publish_lifecycle(&manager, &id, false);
                res.render(Json(value));
            }
            Err(error) => {
                let _ = manager.mark_stale(&id);
                resources::failure(res, error);
            }
        },
        Ok(value) => {
            publish_lifecycle(&manager, &id, true);
            res.render(Json(value));
        }
        Err(error) => failure(res, error),
    }
}

#[handler]
async fn uninstall(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(store) = resources::domain(depot, res) else {
        return;
    };
    let Some(id) = id(req, res) else { return };
    match store.revoke_plugin_runtime_qualifications(id.clone()).await {
        Err(error) => {
            let _ = manager.mark_stale(&id);
            resources::failure(res, error);
        }
        Ok(()) => match manager.uninstall(&id) {
            Ok(()) => res.render(Json(serde_json::json!({"ok": true}))),
            Err(error) => failure(res, error),
        },
    }
}

#[handler]
async fn qualify(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(store) = resources::domain(depot, res) else {
        return;
    };
    let Some(id) = id(req, res) else { return };
    let Some(mut input) = resources::body::<RuntimeQualification>(req, depot, res).await else {
        return;
    };
    if input.runtime.plugin_id != id {
        refusal(res, StatusCode::BAD_REQUEST, "plugin_id_mismatch");
        return;
    }
    input.evidence_digest.clear();
    let transcript = match serde_json::to_vec(&input) {
        Ok(bytes) => bytes,
        Err(_) => {
            refusal(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "plugin_qualification_failed",
            );
            return;
        }
    };
    input.evidence_digest = hagency_plugin::evidence_digest(&transcript);
    if let Err(error) = manager.qualify_runtime(&input) {
        failure(res, error);
        return;
    }
    let evidence = match serde_json::to_value(&input) {
        Ok(value) => value,
        Err(_) => {
            refusal(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "plugin_qualification_failed",
            );
            return;
        }
    };
    let qualified_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default();
    let record = PluginRuntimeQualification {
        plugin_id: input.runtime.plugin_id.clone(),
        adapter_id: input.runtime.adapter_id.clone(),
        package_sha256: input.runtime.package_sha256.clone(),
        settings_sha256: input.runtime.settings_sha256.clone(),
        framework: input.framework.clone(),
        model: input.model.clone(),
        provider: input.provider.clone(),
        reasoning: input.reasoning.clone(),
        roles: input.roles.clone(),
        evidence_digest: input.evidence_digest.clone(),
        qualified_at,
        evidence,
    };
    match store.put_plugin_runtime_qualification(record).await {
        Ok(()) => res.render(Json(
            serde_json::json!({"qualified": true, "qualification": input}),
        )),
        Err(error) => resources::failure(res, error),
    }
}

#[handler]
async fn action(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(id) = id(req, res) else { return };
    let Some(input) = resources::body::<ActionInput>(req, depot, res).await else {
        return;
    };
    let key = if input.idempotency_key.is_empty() {
        format!(
            "{}:{}",
            input.action_id,
            serde_json::to_string(&input.input).unwrap_or_default()
        )
    } else {
        input.idempotency_key
    };
    match manager.submit_action(&id, &input.action_id, input.input, &key) {
        Ok(value) => {
            res.status_code(StatusCode::ACCEPTED);
            res.render(Json(value));
        }
        Err(error) => failure(res, error),
    }
}

#[handler]
async fn invocation(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(invocation_id) = req.param::<String>("invocation_id") else {
        refusal(res, StatusCode::BAD_REQUEST, "plugin_invocation_required");
        return;
    };
    match manager.invocation(&invocation_id) {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}

#[handler]
async fn logs(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(id) = id(req, res) else { return };
    match manager.logs(&id) {
        Ok(value) => res.render(Json(value)),
        Err(error) => failure(res, error),
    }
}

#[handler]
async fn config_dir(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(manager) = manager(depot, res) else {
        return;
    };
    let Some(id) = id(req, res) else { return };
    match manager.config_dir(&id) {
        Ok(path) => res.render(Json(serde_json::json!({"path": path}))),
        Err(error) => failure(res, error),
    }
}
