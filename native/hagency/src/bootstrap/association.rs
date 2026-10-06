//! Owner-authenticated association initiation. The durable local intent precedes
//! the Palpo request and binds a later approved profile to this installation.
use hagency_store::private;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("association setup refused: {0}")]
    Invalid(&'static str),
    #[error("private association state is unavailable")]
    Store(#[from] hagency_store::Error),
}

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    pub state_dir: PathBuf,
    /// Trusted Palpo operations origin, HTTPS except for loopback tests.
    #[arg(long)]
    pub palpo_origin: String,
    #[arg(long)]
    pub homeserver: String,
    /// Private file containing the resource owner's Matrix access token.
    #[arg(long)]
    pub matrix_token_file: PathBuf,
    /// Stable local request name; repeat it unchanged to recover a lost reply.
    #[arg(long)]
    pub request_id: String,
    /// Upgrade this existing legacy namespace through an owner/admin review.
    #[arg(long)]
    pub existing_fleet_id: Option<String>,
    #[arg(long)]
    pub name: String,
    #[arg(long)]
    pub coordinator: String,
    #[arg(long)]
    pub delegation_expires_at_ms: u64,
    #[arg(long)]
    pub allow_self_approval: bool,
    /// Explicit profile recipients, limited to this owner and coordinator.
    #[arg(long)]
    pub export_mxid: Vec<String>,
}

