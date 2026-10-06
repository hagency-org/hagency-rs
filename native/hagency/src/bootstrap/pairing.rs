//! Resumable native-to-native association bootstrap. Browser responses contain
//! progress only; pairing and appservice credentials stay in private files.
use super::{
    association::Error,
    association::{create as write_new, identity, origin, read, response},
    palpo::Live,
};
use hagency_store::private;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Input {
    pub request_id: String,
    pub homeserver: String,
    #[serde(default)]
    pub palpo_origin: Option<String>,
    pub owner_mxid: String,
    pub coordinator_mxid: String,
    pub name: String,
    pub delegation_expires_at_ms: u64,
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
fn pending(phase: &str) -> bool {
    matches!(
        phase,
        "contacting" | "awaiting_owner" | "awaiting_admin" | "awaiting_connection" | "setup_failed"
    )
}
fn files(state: &Path) -> Result<Vec<PathBuf>, Error> {
    let dir = state.join("palpo-pairings");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    private::directory(&dir)?;
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|_| Error::Invalid("pairing directory"))? {
        let entry = entry.map_err(|_| Error::Invalid("pairing file"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.len() == 40
            && name.starts_with("hf_")
            && name.ends_with(".json")
            && name[3..35].bytes().all(|b| b.is_ascii_hexdigit())
        {
            paths.push(entry.path());
            if paths.len() > 100 {
                return Err(Error::Invalid("pairing limit"));
            }
        }
    }
    paths.sort();
    Ok(paths)
}
fn public(v: &Value, enabled: bool) -> Value {
    json!({"id":v["fleetId"],"actionId":v["actionId"],"name":v["intent"]["name"],
        "homeserver":v["homeserver"],"ownerMxid":v["ownerMxid"],"coordinatorMxid":v["intent"]["coordinatorMxid"],
        "phase":v["phase"],"imported":v["imported"],"transportEnabled":enabled,
        "expiresAtMs":v["expiresAtMs"],"problem":v["problem"]})
}
pub(crate) fn list(state: &Path, enabled: bool) -> Result<Value, Error> {
    let rows = files(state)?
        .iter()
        .map(|p| read(p).map(|v| public(&v, enabled)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({"associations":rows}))
}
pub(crate) fn create(state: &Path, input: Input) -> Result<Value, Error> {
    hagency_core::project::identifier(&input.request_id, 128)
        .map_err(|_| Error::Invalid("request id"))?;
    let server = input
        .owner_mxid
        .split_once(':')
        .filter(|(local, server)| local.starts_with('@') && local.len() > 1 && !server.is_empty())
        .map(|(_, server)| server)
        .ok_or(Error::Invalid("owner Matrix ID"))?;
    if input.owner_mxid.len() > 255
        || input.coordinator_mxid.len() > 255
        || input.owner_mxid.chars().any(char::is_whitespace)
        || input.coordinator_mxid.chars().any(char::is_whitespace)
        || !input.coordinator_mxid.starts_with('@')
        || input.coordinator_mxid.split_once(':').map(|(_, s)| s) != Some(server)
        || input.name.trim().is_empty()
        || input.name.len() > 256
        || input.name.chars().any(char::is_control)
        || input.delegation_expires_at_ms <= now()
        || input.delegation_expires_at_ms > now() + 366 * 86_400_000
    {
        return Err(Error::Invalid("association fields"));
    }
    let homeserver = origin(&input.homeserver)?.origin().ascii_serialization();
    let endpoint = origin(input.palpo_origin.as_deref().unwrap_or(&homeserver))?
        .origin()
        .ascii_serialization();
    let runtime = identity(state)?;
    let intent = json!({"requestId":input.request_id,"name":input.name,"runtimeId":runtime,
        "coordinatorMxid":input.coordinator_mxid,"delegationExpiresAtMs":input.delegation_expires_at_ms,
        "allowSelfApproval":false,"exportMxids":[]});
    let key = hagency_core::canonical::digest(
        &json!({"kind":"association","owner":input.owner_mxid,"requestId":input.request_id}),
    )
    .map_err(|_| Error::Invalid("request id"))?;
    let fleet = format!("hf_{}", &key[..32]);
    let directory = state.join("palpo-pairings");
    private::directory(&directory)?;
    let file = directory.join(format!("{fleet}.json"));
    if file.exists() {
        let v = read(&file)?;
        if v["intent"] != intent || v["homeserver"] != homeserver || v["endpoint"] != endpoint {
            return Err(Error::Invalid("request id already binds different content"));
        }
        return Ok(json!({"id":fleet}));
    }
    if files(state)?.len() >= 100 {
        return Err(Error::Invalid("pairing limit"));
    }
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::Invalid("pairing entropy"))?;
    let token = format!("{:x}", Sha256::digest(bytes));
    let v = json!({"fleetId":fleet,"actionId":format!("action_{}",&key[..32]),"serverName":server,
        "ownerMxid":input.owner_mxid,"homeserver":homeserver,"endpoint":endpoint,"intent":intent,
        "token":token,"phase":"contacting","imported":false,"submitted":false,
        "expiresAtMs":now()+30*60_000,"retryUntilMs":input.delegation_expires_at_ms.min(now()+7*86_400_000),"problem":null});
    // Commit credentials and frozen intent before any remote request, including
    // a request whose response may be lost or a browser that closes immediately.
    private::replace(
        &file,
        &serde_json::to_vec(&v).map_err(|_| Error::Invalid("pairing file"))?,
    )?;
    Ok(json!({"id":fleet}))
}
fn client(state: &Path) -> Result<reqwest::Client, Error> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10));
    if let Some(pem) =
        super::config::matrix_root(state).map_err(|_| Error::Invalid("matrix.ca.pem"))?
    {
        builder = builder.add_root_certificate(
            reqwest::Certificate::from_pem(&pem).map_err(|_| Error::Invalid("matrix.ca.pem"))?,
        );
    }
    builder.build().map_err(|_| Error::Invalid("HTTP client"))
}
pub(crate) async fn tick(
    live: &Live,
    cancel: &hagency_palpo::CancellationToken,
) -> Result<(), Error> {
    let state = live.state_dir();
    let client = client(state)?;
    for path in files(state)? {
        if cancel.is_cancelled() {
            break;
        }
        let mut v = read(&path)?;
        if !pending(v["phase"].as_str().unwrap_or_default()) {
            continue;
        }
        if v["retryUntilMs"].as_u64().unwrap_or(0) <= now() {
            v["phase"] = json!("expired");
            v["problem"] = Value::Null;
        } else {
            // Fixed errors only. Upstream bodies, URLs with credentials, and
            // downloaded profiles must never appear in console observations.
            match advance(live, &client, &mut v).await {
                Ok(()) => v["problem"] = Value::Null,
                Err(Error::Invalid("profile binding")) => v["problem"] = json!("profile_binding"),
                Err(_) => v["problem"] = json!("retrying"),
            }
        }
        private::replace(
            &path,
            &serde_json::to_vec(&v).map_err(|_| Error::Invalid("pairing file"))?,
        )?;
    }
    Ok(())
}
async fn advance(live: &Live, client: &reqwest::Client, v: &mut Value) -> Result<(), Error> {
    let endpoint = origin(
        v["endpoint"]
            .as_str()
            .ok_or(Error::Invalid("pairing endpoint"))?,
    )?;
    let token = v["token"]
        .as_str()
        .ok_or(Error::Invalid("pairing capability"))?
        .to_owned();
    if v["submitted"] != true {
        let result = response(
            client
                .post(
                    endpoint
                        .join("_palpo/miniapp/v1/association-start")
                        .unwrap(),
                )
                .bearer_auth(&token)
                .header("Content-Type", "application/json")
                .body(json!({"ownerMxid":v["ownerMxid"],"intent":v["intent"]}).to_string())
                .send()
                .await,
        )
        .await?;
        if result["actionId"] != v["actionId"]
            || result["fleetId"] != v["fleetId"]
            || result["serverName"] != v["serverName"]
        {
            return Err(Error::Invalid("profile binding"));
        }
        let canonical = origin(
            result["serverOrigin"]
                .as_str()
                .ok_or(Error::Invalid("profile binding"))?,
        )?
        .origin()
        .ascii_serialization();
        let binding = json!({"fleetId":v["fleetId"],"serverName":v["serverName"],"ownerMxid":v["ownerMxid"],
            "homeserver":v["homeserver"],"serverOrigin":canonical,"intent":v["intent"]});
        let dir = live.state_dir().join("palpo-associations");
        private::directory(&dir)?;
        let file = dir.join(format!(
            "{}.json",
            v["fleetId"].as_str().ok_or(Error::Invalid("fleet id"))?
        ));
        if file.exists() {
            if read(&file)? != binding {
                return Err(Error::Invalid("profile binding"));
            }
        } else {
            write_new(&file, &binding)?;
        }
        v["submitted"] = json!(true);
    }
    let result = response(
        client
            .post(
                endpoint
                    .join("_palpo/miniapp/v1/association-status")
                    .unwrap(),
            )
            .bearer_auth(&token)
            .header("Content-Type", "application/json")
            .body(json!({"actionId":v["actionId"],"imported":v["imported"]}).to_string())
            .send()
            .await,
    )
    .await?;
    if result["actionId"] != v["actionId"]
        || result["fleetId"] != v["fleetId"]
        || result["ownerMxid"] != v["ownerMxid"]
        || result["coordinatorMxid"] != v["intent"]["coordinatorMxid"]
    {
        return Err(Error::Invalid("profile binding"));
    }
    if let Some(profile) = result.get("profile") {
        live.import(&profile.to_string(), v["homeserver"].as_str().unwrap())
            .await
            .map_err(|_| Error::Invalid("profile binding"))?;
        v["imported"] = json!(true);
    }
    let phase = result["phase"]
        .as_str()
        .ok_or(Error::Invalid("pairing phase"))?;
    if !matches!(
        phase,
        "awaiting_owner"
            | "awaiting_admin"
            | "awaiting_connection"
            | "connected"
            | "setup_failed"
            | "rejected"
            | "expired"
            | "unavailable"
    ) || phase == "connected" && v["imported"] != true
    {
        return Err(Error::Invalid("pairing phase"));
    }
    v["phase"] = json!(if phase == "connected"
        && !live.pairing_verified(v["fleetId"].as_str().unwrap()).await
    {
        "awaiting_connection"
    } else {
        phase
    });
    v["expiresAtMs"] = json!(
        result["expiresAtMs"]
            .as_u64()
            .ok_or(Error::Invalid("pairing expiry"))?
    );
    Ok(())
}
