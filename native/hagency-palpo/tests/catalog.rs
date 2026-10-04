#[allow(dead_code)] // Other transport fixtures use the remaining shared helpers.
mod common;
use common::*;
use hagency_core::{authority::Registration, project::Resource};
use hagency_palpo::{Adapter, CancellationToken, Error, HostConfig, Step};
use hagency_store::{DomainRepository, DomainStore, Store, outbound::RegistrationIdentity};
use serde_json::{Value, json};

fn domain_registration() -> Registration {
    Registration {
        fleet_id: FLEET.into(),
        generation: 7,
        server_name: "matrix.example.test".into(),
        representative_mxid: format!("@{FLEET}_representative:matrix.example.test"),
        approval_bot_mxid: "@approval:matrix.example.test".into(),
        reception_room_id: "!reception:matrix.example.test".into(),
    }
}
fn resource() -> Resource {
    serde_json::from_value(json!({"presetId":"private_preset","seatId":"private_seat",
        "framework":"codex","model":"gpt-5.6-sol","reasoning":"medium",
        "ceiling":{"tokens":1000,"period":"monthly"}}))
    .unwrap()
}
struct Context {
    _custody_dir: tempfile::TempDir,
    _domain_dir: tempfile::TempDir,
    store: Store,
    domain: DomainStore,
    adapter: Adapter,
}
impl Context {
    async fn new(endpoint: &str) -> Self {
        let custody_dir = tempfile::tempdir().unwrap();
        let domain_dir = tempfile::tempdir().unwrap();
        Self::open(custody_dir, domain_dir, endpoint).await
    }
    async fn open(
        custody_dir: tempfile::TempDir,
        domain_dir: tempfile::TempDir,
        endpoint: &str,
    ) -> Self {
        let store = common::open(&custody_dir);
        let domain = DomainStore::start(
            DomainRepository::open(&domain_dir.path().join("private")).unwrap(),
            32,
        )
        .unwrap();
        let registration = domain_registration();
        domain.register(registration.clone()).await.unwrap();
        let identity = RegistrationIdentity {
            registration_fingerprint: hagency_store::publication_fingerprint(&registration)
                .unwrap(),
            ..common::registration()
        };
        let config = HostConfig::new(identity, endpoint, TOKEN, 31, limits())
            .unwrap()
            .with_root_pem(include_bytes!("fixtures/ca.pem"))
            .unwrap();
        let adapter = Adapter::attach(config, store.clone()).await.unwrap();
        Self {
            _custody_dir: custody_dir,
            _domain_dir: domain_dir,
            store,
            domain,
            adapter,
        }
    }
    async fn close(self) {
        self.domain.shutdown().await.unwrap();
        self.store.shutdown().await.unwrap();
    }
    async fn restart(self, endpoint: &str) -> Self {
        let Self {
            _custody_dir,
            _domain_dir,
            store,
            domain,
            adapter,
        } = self;
        drop(adapter);
        domain.shutdown().await.unwrap();
        store.shutdown().await.unwrap();
        Self::open(_custody_dir, _domain_dir, endpoint).await
    }
}
fn offers(v: &Value) -> &[Value] {
    v["capabilities"]["offers"].as_array().unwrap()
}