pub(super) fn origin(value: &str) -> Result<reqwest::Url, Error> {
    let url = reqwest::Url::parse(value).map_err(|_| Error::Invalid("origin"))?;
    if !(url.scheme() == "https"
        || url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Invalid("origin"));
    }
    Ok(url)
}
pub(super) fn read(path: &Path) -> Result<Value, Error> {
    use std::io::Read;
    let file = private::open(path, false)?;
    let mut bytes = Vec::new();
    file.take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Invalid("local intent read"))?;
    if bytes.len() > 65536 {
        return Err(Error::Invalid("local intent size"));
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("local intent"))
}
pub(super) fn create(path: &Path, value: &Value) -> Result<(), Error> {
    use std::io::Write;
    let mut file = private::open(path, true)?;
    file.write_all(&serde_json::to_vec(value).map_err(|_| Error::Invalid("local intent"))?)
        .map_err(|_| Error::Invalid("local intent write"))?;
    file.sync_all()
        .map_err(|_| Error::Invalid("local intent sync"))?;
    #[cfg(unix)]
    std::fs::File::open(
        path.parent()
            .ok_or(Error::Invalid("local intent directory"))?,
    )
    .and_then(|d| d.sync_all())
    .map_err(|_| Error::Invalid("local intent directory sync"))?;
    Ok(())
}
pub(super) fn identity(state: &Path) -> Result<String, Error> {
    let file = state.join("association-runtime.json");
    if !file
        .try_exists()
        .map_err(|_| Error::Invalid("runtime identity"))?
    {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).map_err(|_| Error::Invalid("runtime entropy"))?;
        let id = format!("{:x}", Sha256::digest(bytes));
        if let Err(e) = create(&file, &json!({"runtimeId":id}))
            && !file.exists()
        {
            return Err(e);
        }
    }
    read(&file)?["runtimeId"]
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
        .ok_or(Error::Invalid("runtime identity"))
}
pub(super) async fn response(
    response: Result<reqwest::Response, reqwest::Error>,
) -> Result<Value, Error> {
    let mut response =
        response.map_err(|_| Error::Invalid("server unavailable; retry the same request"))?;
    if !response.status().is_success() {
        return Err(Error::Invalid(
            "server refused request; check the account and request fields",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Error::Invalid("server response"))?
    {
        if bytes.len() + chunk.len() > 65536 {
            return Err(Error::Invalid("server response size"));
        }
        bytes.extend(chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("server response"))
}

pub async fn run(args: Args) -> Result<Value, Error> {
    private::read_secret(&args.state_dir.join("operator.token"))?;
    let palpo = origin(&args.palpo_origin)?;
    let homeserver = origin(&args.homeserver)?;
    if args.request_id.is_empty()
        || args.request_id.len() > 128
        || !args
            .request_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(Error::Invalid("request id"));
    }
    let token = String::from_utf8(private::read_secret(&args.matrix_token_file)?)
        .map_err(|_| Error::Invalid("Matrix token file"))?;
    let token = token.trim();
    if token.is_empty() || token.len() > 8192 {
        return Err(Error::Invalid("Matrix token file"));
    }
    let mut client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(20));
    if let Some(pem) = crate::bootstrap::config::matrix_root(&args.state_dir)
        .map_err(|_| Error::Invalid("matrix.ca.pem"))?
    {
        client = client.add_root_certificate(
            reqwest::Certificate::from_pem(&pem).map_err(|_| Error::Invalid("matrix.ca.pem"))?,
        );
    }
    let client = client.build().map_err(|_| Error::Invalid("HTTP client"))?;
    let who = response(
        client
            .get(
                homeserver
                    .join("_matrix/client/v3/account/whoami")
                    .map_err(|_| Error::Invalid("homeserver"))?,
            )
            .bearer_auth(token)
            .send()
            .await,
    )
    .await?;
    let owner = who["user_id"]
        .as_str()
        .ok_or(Error::Invalid("owner account"))?;
    let (_, server) = owner
        .split_once(':')
        .filter(|(local, server)| local.starts_with('@') && !server.is_empty())
        .ok_or(Error::Invalid("owner account"))?;
    if !args.coordinator.starts_with('@')
        || args.coordinator.split_once(':').map(|(_, s)| s) != Some(server)
        || args
            .export_mxid
            .iter()
            .any(|m| m != owner && m != &args.coordinator)
    {
        return Err(Error::Invalid(
            "local coordinator and export accounts required",
        ));
    }
    let mut profile = homeserver.clone();
    profile
        .path_segments_mut()
        .map_err(|_| Error::Invalid("homeserver"))?
        .extend(["_matrix", "client", "v3", "profile", &args.coordinator]);
    response(client.get(profile).bearer_auth(token).send().await).await?;
    let runtime = identity(&args.state_dir)?;
    let mut intent = json!({"requestId":args.request_id,"name":args.name,"runtimeId":runtime,"coordinatorMxid":args.coordinator,
        "delegationExpiresAtMs":args.delegation_expires_at_ms,"allowSelfApproval":args.allow_self_approval,"exportMxids":args.export_mxid});
    let key = hagency_core::canonical::digest(
        &json!({"kind":"association","owner":owner,"requestId":args.request_id}),
    )
    .map_err(|_| Error::Invalid("request id"))?;
    let fleet = if let Some(existing) = &args.existing_fleet_id {
        if existing.len() != 35
            || !existing.starts_with("hf_")
            || !existing[3..].bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(Error::Invalid("legacy fleet id"));
        }
        intent["existingFleetId"] = json!(existing);
        existing.clone()
    } else {
        format!("hf_{}", &key[..32])
    };
    let pending = json!({"fleetId":fleet,"serverName":server,"ownerMxid":owner,"homeserver":homeserver.origin().ascii_serialization(),
        "serverOrigin":palpo.origin().ascii_serialization(),"intent":intent});
    let directory = args.state_dir.join("palpo-associations");
    private::directory(&directory)?;
    let file = directory.join(format!("{fleet}.json"));
    if file
        .try_exists()
        .map_err(|_| Error::Invalid("local intent"))?
    {
        if read(&file)? != pending {
            return Err(Error::Invalid("request id already binds different content"));
        }
    } else {
        create(&file, &pending)?;
    }
    let result = response(
        client
            .post(
                palpo
                    .join("_palpo/miniapp/v1/association-request")
                    .map_err(|_| Error::Invalid("Palpo origin"))?,
            )
            .bearer_auth(token)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&intent).map_err(|_| Error::Invalid("request encoding"))?)
            .send()
            .await,
    )
    .await?;
    if result["action"]["fleetId"] != fleet
        || result["action"]["ownerMxid"] != owner
        || result["serverName"] != server
        || result["serverOrigin"] != pending["serverOrigin"]
    {
        return Err(Error::Invalid("association response binding"));
    }
    Ok(
        json!({"fleetId":fleet,"actionId":result["action"]["id"],"state":result["action"]["state"],"serverName":server,
        "next":"The designated Matrix administrator reviews this association in the Rinx Inbox. Import its approved profile here."}),
    )
}

