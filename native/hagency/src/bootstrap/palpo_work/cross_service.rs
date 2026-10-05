//! Opt-in integration with the actual pinned Palpo admin service. Only Matrix
//! and the withheld capability are fixtures; no business receipt is fabricated.
use super::*;
use hagency_core::{authority::Registration, project::Resource, project_grants::*};
use hagency_palpo::{HostConfig, Limits, Step};
use hagency_store::{DomainRepository, Repository, Store, outbound::RegistrationIdentity};
use std::process::Stdio;
use tokio::{io::AsyncBufReadExt, process::Command};

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
struct Peer {
    child: tokio::process::Child,
    config: Value,
    client: reqwest::Client,
}
impl Peer {
    async fn start(dir: &Path) -> Self {
        let root = std::env::var("PALPO_SOURCE_DIR").expect("PALPO_SOURCE_DIR");
        let revision =
            std::env::var("PALPO_SOURCE_REVISION").expect("explicit PALPO_SOURCE_REVISION");
        let current = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&root)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(current.stdout).unwrap().trim(), revision);
        assert!(
            std::process::Command::new("git")
                .args([
                    "cat-file",
                    "-e",
                    &format!("{revision}:web-admin/test/hagency-workflow-peer.mjs")
                ])
                .current_dir(&root)
                .status()
                .unwrap()
                .success(),
            "the peer must be tracked at the explicit Palpo revision"
        );
        assert!(
            std::process::Command::new("git")
                .args([
                    "diff",
                    "--exit-code",
                    &revision,
                    "--",
                    "web-admin/lib",
                    "web-admin/server.mjs",
                    "web-admin/test/fixture.mjs",
                    "web-admin/test/hagency-workflow-peer.mjs"
                ])
                .current_dir(&root)
                .status()
                .unwrap()
                .success(),
            "Palpo service differs from the explicit source revision"
        );
        let mut child = Command::new(std::env::var("PALPO_TEST_NODE").expect("Node 24 executable"))
            .arg(Path::new(&root).join("web-admin/test/hagency-workflow-peer.mjs"))
            .arg(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut line = String::new();
        let count = tokio::time::timeout(
            Duration::from_secs(20),
            tokio::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(count > 0, "Palpo fixture exited before readiness");
        let config: Value = serde_json::from_str(&line).unwrap();
        for key in ["endpoint", "miniapp", "matrix"] {
            let url = Url::parse(config[key].as_str().unwrap()).unwrap();
            assert_eq!(url.scheme(), "http");
            assert_eq!(url.host_str(), Some("127.0.0.1"));
        }
        eprintln!(
            "Palpo source {revision}; actual HTTP/SQLite and Rust worker; explicit Matrix fixture"
        );
        Self {
            child,
            config,
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(Policy::none())
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap(),
        }
    }
    async fn control(&self, name: &str, body: Value) -> Value {
        let response = self
            .client
            .post(format!(
                "{}/fixture/{name}",
                self.config["matrix"].as_str().unwrap()
            ))
            .bearer_auth(self.config["controlToken"].as_str().unwrap())
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        let status = response.status();
        let value: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(status, StatusCode::OK, "fixture {name}: {value}");
        value
    }
    async fn call_status(&self, role: &str, service: &str, args: Value) -> (StatusCode, Value) {
        let response = self
            .client
            .post(self.config["miniapp"].as_str().unwrap())
            .bearer_auth(self.config["sessions"][role].as_str().unwrap())
            .header("content-type", "application/json")
            .body(json!({"service":service,"args":args}).to_string())
            .send()
            .await
            .unwrap();
        let status = response.status();
        let value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        (status, value)
    }
    async fn call(&self, role: &str, service: &str, args: Value) -> Value {
        let (status, value) = self.call_status(role, service, args).await;
        assert_eq!(status, StatusCode::OK, "{role} {service}: {value}");
        value
    }
    async fn close(mut self) {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap();
    }
}