#[tokio::test]
async fn contribution_pages_resume_after_lost_ack_and_restart_without_skipping_rows() {
    use hagency_core::project_grants::{GrantLimits, ResourceDelegation};
    let mut fake = Fake::start(true).await;
    let ctx = Context::new(&fake.endpoint).await;
    ctx.domain.put_resource(resource()).await.unwrap();
    let registration = domain_registration();
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 86400000;
    for i in 0..18 {
        ctx.domain
            .delegate_resource(ResourceDelegation {
                v: 1,
                id: format!("contribution_{i:02}"),
                revision: 1,
                fleet_id: registration.fleet_id.clone(),
                registration_generation: registration.generation,
                issuer: registration.server_name.clone(),
                resource_id: resource().id(),
                limits: GrantLimits {
                    tokens: 10,
                    max_agents: 1,
                    max_rate_per_day: 10,
                },
                expires_at_ms: expires,
            })
            .await
            .unwrap();
    }
    let cancel = CancellationToken::new();
    let mut original = Vec::new();
    let (first, ()) = tokio::join!(
        ctx.adapter.publish_resources_once(&ctx.domain, &cancel),
        async {
            let request = fake.next().await;
            let value = check(&request, 1);
            let page = &value["contributionPage"];
            assert_eq!(page["registrationGeneration"], 7);
            assert_eq!(page["contributions"].as_array().unwrap().len(), 16);
            assert_eq!(page["nextAfter"], "contribution_15");
            assert!(value["capabilities"].get("projectWorkflow").is_none());
            original = request.body.clone();
            drop(request);
        }
    );
    assert_eq!(first, Err(Error::Transport));
    let ctx = ctx.restart(&fake.endpoint).await;
    let (retry, ()) = tokio::join!(
        ctx.adapter.publish_resources_once(&ctx.domain, &cancel),
        async {
            let request = fake.next().await;
            assert_eq!(request.body, original);
            request.json(200, json!({"ok":true}));
        }
    );
    assert_eq!(retry, Ok(Step::Published));
    let (next, ()) = tokio::join!(
        ctx.adapter.publish_resources_once(&ctx.domain, &cancel),
        async {
            let request = fake.next().await;
            let value = check(&request, 2);
            let page = &value["contributionPage"];
            assert_eq!(page["after"], "contribution_15");
            assert_eq!(page["contributions"].as_array().unwrap().len(), 2);
            assert_eq!(page["contributions"][0]["grant"]["id"], "contribution_16");
            assert_eq!(page["nextAfter"], Value::Null);
            request.json(200, json!({"ok":true}));
        }
    );
    assert_eq!(next, Ok(Step::Published));
    let (again, ()) = tokio::join!(
        ctx.adapter.publish_resources_once(&ctx.domain, &cancel),
        async {
            let request = fake.next().await;
            let value = check(&request, 3);
            assert_eq!(value["contributionPage"]["after"], "");
            assert_eq!(
                value["contributionPage"]["contributions"][0]["grant"]["id"],
                "contribution_00"
            );
            request.json(200, json!({"ok":true}));
        }
    );
    assert_eq!(again, Ok(Step::Published));
    ctx.close().await;
    fake.close().await;
}
fn check(request: &Request, sequence: u64) -> Value {
    assert!(request.target.ends_with("/updates"));
    assert_eq!(request.method, "POST");
    assert_eq!(request.headers["authorization"], format!("Bearer {TOKEN}"));
    let value = request.value();
    assert_eq!(value["generation"], 31);
    assert_eq!(value["sequence"], sequence);
    assert_eq!(value["capabilities"]["fleetId"], FLEET);
    assert_eq!(
        value["capabilities"]["representativeMxid"],
        domain_registration().representative_mxid
    );
    assert!(!value.to_string().contains("private_preset"));
    assert!(!value.to_string().contains("private_seat"));
    value
}

#[tokio::test]
async fn native_catalog_outbound_dynamic() {
    let mut fake = Fake::start(true).await;
    let ctx = Context::new(&fake.endpoint).await;
    let cancel = CancellationToken::new();
    let (result, ()) = tokio::join!(
        ctx.adapter.run_with_resources(&ctx.domain, &cancel),
        async {
            let mut sequence = 0;
            loop {
                let request = fake.next().await;
                if request.target.contains("/poll?") {
                    request.json(200, empty(31));
                    continue;
                }
                sequence += 1;
                let value = check(&request, sequence);
                match sequence {
                    1 => {
                        assert!(offers(&value).is_empty());
                        ctx.domain.put_resource(resource()).await.unwrap();
                    }
                    2 => {
                        assert!(!offers(&value).is_empty());
                        for offer in offers(&value) {
                            assert_eq!(offer["resources"][0]["id"], resource().id());
                        }
                        let mut withdrawn = resource();
                        withdrawn.published = false;
                        ctx.domain.put_resource(withdrawn).await.unwrap();
                    }
                    3 => assert!(offers(&value).is_empty()),
                    _ => panic!("unexpected publication"),
                }
                request.json(200, json!({"ok":true}));
                if sequence == 3 {
                    cancel.cancel();
                    break;
                }
            }
        }
    );
    assert_eq!(result, Ok(()));
    ctx.close().await;
    fake.close().await;
}