/// Old downloads remain importable; versioned association profiles must match a
/// durable request from this installation, including its chosen homeserver.
pub(crate) fn validate_import(
    state: &Path,
    raw: &str,
    homeserver: &str,
    accepted: Option<&hagency_store::coordinator::ServerEngagement>,
) -> Result<(), Error> {
    let profile: Value = serde_json::from_str(raw).map_err(|_| Error::Invalid("profile"))?;
    if profile.get("schemaVersion").is_none() && profile.get("runtimeId").is_none() {
        // An unversioned download predates associations. It still imports a
        // fleet, but the coordinator, its self-approval and its expiry come
        // only from this installation's own pending association, never from
        // a downloaded file, and an accepted engagement is re-imported only
        // through the checks below.
        if profile.get("engagement").is_some() {
            return Err(Error::Invalid(
                "a coordinator engagement needs this installation's association profile",
            ));
        }
        if accepted.is_some() {
            return Err(Error::Invalid(
                "this engagement was accepted through an association; import its association profile",
            ));
        }
        return Ok(());
    }
    if profile["schemaVersion"] != 1 {
        return Err(Error::Invalid("profile schema"));
    }
    let fleet = profile["fleetId"]
        .as_str()
        .filter(|s| {
            s.len() == 35 && s.starts_with("hf_") && s[3..].bytes().all(|b| b.is_ascii_hexdigit())
        })
        .ok_or(Error::Invalid("fleet id"))?;
    let pending = read(
        &state
            .join("palpo-associations")
            .join(format!("{fleet}.json")),
    )?;
    let runtime = read(&state.join("association-runtime.json"))?;
    let policy = if let Some(accepted) = accepted {
        if matches!(
            serde_json::to_value(accepted.state)
                .map_err(|_| Error::Invalid("accepted delegation"))?
                .as_str(),
            Some("suspended" | "revoked")
        ) {
            return Err(Error::Invalid("delegation is suspended or revoked"));
        }
        serde_json::to_value(accepted).map_err(|_| Error::Invalid("accepted delegation"))?
    } else {
        json!({"coordinator":pending["intent"]["coordinatorMxid"],"allowSelfApproval":pending["intent"]["allowSelfApproval"],
            "delegationExpiresAtMs":pending["intent"]["delegationExpiresAtMs"],"delegationRevision":1})
    };
    if profile["runtimeId"] != runtime["runtimeId"]
        || profile["runtimeId"] != pending["intent"]["runtimeId"]
        || profile["serverName"] != pending["serverName"]
        || profile["serverOrigin"] != pending["serverOrigin"]
        || profile["engagement"]["id"] != fleet
        || profile["engagement"]["owner"] != pending["ownerMxid"]
        || profile["engagement"]["coordinator"] != policy["coordinator"]
        || profile["engagement"]["allowSelfApproval"] != policy["allowSelfApproval"]
        || profile["engagement"]["delegationExpiresAtMs"] != policy["delegationExpiresAtMs"]
        || profile["engagement"]["delegationRevision"] != policy["delegationRevision"]
        || origin(homeserver)?.origin().ascii_serialization() != pending["homeserver"]
    {
        return Err(Error::Invalid(
            "profile does not match this installation's pending association",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use salvo::prelude::*;
    use std::sync::{Arc, Mutex};

    struct State {
        requests: Arc<Mutex<Vec<Value>>>,
        origin: String,
    }
    #[salvo::async_trait]
    impl Handler for State {
        async fn handle(
            &self,
            _: &mut Request,
            depot: &mut Depot,
            _: &mut Response,
            _: &mut FlowCtrl,
        ) {
            depot.insert_typed(self.requests.clone());
            depot.insert("origin", self.origin.clone());
        }
    }

    #[handler]
    async fn peer(req: &mut Request, depot: &mut Depot, res: &mut Response) {
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer fixture-owner-token"
        );
        if req.uri().path().ends_with("/whoami") {
            res.render(Json(json!({"user_id":"@owner:example.test"})));
            return;
        }
        if req.uri().path().contains("/profile/") {
            res.render(Json(json!({"displayname":"Coordinator"})));
            return;
        }
        assert_eq!(req.uri().path(), "/_palpo/miniapp/v1/association-request");
        let body = req.parse_json::<Value>().await.unwrap();
        let fixture = depot.get_typed::<Arc<Mutex<Vec<Value>>>>().unwrap();
        let mut requests = fixture.lock().unwrap();
        requests.push(body.clone());
        if requests.len() == 1 {
            res.status_code(StatusCode::BAD_GATEWAY);
            res.render(Json(json!({"code":"lost_response"})));
            return;
        }
        let key=hagency_core::canonical::digest(&json!({"kind":"association","owner":"@owner:example.test","requestId":body["requestId"]})).unwrap();
        let fleet = body["existingFleetId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("hf_{}", &key[..32]));
        res.render(Json(json!({"action":{"fleetId":fleet,"id":format!("action_{}",&key[..32]),"ownerMxid":"@owner:example.test","state":"requested"},
            "serverName":"example.test","serverOrigin":depot.get::<String>("origin").unwrap()})));
    }

    #[tokio::test]
    async fn native_owner_association_retries_frozen_intent_and_refuses_foreign_profile() {
        association_recovery(None).await;
    }
    #[tokio::test]
    async fn native_legacy_association_keeps_selected_fleet_and_frozen_owner_intent() {
        association_recovery(Some(format!("hf_{}", "a".repeat(32)))).await;
    }
    async fn association_recovery(existing: Option<String>) {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("owner");
        crate::setup::init_state(&state).unwrap();
        private::replace(&state.join("matrix.token"), b"fixture-owner-token").unwrap();
        let acceptor = TcpListener::new("127.0.0.1:0").try_bind().await.unwrap();
        let address = acceptor.local_addr().unwrap();
        let origin = format!("http://{address}");
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let router = Router::new()
            .hoop(State {
                requests: requests.clone(),
                origin: origin.clone(),
            })
            .push(Router::with_path("{**rest}").goal(peer));
        let server = tokio::spawn(Server::new(acceptor).serve(router));
        let args = || Args {
            state_dir: state.clone(),
            palpo_origin: origin.clone(),
            homeserver: origin.clone(),
            matrix_token_file: state.join("matrix.token"),
            existing_fleet_id: existing.clone(),
            request_id: "stable_request".into(),
            name: "Owner Hagency".into(),
            coordinator: "@coordinator:example.test".into(),
            delegation_expires_at_ms: 1900000000000,
            allow_self_approval: false,
            export_mxid: vec!["@owner:example.test".into()],
        };
        assert!(run(args()).await.is_err());
        let result = run(args()).await.unwrap();
        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests[0], requests[1]);
        }
        let mut changed = args();
        changed.coordinator = "@other:example.test".into();
        assert!(run(changed).await.is_err());
        assert_eq!(requests.lock().unwrap().len(), 2);
        let fleet = result["fleetId"].as_str().unwrap();
        if let Some(existing) = &existing {
            assert_eq!(fleet, existing);
        }

        let saved = read(
            &state
                .join("palpo-associations")
                .join(format!("{fleet}.json")),
        )
        .unwrap();
        assert!(!saved.to_string().contains("fixture-owner-token"));
        let profile = json!({"schemaVersion":1,"fleetId":fleet,"serverName":"example.test","serverOrigin":origin,"runtimeId":saved["intent"]["runtimeId"],
            "engagement":{"id":fleet,"owner":"@owner:example.test","coordinator":"@coordinator:example.test","allowSelfApproval":false,"delegationRevision":1,"delegationExpiresAtMs":1900000000000_u64}});
        validate_import(&state, &profile.to_string(), &origin, None).unwrap();
        assert!(
            validate_import(
                &state,
                &profile.to_string(),
                "https://another-server.test",
                None
            )
            .is_err()
        );
        let other = root.path().join("other");
        crate::setup::init_state(&other).unwrap();
        assert!(validate_import(&other, &profile.to_string(), &origin, None).is_err());
        let mut forged = profile.clone();
        forged["engagement"]["coordinator"] = json!("@other:example.test");
        assert!(validate_import(&state, &forged.to_string(), &origin, None).is_err());
        let accepted: hagency_store::coordinator::ServerEngagement = serde_json::from_value(json!({"id":fleet,"server":"example.test","owner":"@owner:example.test","coordinator":"@other:example.test","registrationGeneration":1,"delegationRevision":2,"delegationExpiresAtMs":1900000000000_u64,"state":"verified","allowSelfApproval":false,"coordinatorApprovalV1":true})).unwrap();
        forged["engagement"]["delegationRevision"] = json!(2);
        validate_import(&state, &forged.to_string(), &origin, Some(&accepted)).unwrap();
        assert!(validate_import(&state, &profile.to_string(), &origin, Some(&accepted)).is_err());
        server.abort();
    }

    /// An unversioned download predates associations. It still imports a
    /// fleet, but it can neither carry a coordinator policy nor replace the
    /// profile of an engagement this installation accepted.
    #[test]
    fn native_association_unversioned_download_carries_no_coordinator_policy() {
        let root = tempfile::tempdir().unwrap();
        let origin = "https://matrix.example.test";
        let fleet = format!("hf_{}", "a".repeat(32));
        let plain = json!({"fleetId": fleet, "serverName": "example.test"});
        validate_import(root.path(), &plain.to_string(), origin, None).unwrap();
        let mut policy = plain.clone();
        policy["engagement"] = json!({"id":fleet,"server":"example.test","owner":"@mallory:example.test",
            "coordinator":"@mallory:example.test","registrationGeneration":1,"delegationRevision":1,
            "delegationExpiresAtMs":4102444800000_u64,"state":"verified","allowSelfApproval":true,
            "coordinatorApprovalV1":true});
        assert!(validate_import(root.path(), &policy.to_string(), origin, None).is_err());
        let accepted: hagency_store::coordinator::ServerEngagement = serde_json::from_value(json!({"id":fleet,"server":"example.test","owner":"@owner:example.test","coordinator":"@coordinator:example.test","registrationGeneration":1,"delegationRevision":1,"delegationExpiresAtMs":1900000000000_u64,"state":"suspended","allowSelfApproval":false,"coordinatorApprovalV1":true})).unwrap();
        assert!(validate_import(root.path(), &plain.to_string(), origin, Some(&accepted)).is_err());
        assert!(
            validate_import(root.path(), &policy.to_string(), origin, Some(&accepted)).is_err()
        );
    }
}