struct Worker {
    store: Store,
    domain: DomainStore,
    adapter: Adapter,
    reader: Reader,
    probes: Arc<Probes>,
    registration: Registration,
}
impl Worker {
    async fn open(dir: &Path, peer: &Peer) -> Self {
        let registration: Registration =
            serde_json::from_value(peer.config["registration"].clone()).unwrap();
        let store = Store::start(Repository::open(&dir.join("custody")).unwrap(), 32).unwrap();
        let domain =
            DomainStore::start(DomainRepository::open(&dir.join("domain")).unwrap(), 32).unwrap();
        domain.register(registration.clone()).await.unwrap();
        let identity = RegistrationIdentity {
            binding: "project-cross-service".into(),
            registration_generation: registration.generation,
            side_id: registration.server_name.clone(),
            fleet_id: registration.fleet_id.clone(),
            registration_fingerprint: hagency_store::publication_fingerprint(&registration)
                .unwrap(),
        };
        let config = HostConfig::new(
            identity,
            peer.config["endpoint"].as_str().unwrap(),
            peer.config["machineToken"].as_str().unwrap(),
            peer.config["machineGeneration"].as_u64().unwrap(),
            Limits {
                poll_wait: Duration::ZERO,
                ..Limits::default()
            },
        )
        .unwrap();
        let probes = Probes::new(dir);
        let adapter = Adapter::attach(config, store.clone())
            .await
            .unwrap()
            .with_probe_receipts(probes.clone());
        let reader = Reader::new(&Appservice {
            homeserver: peer.config["matrix"].as_str().unwrap().into(),
            as_token: peer.config["appserviceToken"].as_str().unwrap().into(),
            representative: registration.representative_mxid.clone(),
        })
        .unwrap();
        Self {
            store,
            domain,
            adapter,
            reader,
            probes,
            registration,
        }
    }
    async fn publish(&self, peer: &Peer) {
        let cancel = CancellationToken::new();
        // The real loop scans continuously. Cross an empty end page here too,
        // so a preceding scan cannot hide the state this step needs to verify.
        for _ in 0..2 {
            refresh_statuses(
                &self.adapter,
                &self.domain,
                &self.probes,
                &self.registration.fleet_id,
                &self.reader,
                &cancel,
            )
            .await;
            if !self.probes.statuses().is_empty() {
                break;
            }
        }
        assert_eq!(
            self.adapter
                .publish_resources_once(&self.domain, &CancellationToken::new())
                .await
                .unwrap(),
            Step::Published
        );
        peer.control("support", json!({})).await;
    }
    async fn consume(&self) {
        let cancel = CancellationToken::new();
        assert_eq!(
            self.adapter.poll_once(Lane::Work, &cancel).await.unwrap(),
            Step::Acknowledged
        );
        assert!(
            matches!(
                work_once(
                    &self.adapter,
                    &self.probes,
                    &self.domain,
                    &self.reader,
                    &self.registration.fleet_id,
                    &cancel
                )
                .await,
                Outcome::Done
            ),
            "actual work consumer did not commit a business receipt"
        );
    }
    async fn close(self) {
        self.domain.shutdown().await.unwrap();
        self.store.shutdown().await.unwrap();
    }
}
fn resource(index: usize) -> Resource {
    serde_json::from_value(json!({"presetId":format!("cross_preset_{index}"),"seatId":format!("cross_seat_{index}"),
        "framework":"codex","model":"gpt-5.6-sol","reasoning":"medium","ceiling":{"tokens":4000000,"period":"monthly"},"published":true})).unwrap()
}
async fn project(peer: &Peer, worker: &Worker, name: &str, pools: &[Resource]) -> Value {
    let requested = peer.call("owner", "palpo.inbox.submit", json!({"requestId":name,"kind":"project","name":name,"reason":"Cross-service budget",
        "fleetId":worker.registration.fleet_id,"resourceIds":pools.iter().map(Resource::id).collect::<Vec<_>>(),
        "allocations":pools.iter().map(|r| json!({"contributionId":format!("contribution_{}", r.id()),"resourceId":r.id(),
            "limits":{"tokens":400000,"maxAgents":4,"maxRatePerDay":40000},"durationHours":24})).collect::<Vec<_>>() })).await;
    let action = &requested["action"];
    peer.call("admin", "palpo.inbox.decide", json!({"id":action["id"],"expectedRevision":action["revision"],
        "commandId":format!("approve_{name}"),"decision":"approve","reason":"Reviewed","administrators":["@other:example.test"],"allowSelfApproval":false})).await;
    action.clone()
}
async fn action(peer: &Peer, id: &Value, role: &str) -> Value {
    peer.call(role, "palpo.inbox.get", json!({"id":id})).await["action"].clone()
}