#[tokio::test]
async fn native_catalog_outbound_pending() {
    let mut fake = Fake::start(true).await;
    let ctx = Context::new(&fake.endpoint).await;
    ctx.domain.put_resource(resource()).await.unwrap();
    let cancel = CancellationToken::new();
    let mut original = Vec::new();
    let (first, ()) = tokio::join!(
        ctx.adapter.publish_resources_once(&ctx.domain, &cancel),
        async {
            let request = fake.next().await;
            assert!(!offers(&check(&request, 1)).is_empty());
            original = request.body.clone();
            drop(request); // Actual accepted bytes, missing HTTP acknowledgment.
        }
    );
    assert_eq!(first, Err(Error::Transport));
    let mut withdrawn = resource();
    withdrawn.published = false;
    ctx.domain.put_resource(withdrawn).await.unwrap();
    let (retry, ()) = tokio::join!(
        ctx.adapter.publish_resources_once(&ctx.domain, &cancel),
        async {
            let request = fake.next().await;
            assert_eq!(request.body, original);
            check(&request, 1);
            request.json(200, json!({"ok":true}));
        }
    );
    assert_eq!(retry, Ok(Step::Published));
    let (next, ()) = tokio::join!(
        ctx.adapter.publish_resources_once(&ctx.domain, &cancel),
        async {
            let request = fake.next().await;
            assert!(offers(&check(&request, 2)).is_empty());
            request.json(200, json!({"ok":true}));
        }
    );
    assert_eq!(next, Ok(Step::Published));
    ctx.close().await;
    fake.close().await;
}

#[tokio::test]
async fn native_catalog_outbound_retirement() {
    let mut fake = Fake::start(true).await;
    let ctx = Context::new(&fake.endpoint).await;
    ctx.domain.put_resource(resource()).await.unwrap();
    let cancel = CancellationToken::new();
    let (first, ()) = tokio::join!(
        ctx.adapter.publish_resources_once(&ctx.domain, &cancel),
        async {
            let request = fake.next().await;
            check(&request, 1);
            let mut rotated = domain_registration();
            rotated.generation += 1;
            ctx.domain.register(rotated).await.unwrap();
            drop(request); // Already admitted bytes cannot be recalled by rotation.
        }
    );
    assert_eq!(first, Err(Error::Transport));
    assert_eq!(
        ctx.adapter
            .publish_resources_once(&ctx.domain, &cancel)
            .await,
        Err(Error::Generation)
    );
    fake.quiesced(fake.requests()).await;
    cancel.cancel();
    assert_eq!(
        ctx.adapter
            .publish_resources_once(&ctx.domain, &cancel)
            .await,
        Err(Error::Cancelled)
    );
    ctx.close().await;
    fake.close().await;
}

#[tokio::test]
async fn native_catalog_outbound_queued_rotation() {
    use hagency_store::outbound::{Command, Reply};
    use std::time::Duration;
    let mut fake = Fake::start(true).await;
    let ctx = Context::new(&fake.endpoint).await;
    ctx.domain.put_resource(resource()).await.unwrap();
    // Preserve a real frozen update before blocking the original writer. This
    // separate connection only owns a fixture lock; it never writes authority.
    let identity = RegistrationIdentity {
        registration_fingerprint: hagency_store::publication_fingerprint(&domain_registration())
            .unwrap(),
        ..common::registration()
    };
    let snapshot = ctx.domain.published_catalog(identity).await.unwrap();
    ctx.adapter
        .freeze_update(snapshot.into_update())
        .await
        .unwrap();
    let lock = rusqlite::Connection::open(ctx._custody_dir.path().join("private/custody.sqlite3"))
        .unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut blocker = std::pin::pin!(
        ctx.store
            .outbound(Command::PendingPublication(ctx.adapter.scope()), now())
    );
    // Poll the original command once to enqueue it, then observe that the
    // exclusive writer consumed it. It cannot complete while this lock is held.
    tokio::select! {
        biased;
        result = &mut blocker => panic!("custody lock was not held: {}", result.is_ok()),
        () = tokio::task::yield_now() => {}
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while ctx.store.queue_remaining() != 32 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let cancel = CancellationToken::new();
    let mut publication = std::pin::pin!(ctx.adapter.publish_resources_once(&ctx.domain, &cancel));
    tokio::select! {
        result = &mut publication => panic!("publication did not queue: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(1), async {
            while ctx.store.queue_remaining() == 32 { tokio::task::yield_now().await; }
        }) => result.unwrap(),
    }
    // The publication itself has now enqueued a custody command, proving its
    // first current-domain observation completed before this actual rotation.
    let mut rotated = domain_registration();
    rotated.generation += 1;
    ctx.domain.register(rotated).await.unwrap();
    lock.execute_batch("ROLLBACK").unwrap();
    let (blocked, result) = tokio::join!(&mut blocker, &mut publication);
    assert!(matches!(blocked.unwrap(), Reply::Publication(Some(_))));
    assert_eq!(result, Err(Error::Generation));
    fake.quiesced(fake.requests()).await;
    assert!(matches!(
        ctx.store
            .outbound(Command::PendingPublication(ctx.adapter.scope()), now())
            .await
            .unwrap(),
        Reply::Publication(Some(_))
    ));
    ctx.domain.shutdown().await.unwrap();
    ctx.store.shutdown().await.unwrap();
    fake.close().await;
}

