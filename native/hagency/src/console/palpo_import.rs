//! Import the Palpo owner download from the console (TS parity: the
//! "导入 Palpo 已授权配置" step of `mockup/app/projects/new/page.jsx`, which
//! saves the parsed credential with `PUT /api/project-sides/:id/credential`
//! and lets the bridge's outbound reconcile start the fleet).
//!
//! The operator downloads the configuration in Palpo (My Hagency access →
//! Download Hagency configuration) and picks it here. The route validates it
//! with the CLI importer's own rules, saves the fleet registration, the
//! transport files and the project side's credential, then starts the Palpo
//! transport without a restart. The answer carries the fleet's public facts
//! only: no App Service or machine token ever leaves the state directory.
use super::engagements::check_lifecycle;
use super::{Error, body, failed, recheck};
use crate::bootstrap::palpo::ImportError;
use crate::refusal;
use salvo::prelude::*;
use serde::Deserialize;
use serde_json::json;

pub(super) fn router() -> Router {
    Router::with_path("palpo/import").post(import)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportBody {
    /// The downloaded file's text, exactly as Palpo served it.
    configuration: String,
    /// The Matrix client API of the project's homeserver.
    homeserver: String,
}

#[handler]
async fn import(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    // Connecting a project server is a project-side write: the lifecycle
    // scope, refused before any body parse.
    if !check_lifecycle(depot, res) {
        return;
    }
    let raw = match body(req, 80 * 1024).await {
        Ok(raw) => raw,
        Err(_) => {
            failed(res, Error::Invalid);
            return;
        }
    };
    let input: ImportBody = match serde_json::from_slice(&raw) {
        Ok(input) => input,
        Err(_) => {
            failed(res, Error::Invalid);
            return;
        }
    };
    let Some(live) = depot
        .get_typed::<crate::App>()
        .ok()
        .and_then(|app| app.palpo_live().cloned())
    else {
        refusal(
            res,
            StatusCode::SERVICE_UNAVAILABLE,
            "palpo_import_unavailable",
        );
        return;
    };
    let result = live.import(&input.configuration, &input.homeserver).await;
    if let Err(error) = recheck(depot) {
        failed(res, error);
        return;
    }
    match result {
        Ok(connected) => {
            let imported = connected.imported;
            res.render(Json(json!({
                "ok": true,
                "fleetId": imported.fleet_id,
                "serverName": imported.server_name,
                "representative": imported.representative,
                "approvalBot": imported.approval_bot,
                "endpoint": imported.endpoint,
                "receptionBound": !imported.reception.is_empty(),
                "started": connected.started,
                "transport": live.status().get(),
            })));
        }
        // The importer names the refused field, never its value.
        Err(ImportError::Invalid(field)) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(json!({
                "ok": false, "code": "palpo_import_invalid", "field": field,
                "error": format!("this is not the Hagency configuration downloaded from Palpo ({field})"),
            })));
        }
        Err(ImportError::Store(_) | ImportError::Start(_) | ImportError::Closed) => {
            refusal(
                res,
                StatusCode::SERVICE_UNAVAILABLE,
                "palpo_import_unavailable",
            );
        }
    }
}