#[tokio::test]
#[ignore = "requires explicit PALPO_SOURCE_DIR, PALPO_SOURCE_REVISION and PALPO_TEST_NODE; no deployed service"]
async fn actual_palpo_worker_receipts_and_restart_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let peer = Peer::start(temp.path()).await;
    let mut worker = Worker::open(temp.path(), &peer).await;
    let pools = [resource(0), resource(1)];
    for pool in &pools {
        worker.domain.put_resource(pool.clone()).await.unwrap();
        worker
            .domain
            .delegate_resource(ResourceDelegation {
                v: 1,
                id: format!("contribution_{}", pool.id()),
                revision: 1,
                fleet_id: worker.registration.fleet_id.clone(),
                registration_generation: 1,
                issuer: worker.registration.server_name.clone(),
                resource_id: pool.id(),
                limits: GrantLimits {
                    tokens: 2000000,
                    max_agents: 20,
                    max_rate_per_day: 200000,
                },
                expires_at_ms: now() + 30 * 86400000,
            })
            .await
            .unwrap();
    }
    worker.publish(&peer).await;
    let proposal = project(&peer, &worker, "RealBudget", &pools[..1]).await;
    worker.consume().await;
    // Transport custody ACK cannot allocate the project before the business publication.
    assert_eq!(
        action(&peer, &proposal["id"], "owner").await["execution"],
        "awaiting_reservation"
    );
    peer.control("drop-update", json!({})).await;
    assert!(
        worker
            .adapter
            .publish_resources_once(&worker.domain, &CancellationToken::new())
            .await
            .is_err()
    );
    let committed = peer.control("state", json!({})).await;
    assert_eq!(
        action(&peer, &proposal["id"], "owner").await["execution"],
        "done"
    );
    worker.close().await;
    worker = Worker::open(temp.path(), &peer).await;
    worker.publish(&peer).await;
    let replay = peer.control("state", json!({})).await;
    assert_eq!(replay["sequence"], committed["sequence"]);
    assert_eq!(replay["notices"], committed["notices"]);
    let updates = replay["updates"].as_array().unwrap();
    assert_eq!(
        updates[updates.len() - 1]["digest"],
        updates[updates.len() - 2]["digest"]
    );

    let requested = peer.call("owner", "palpo.requests.create", json!({"requestId":"agent_real","projectId":proposal["result"]["projectId"],
        "role":"coding","requestedTokens":100000,"ratePerDay":10000,"agentDefinition":{"name":"Worker","resourceId":pools[0].id()}})).await;
    let agent_id = &requested["request"]["actionId"];
    let row = action(&peer, agent_id, "assigned").await;
    let decision = json!({"id":agent_id,"expectedRevision":row["revision"],"commandId":"approve_agent_real","decision":"approve","reason":"Reviewed","allocatedTokens":80000});
    assert!(
        !peer
            .call_status("owner", "palpo.inbox.decide", decision.clone())
            .await
            .0
            .is_success()
    );
    peer.call("assigned", "palpo.inbox.decide", decision.clone())
        .await;
    worker.consume().await;
    worker.publish(&peer).await;
    let admitted = action(&peer, agent_id, "owner").await;
    assert_eq!(admitted["execution"], "done");
    let engagement = admitted["result"]["engagementId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        worker
            .domain
            .engagements("".into(), 100)
            .await
            .unwrap()
            .len(),
        1
    );
    let state = peer.control("state", json!({})).await;
    assert_eq!(state["legacyDeliveries"], 0);
    assert_eq!(state["requests"][0]["allocation"]["tokens"], 80000);
    assert_eq!(
        state["requests"][0]["lifecycle"]["spentTokensLowerBound"],
        Value::Null
    );
    assert_eq!(state["requests"][0]["lifecycle"]["runtimeStopped"], false);
    assert_eq!(state["requests"][0]["usable"], false); // No physical runtime fixture claims Ready.
    let command_id = state["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["command"]["operation"]["kind"] == "approve_agent")
        .unwrap()["command"]["commandId"]
        .clone();
    worker.close().await;
    worker = Worker::open(temp.path(), &peer).await;
    peer.control("redeliver", json!({"commandId":command_id}))
        .await;
    worker.consume().await;
    worker.publish(&peer).await;
    assert_eq!(
        worker
            .domain
            .engagements("".into(), 100)
            .await
            .unwrap()
            .len(),
        1
    );

    let increase = peer
        .call(
            "owner",
            "palpo.inbox.submit",
            json!({"requestId":"increase_real","kind":"top_up",
        "agentRequestId":requested["request"]["id"],"addTokens":50000,"reason":"Continue"}),
        )
        .await;
    let row = &increase["action"];
    peer.call(
        "assigned",
        "palpo.inbox.decide",
        json!({"id":row["id"],"expectedRevision":row["revision"],"commandId":"approve_increase",
        "decision":"approve","reason":"Finite increase","allocatedTokens":40000}),
    )
    .await;
    worker.consume().await;
    worker.publish(&peer).await;
    assert_eq!(
        peer.control("state", json!({})).await["requests"][0]["allocation"]["tokens"],
        120000
    );
    let state = peer.control("state", json!({})).await;
    let top_up = state["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["command"]["operation"]["kind"] == "top_up_agent")
        .unwrap()["command"]["commandId"]
        .clone();
    peer.control("redeliver", json!({"commandId":top_up})).await;
    worker.consume().await;
    worker.publish(&peer).await;
    assert_eq!(
        worker
            .domain
            .engagement(engagement.clone())
            .await
            .unwrap()
            .allocation(),
        120000.try_into().unwrap()
    );
    let removal = peer
        .call(
            "owner",
            "palpo.inbox.submit",
            json!({"requestId":"remove_real","kind":"agent_removal",
        "agentRequestId":requested["request"]["id"],"reason":"Finished"}),
        )
        .await;
    worker.consume().await;
    assert_eq!(
        action(&peer, &removal["action"]["id"], "owner").await["execution"],
        "awaiting_hagency"
    );
    worker.publish(&peer).await;
    assert_eq!(
        action(&peer, &removal["action"]["id"], "owner").await["execution"],
        "done"
    );
    let removed = peer.control("state", json!({})).await;
    assert_eq!(removed["requests"][0]["state"], "removed");
    assert_eq!(removed["requests"][0]["lifecycle"]["runtimeStopped"], true);
    assert_eq!(
        removed["requests"][0]["lifecycle"]["localCleanup"],
        "not_required"
    );
    assert_eq!(
        removed["requests"][0]["lifecycle"]["agentMxid"],
        Value::Null
    );
    assert_eq!(removed["requests"][0]["lifecycle"]["matrixRetired"], false);
    assert_eq!(
        worker.domain.engagement(engagement).await.unwrap().state,
        hagency_core::project::EngagementState::Revoked
    );

    let rejected = peer.call("owner", "palpo.requests.create", json!({"requestId":"agent_rejected","projectId":proposal["result"]["projectId"],
        "role":"coding","requestedTokens":100000,"ratePerDay":10000,"agentDefinition":{"name":"RejectedWorker","resourceId":pools[0].id()}})).await;
    let row = action(&peer, &rejected["request"]["actionId"], "assigned").await;
    peer.call(
        "assigned",
        "palpo.inbox.decide",
        json!({"id":row["id"],"expectedRevision":row["revision"],"commandId":"reject_agent_real",
        "decision":"reject","reason":"Not needed"}),
    )
    .await;
    worker.consume().await;
    worker.publish(&peer).await;
    assert_eq!(
        action(&peer, &row["id"], "owner").await["execution"],
        "done"
    );
    let state = peer.control("state", json!({})).await;
    assert_eq!(state["requests"][1]["state"], "rejected");
    assert_eq!(state["legacyDeliveries"], 0);

    for mode in ["retry", "release"] {
        let proposal = project(&peer, &worker, &format!("partial_{mode}"), &pools).await;
        worker.consume().await;
        peer.control("lock-admin", json!({"locked":true})).await;
        worker.consume().await;
        peer.control("lock-admin", json!({"locked":false})).await;
        worker.publish(&peer).await;
        let failed = action(&peer, &proposal["id"], "admin").await;
        assert_eq!(failed["execution"], "reservation_refused");
        let recover = json!({"id":failed["id"],"expectedRevision":failed["revision"],"commandId":format!("recover_{mode}"),"operation":mode,"reason":"Reviewed recovery"});
        peer.call("admin", "palpo.inbox.recover", recover.clone())
            .await;
        worker.consume().await;
        worker.publish(&peer).await;
        assert_eq!(
            action(&peer, &proposal["id"], "owner").await["execution"],
            if mode == "retry" { "done" } else { "released" }
        );
        peer.call("admin", "palpo.inbox.recover", recover).await;
    }
    let final_state = peer.control("state", json!({})).await;
    let commands = final_state["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 11);
    assert_eq!(
        commands
            .iter()
            .filter(|e| e["receipt"]["outcome"]["status"] == "applied")
            .count(),
        9
    );
    assert_eq!(
        commands
            .iter()
            .filter(|e| e["receipt"]["outcome"]["status"] == "refused")
            .count(),
        2
    );
    assert_eq!(final_state["pendingDeliveries"], 0);
    assert_eq!(final_state["legacyDeliveries"], 0);
    let contributions = final_state["contributions"].as_array().unwrap();
    assert_eq!(
        contributions
            .iter()
            .map(|r| r["reserved"]["tokens"].as_u64().unwrap())
            .sum::<u64>(),
        1600000
    );
    assert_eq!(
        contributions
            .iter()
            .map(|r| r["released"]["tokens"].as_u64().unwrap())
            .sum::<u64>(),
        400000
    );
    assert_eq!(
        worker
            .domain
            .engagements("".into(), 100)
            .await
            .unwrap()
            .len(),
        2
    );
    eprintln!(
        "Cross-service verified: 11 business receipts (9 applied, 2 current-authority refusals); no pending or legacy deliveries; one 120000-token agent plus one rejected request; 1600000 cumulatively reserved, 400000 verified unused release. Matrix/provider execution remains a fixture boundary."
    );
    worker.close().await;
    peer.close().await;
}
