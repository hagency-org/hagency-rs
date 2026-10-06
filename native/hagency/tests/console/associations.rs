use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, Ordering},
};
use std::time::Duration;

#[derive(Default)]
struct PeerState {
    requests: Vec<Value>,
    capability: Option<String>,
    lose_first: bool,
}
pub(super) struct PairPeer {
    pub origin: String,
    pub mode: Arc<AtomicU8>,
    state: Arc<Mutex<PeerState>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for PairPeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl PairPeer {
    pub(super) async fn start(lose_first: bool) -> Self {
        let acceptor = TcpListener::new("127.0.0.1:0").try_bind().await.unwrap();
        let origin = format!("http://{}", acceptor.local_addr().unwrap());
        let state = Arc::new(Mutex::new(PeerState {
            lose_first,
            ..Default::default()
        }));
        let mode = Arc::new(AtomicU8::new(0));
        let router = Router::new()
            .hoop(PeerContext {
                state: state.clone(),
                mode: mode.clone(),
                origin: origin.clone(),
            })
            .push(Router::with_path("{**rest}").post(pairing_peer));
        let task = tokio::spawn(Server::new(acceptor).serve(router));
        Self {
            origin,
            mode,
            state,
            task,
        }
    }
}
struct PeerContext {
    state: Arc<Mutex<PeerState>>,
    mode: Arc<AtomicU8>,
    origin: String,
}
#[salvo::async_trait]
impl Handler for PeerContext {
    async fn handle(&self, _: &mut Request, depot: &mut Depot, _: &mut Response, _: &mut FlowCtrl) {
        depot.insert_typed(self.state.clone());
        depot.insert_typed(self.mode.clone());
        depot.insert_typed(self.origin.clone());
    }
}
#[handler]
async fn pairing_peer(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    assert!(req.headers().get("cookie").is_none());
    assert!(req.headers().get("origin").is_none());
    let token = req
        .headers()
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let input = req.parse_json::<Value>().await.unwrap();
    let mut state = depot
        .get_typed::<Arc<Mutex<PeerState>>>()
        .unwrap()
        .lock()
        .unwrap();
    let origin = depot.get_typed::<String>().unwrap();
    if let Some(prior) = &state.capability {
        assert_eq!(prior, &token);
    } else {
        assert_eq!(token.len(), 71);
        state.capability = Some(token);
    }
    if req.uri().path().ends_with("association-start") {
        if let Some(prior) = state.requests.first() {
            assert_eq!(prior, &input);
        }
        state.requests.push(input.clone());
        if std::mem::take(&mut state.lose_first) {
            res.status_code(StatusCode::BAD_GATEWAY);
            res.render(Json(json!({"code":"lost_reply"})));
            return;
        }
    }
    let intent = &state.requests[0]["intent"];
    let key = hagency_core::canonical::digest(&json!({"kind":"association","owner":"@owner:example.test","requestId":intent["requestId"]})).unwrap();
    let fleet = format!("hf_{}", &key[..32]);
    let action = format!("action_{}", &key[..32]);
    if req.uri().path().ends_with("association-start") {
        res.render(Json(json!({"actionId":action,"fleetId":fleet,"serverName":"example.test","serverOrigin":origin})));
        return;
    }
    assert!(req.uri().path().ends_with("association-status"));
    let mode = depot
        .get_typed::<Arc<AtomicU8>>()
        .unwrap()
        .load(Ordering::SeqCst);
    let phase = match mode {
        0 => "awaiting_owner",
        1 => "awaiting_admin",
        4 => "connected",
        _ => "awaiting_connection",
    };
    let mut answer = json!({"actionId":action,"fleetId":fleet,"phase":phase,
        "ownerMxid":"@owner:example.test","coordinatorMxid":"@coordinator:example.test","expiresAtMs":now()+3600000});
    if mode >= 2 && input["imported"] != true {
        answer["profile"] = json!({"schemaVersion":1,"fleetId":fleet,"serverName":"example.test","serverOrigin":origin,
            "runtimeId":if mode == 2 {json!("f".repeat(64))} else {intent["runtimeId"].clone()},"credentialVersion":1,
            "registration":{"id":fleet,"url":format!("{origin}/api/relay/v2/{fleet}"),"as_token":"pairing-test-appservice-secret","hs_token":"pairing-test-homeserver-secret",
                "sender_localpart":format!("{fleet}_representative"),"namespaces":{"users":[{"exclusive":true,"regex":format!("^@{fleet}_[a-z0-9_]+:example\\.test$")}],"aliases":[],"rooms":[]},"rate_limited":true,"receive_ephemeral":false},
            "transport":{"mode":"outbound","url":format!("{origin}/api/fleet/v2/{fleet}"),"token":"pairing-test-machine-secret-0123456789","generation":1},
            "engagement":{"id":fleet,"server":"example.test","owner":"@owner:example.test","coordinator":"@coordinator:example.test",
                "registrationGeneration":1,"delegationRevision":1,"delegationExpiresAtMs":intent["delegationExpiresAtMs"],"allowSelfApproval":false,"state":"configuring","coordinatorApprovalV1":false}});
    }
    res.render(Json(answer));
}
async fn progress(service: &Service, cookie: &str, phase: &str, problem: Option<&str>) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let mut reply = get("/console/api/palpo/associations", cookie)
            .send(service)
            .await;
        assert_eq!(reply.status_code, Some(StatusCode::OK));
        let value = reply.take_json::<Value>().await.unwrap();
        assert_private(&value);
        for private in [
            "pairing-test-",
            "\"profile\":",
            "\"token\":",
            "secret",
            "runtimeId",
        ] {
            assert!(!value.to_string().contains(private));
        }
        let row = &value["associations"][0];
        if row["phase"] == phase && row["problem"].as_str() == problem {
            return row.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "progress did not reach {phase}: {value}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}
#[tokio::test]
async fn native_association_recovers_lost_reply_and_restart_without_exposing_credentials() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let peer = PairPeer::start(true).await;
    let state = f.root.path().join("state");
    let service = Service::new(f.app.clone().with_palpo_import(state.clone()).router());
    let body = json!({"requestId":"desktop_pairing","homeserver":peer.origin,"ownerMxid":"@owner:example.test",
        "coordinatorMxid":"@coordinator:example.test","name":"Desktop association","delegationExpiresAtMs":now()+86400000});
    let anon = post("/console/api/palpo/associations", "")
        .json(&body)
        .send(&service)
        .await;
    assert_eq!(anon.status_code, Some(StatusCode::UNAUTHORIZED));
    assert!(!state.join("palpo-pairings").exists());
    let cookie = lifecycle_session(&service).await;
    let mut invalid = body.clone();
    invalid["coordinatorMxid"] = json!("@other:elsewhere.test");
    assert_eq!(
        post("/console/api/palpo/associations", &cookie)
            .json(&invalid)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    assert_eq!(
        post("/console/api/palpo/associations", &cookie)
            .json(&body)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    let waiting = progress(&service, &cookie, "awaiting_owner", None).await;
    assert_eq!(peer.state.lock().unwrap().requests.len(), 2);
    let file = state
        .join("palpo-pairings")
        .join(format!("{}.json", waiting["id"].as_str().unwrap()));
    let private: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(private["token"].as_str().unwrap().len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    drop(service);
    // A new runtime handle uses the private persisted request and capability.
    let service = Service::new(f.app.clone().with_palpo_import(state.clone()).router());
    let cookie = lifecycle_session(&service).await;
    peer.mode.store(1, Ordering::SeqCst);
    progress(&service, &cookie, "awaiting_admin", None).await;
    assert_eq!(peer.state.lock().unwrap().requests.len(), 2);
    peer.mode.store(2, Ordering::SeqCst);
    progress(&service, &cookie, "awaiting_admin", Some("profile_binding")).await;
    assert!(!state.join("palpo-appservice.json").exists());
    peer.mode.store(3, Ordering::SeqCst);
    let imported = progress(&service, &cookie, "awaiting_connection", None).await;
    assert_eq!(imported["imported"], true);
    assert!(state.join("palpo-appservice.json").exists());
    assert!(
        f.domain
            .coordinator_authority(waiting["id"].as_str().unwrap().into())
            .await
            .unwrap()
            .is_some()
    );
    peer.mode.store(4, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(3200)).await;
    progress(&service, &cookie, "awaiting_connection", None).await;
    // The fixture supplies the native probe result separately. A remote label
    // alone must not mark a local installation as verified.
    f.domain
        .bind_reception(
            waiting["id"].as_str().unwrap().into(),
            1,
            "!pairing:example.test".into(),
        )
        .await
        .unwrap();
    progress(&service, &cookie, "connected", None).await;
    let mut changed = body.clone();
    changed["name"] = json!("Changed intent");
    assert_eq!(
        post("/console/api/palpo/associations", &cookie)
            .json(&changed)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    drop(service);
    f.close().await;
}