fn project_command(id: &str, expires: u64) -> hagency_core::project_commands::ProjectCommand {
    use hagency_core::project_commands::{ProjectCommand, ProjectOperation};
    ProjectCommand {
        v: 1,
        command_id: id.into(),
        fleet_id: FLEET.into(),
        registration_generation: 7,
        issuer: "matrix.example.test".into(),
        actor_mxid: "@admin:matrix.example.test".into(),
        expires_at_ms: expires,
        operation: ProjectOperation::RevokeProject {
            grant_id: "fixture_grant".into(),
            expected_revision: 1,
        },
    }
}
async fn expire(ctx: &Context, id: &str) -> hagency_core::project_commands::ProjectReceipt {
    use hagency_core::project_commands::ProjectAuthorization;
    let command = project_command(id, 1);
    let auth = ProjectAuthorization {
        v: 1,
        command_id: id.into(),
        command_digest: command.digest().unwrap(),
        allowed: false,
        valid_until_ms: 0,
    };
    ctx.domain
        .apply_project_command(command, domain_registration(), auth, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn project_receipt_publication_replays_exact_bytes_after_response_loss_and_restart() {
    let mut fake = Fake::start(true).await;
    let ctx = Context::new(&fake.endpoint).await;
    let first = expire(&ctx, "first").await;
    let cancel = CancellationToken::new();
    let mut original = Vec::new();
    let (lost, ()) = tokio::join!(
        ctx.adapter.publish_resources_once(&ctx.domain, &cancel),
        async {
            let request = fake.next().await;
            assert_eq!(request.value()["commandReceipts"], json!([first]));
            original = request.body.clone();
            drop(request);
        }
    );
    assert_eq!(lost, Err(Error::Transport));
    let ctx = ctx.restart(&fake.endpoint).await;
    let second = expire(&ctx, "second").await;
    assert_eq!(
        ctx.domain
            .pending_project_receipts(domain_registration())
            .await
            .unwrap()
            .len(),
        2
    );
    let (retry, ()) = tokio::join!(
        ctx.adapter.publish_resources_once(&ctx.domain, &cancel),
        async {
            let request = fake.next().await;
            assert_eq!(request.body, original);
            request.json(200, json!({"ok":true}));
        }
    );
    assert_eq!(retry, Ok(Step::Published));
    // Only receipts actually present in the frozen update are acknowledged.
    assert_eq!(
        ctx.domain
            .pending_project_receipts(domain_registration())
            .await
            .unwrap(),
        vec![second.clone()]
    );
    let (next, ()) = tokio::join!(
        ctx.adapter.publish_resources_once(&ctx.domain, &cancel),
        async {
            let request = fake.next().await;
            assert_eq!(request.value()["sequence"], 2);
            assert_eq!(request.value()["commandReceipts"], json!([second]));
            request.json(200, json!({"ok":true}));
        }
    );
    assert_eq!(next, Ok(Step::Published));
    assert!(
        ctx.domain
            .pending_project_receipts(domain_registration())
            .await
            .unwrap()
            .is_empty()
    );
    ctx.close().await;
    fake.close().await;
}

#[tokio::test]
async fn project_execution_authorization_binds_command_digest_deadline_and_registration() {
    let mut fake = Fake::start(true).await;
    let ctx = Context::new(&fake.endpoint).await;
    let cancel = CancellationToken::new();
    let command = project_command("authorization", now() + 60000);
    for case in [
        "allow",
        "deny",
        "wrong_command",
        "wrong_digest",
        "expired",
        "too_long",
        "unknown_field",
        "offline",
    ] {
        let (result, ()) = tokio::join!(
            ctx.adapter
                .authorize_project_command(&command, &ctx.domain, &cancel),
            async {
                let request = fake.next().await;
                assert!(request.target.ends_with("/authorize-command"));
                assert_eq!(request.headers["authorization"], format!("Bearer {TOKEN}"));
                assert_eq!(request.headers["x-hagency-generation"], "31");
                assert_eq!(
                    request.value(),
                    json!({"commandId":command.command_id,"commandDigest":command.digest().unwrap()})
                );
                let mut answer = json!({"v":1,"commandId":command.command_id,"commandDigest":command.digest().unwrap(),"allowed":case != "deny","validUntilMs":now()+10000});
                match case {
                    "wrong_command" => answer["commandId"] = json!("another_command"),
                    "wrong_digest" => answer["commandDigest"] = json!("0".repeat(64)),
                    "expired" => answer["validUntilMs"] = json!(now()),
                    "too_long" => answer["validUntilMs"] = json!(now() + 60000),
                    "unknown_field" => answer["override"] = json!(true),
                    _ => (),
                }
                request.json(if case == "offline" { 503 } else { 200 }, answer);
            }
        );
        if case == "allow" || case == "deny" {
            assert_eq!(result.unwrap().allowed, case == "allow");
        } else {
            assert!(result.is_err(), "{case}");
        }
    }
    let (rotated, ()) = tokio::join!(
        ctx.adapter
            .authorize_project_command(&command, &ctx.domain, &cancel),
        async {
            let request = fake.next().await;
            let mut registration = domain_registration();
            registration.generation += 1;
            ctx.domain.register(registration).await.unwrap();
            request.json(200, json!({"v":1,"commandId":command.command_id,"commandDigest":command.digest().unwrap(),"allowed":true,"validUntilMs":now()+10000}));
        }
    );
    assert_eq!(rotated.unwrap_err(), Error::Generation);
    ctx.close().await;
    fake.close().await;
}

#[tokio::test]
async fn frozen_status_page_is_acknowledged_before_producer_advances() {
    use hagency_palpo::ProbeReceipts;
    use std::sync::{Arc, Mutex};
    struct Pages(Mutex<Vec<Value>>);
    impl ProbeReceipts for Pages {
        fn pending(&self) -> Vec<Value> {
            Vec::new()
        }
        fn published(&self, _: &[Value]) {}
        fn statuses(&self) -> Vec<Value> {
            self.0.lock().unwrap().clone()
        }
        fn statuses_published(&self, sent: &[Value]) {
            self.0.lock().unwrap().retain(|v| !sent.contains(v));
        }
    }
    let mut fake = Fake::start(true).await;
    let mut ctx = Context::new(&fake.endpoint).await;
    let pages = Arc::new(Pages(Mutex::new(vec![json!({"requestId":"page_1"})])));
    ctx.adapter = ctx.adapter.with_probe_receipts(pages.clone());
    let cancel = CancellationToken::new();
    let (lost, ()) = tokio::join!(
        ctx.adapter.publish_resources_once(&ctx.domain, &cancel),
        async {
            let request = fake.next().await;
            assert_eq!(request.value()["statuses"], json!([{"requestId":"page_1"}]));
            drop(request);
        }
    );
    assert_eq!(lost, Err(Error::Transport));
    // Simulate a producer recovering another page while an old frozen update
    // remains. Acknowledging that old update must not clear the newer page.
    *pages.0.lock().unwrap() = vec![json!({"requestId":"page_2"})];
    let (retry, ()) = tokio::join!(
        ctx.adapter.publish_resources_once(&ctx.domain, &cancel),
        async {
            let request = fake.next().await;
            assert_eq!(request.value()["statuses"], json!([{"requestId":"page_1"}]));
            request.json(200, json!({"ok":true}));
        }
    );
    assert_eq!(retry, Ok(Step::Published));
    assert_eq!(pages.statuses(), vec![json!({"requestId":"page_2"})]);
    let (next, ()) = tokio::join!(
        ctx.adapter.publish_resources_once(&ctx.domain, &cancel),
        async {
            let request = fake.next().await;
            assert_eq!(request.value()["statuses"], json!([{"requestId":"page_2"}]));
            request.json(200, json!({"ok":true}));
        }
    );
    assert_eq!(next, Ok(Step::Published));
    assert!(pages.statuses().is_empty());
    ctx.close().await;
    fake.close().await;
}
