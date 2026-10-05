#[path = "palpo_service/fixture.rs"]
mod fixture;
#[allow(dead_code)] // Reuse the existing bounded real HTTPS peer and trust root.
#[path = "../../hagency-palpo/tests/common/mod.rs"]
mod peer;
use fixture::*;
use hagency::bootstrap::{Bootstrap, Options};
use hagency_palpo::CancellationToken;
use serde_json::json;
use std::{fs, time::Duration};

#[tokio::test]
async fn native_palpo_retirement_reconciles_lost_reply_after_restart_without_a_runtime() {
    retirement_recovery(false).await;
}
#[cfg(unix)]
#[tokio::test]
async fn native_palpo_retirement_cancellation_persists_uncertainty_before_store_reopen() {
    retirement_recovery(true).await;
}
async fn retirement_recovery(cancel_reply: bool) {
    use hagency_core::project::CleanupState;
    use hagency_store::{DomainRepository, private};
    let mut f = Fixture::new(true, true).await;
    let id = retiring_agent(&f.state);
    let reserved_before = DomainRepository::open(&f.state)
        .unwrap()
        .resource_budget(&resource().id())
        .unwrap()
        .pool
        .committed;
    private::write_new(&f.state.join("palpo-appservice.json"),&serde_json::to_vec(&json!({"homeserver":"http://127.0.0.1:9","as_token":"isolated-native-fixture-token","sender_localpart":format!("{}_representative",peer::FLEET)})).unwrap()).unwrap();
    let mxid = format!("@{}_{}:matrix.example.test", peer::FLEET, id);
    let expected = json!({"requestId":"remote_retirement","agentMxid":mxid});
    let mut original = None;
    for restart in [false, true] {
        if restart {
            fs::rename(
                f.root.path().join("native.stderr"),
                f.root.path().join("native.before-restart.stderr"),
            )
            .unwrap();
        }
        let mut child = f.launch(true);
        let mut stopped = false;
        let until = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut identity_calls = 0;
        loop {
            assert!(
                tokio::time::Instant::now() < until,
                "identity cleanup did not finish: {}",
                fs::read_to_string(f.root.path().join("native.stderr")).unwrap()
            );
            let request = f.fake.next_with_timeout(Duration::from_secs(10)).await;
            if request.target.ends_with("/retire-agent") {
                assert_eq!(request.value(), expected);
                assert_eq!(request.headers["x-hagency-generation"], "31");
                assert_eq!(
                    request.headers["authorization"],
                    format!("Bearer {}", peer::TOKEN)
                );
                if let Some(previous) = &original {
                    assert_eq!(&request.body, previous);
                } else {
                    original = Some(request.body.clone());
                }
                identity_calls += 1;
                if !restart {
                    if cancel_reply {
                        #[cfg(unix)]
                        child.graceful().await;
                        stopped = true;
                        drop(request);
                    } else {
                        request.json(502, json!({}));
                    }
                    break;
                }
                if identity_calls == 1 {
                    request.json(200, json!({"ok":true}));
                    continue;
                }
                request.json(200,json!({"ok":true,"fleetId":peer::FLEET,"requestId":"remote_retirement","agent":{"mxid":mxid,"state":"retired","matrixIdentity":"deactivated","appserviceAccess":"revoked","joinedRooms":[]}}));
                break;
            } else if request.target.contains("/poll?") {
                if restart {
                    request.json(401, json!({"code":"transport_unauthorized"}));
                } else {
                    request.json(200, peer::empty(31));
                }
            } else {
                assert!(request.target.ends_with("/updates") || request.target.ends_with("/ack"));
                request.json(200, json!({"ok":true}));
            }
        }
        let target = if restart { "complete" } else { "uncertain" };
        for attempt in 0..200 {
            let state: String = f
                .sql("domain.sqlite3")
                .query_row(
                    "SELECT state FROM effects WHERE id=?1",
                    [format!("retire_{id}")],
                    |r| r.get(0),
                )
                .unwrap();
            if state == target {
                break;
            }
            assert!(
                attempt < 199,
                "effect stayed {state}; expected {target} before reopening the store"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        #[cfg(unix)]
        if !stopped {
            child.graceful().await;
        }
        drop(child);
        let db = DomainRepository::open(&f.state).unwrap();
        let effect = db.effect(&format!("retire_{id}")).unwrap();
        assert_eq!(
            effect.fence, 1,
            "inspection must retain the original effect fence"
        );
        assert_eq!(
            db.get(&id).unwrap().cleanup,
            if restart {
                CleanupState::Complete
            } else {
                CleanupState::Uncertain
            }
        );
        assert_eq!(
            db.resource_budget(&resource().id()).unwrap().pool.committed,
            reserved_before,
            "remote retirement must not invent a usage refund"
        );
    }
    f.fake.close().await;
}

#[tokio::test]
async fn native_palpo_worker_publishes_terminal_project_and_top_up_refusals() {
    use hagency_core::canonical;
    use hagency_core::custody::Lane;
    use hagency_store::{DomainRepository, private};
    let mut f = Fixture::new(true, false).await;
    let now = peer::now();
    let fleet = peer::FLEET;
    let mut db = DomainRepository::open(&f.state).unwrap();
    db.configure_coordinator(
        &serde_json::from_value(json!({
            "id":fleet,"server":"matrix.example.test","owner":"@owner:matrix.example.test",
            "coordinator":"@coordinator:matrix.example.test","registrationGeneration":7,
            "delegationRevision":2,"delegationExpiresAtMs":now+3600000,"state":"verified",
            "allowSelfApproval":false,"coordinatorApprovalV1":true
        }))
        .unwrap(),
    )
    .unwrap();
    drop(db);
    // An unreachable Matrix origin proves that local authority refusals do not
    // depend on a successful network read or an operator's second decision.
    private::write_new(
        &f.state.join("palpo-appservice.json"),
        &serde_json::to_vec(&json!({
            "homeserver":"http://127.0.0.1:9","as_token":"isolated-native-fixture-token",
            "sender_localpart":format!("{fleet}_representative")
        }))
        .unwrap(),
    )
    .unwrap();
    let context = |id: &str, revision: u64| {
        json!({
            "version":1,"commandId":id,"serverEngagementId":fleet,"registrationGeneration":7,
            "delegationRevision":revision,"actor":"@coordinator:matrix.example.test",
            "issuedAtMs":now,"expiresAtMs":now+600000
        })
    };
    let definition = json!({"name":"Stale project","roomId":"!project:matrix.example.test","ownerDmRoomId":"!dm:matrix.example.test"});
    let project = json!({"operation":"coordinator_project_approval","definition":definition,
        "command":{"context":context("stale_project",1),"request":{
            "id":"project_request","revision":1,"serverEngagementId":fleet,"projectId":"project_one",
            "owner":"@manager:matrix.example.test","requester":"@manager:matrix.example.test",
            "definitionDigest":canonical::digest(&definition).unwrap(),"resourceAllocations":["grant_one"]}}});
    let top_up = json!({"operation":"coordinator_token_top_up","command":{
        "context":context("missing_project_topup",2),"request":{
            "id":"topup_request","revision":1,"serverEngagementId":fleet,"projectId":"missing_project",
            "projectRevision":1,"resourceAllocationId":"grant_one","agentAllocationId":"agent_one",
            "projectOwner":"@manager:matrix.example.test","requester":"@manager:matrix.example.test",
            "definitionDigest":"a".repeat(64),"expectedAllocatedTokens":200,"requestedAdditionalTokens":50},
        "additionalTokens":50}});
    let commands = [project, top_up];
    let mut child = f.launch(true);
    let mut sent = 0;
    let mut refusals = std::collections::BTreeMap::new();
    let until = tokio::time::Instant::now() + Duration::from_secs(45);
    while refusals.len() < 2 {
        assert!(
            tokio::time::Instant::now() < until,
            "native refusals were not published: {}",
            fs::read_to_string(f.root.path().join("native.stderr")).unwrap()
        );
        let request = f.fake.next().await;
        if request.target.contains("/poll?") {
            if request.target.contains("lane=work") && sent < commands.len() {
                let mut delivery =
                    peer::delivery(&format!("decision_{sent}"), Lane::Work, 31, "private-lease");
                delivery["delivery"]["payload"] = commands[sent].clone();
                sent += 1;
                request.json(200, delivery);
            } else {
                request.json(200, peer::empty(31));
            }
        } else if request.target.ends_with("/ack") {
            request.json(200, json!({"ok":true}));
        } else {
            assert!(request.target.ends_with("/updates"));
            if let Some(updates) = request.value()["coordinatorUpdates"].as_array() {
                for update in updates {
                    if update["payload"]["state"] == "refused" {
                        assert_eq!(update["payload"]["delegationRevision"], 2);
                        let receipt = &update["payload"];
                        refusals.insert(
                            receipt["commandId"].as_str().unwrap().to_owned(),
                            receipt.clone(),
                        );
                    }
                }
            }
            request.json(200, json!({"ok":true}));
        }
    }
    assert_eq!(refusals["stale_project"]["reason"], "authority_changed");
    assert_eq!(
        refusals["missing_project_topup"]["reason"],
        "project_unavailable"
    );
    #[cfg(unix)]
    child.graceful().await;
    drop(child);
    let db = DomainRepository::open(&f.state).unwrap();
    for command in &commands {
        let id = command["command"]["context"]["commandId"].as_str().unwrap();
        let mut published = refusals[id].clone();
        published
            .as_object_mut()
            .unwrap()
            .remove("registrationGeneration");
        published
            .as_object_mut()
            .unwrap()
            .remove("delegationRevision");
        assert_eq!(
            db.coordinator_command_outcome(fleet, command)
                .unwrap()
                .unwrap(),
            published
        );
    }
    f.no_runner();
    f.fake.close().await;
}

#[tokio::test]
async fn native_palpo_service_executable_catalog() {
    let mut f = Fixture::new(true, false).await;
    let mut child = f.launch(true);
    let first = f.publication().await;
    assert!(offers(&check(&first, 1)).is_empty());
    let status = f.capabilities().await;
    assert_eq!(status["palpo_publication"]["state"], "running");
    assert_eq!(status["development_execution"]["state"], "disabled");
    for field in [
        "agent_execution",
        "palpo_transport",
        "matrix_crypto",
        "project_request_transport",
        "production_api_parity",
    ] {
        assert_eq!(status[field], false);
    }
    assert!(!status.to_string().contains(peer::TOKEN));
    assert!(!status.to_string().contains(f.state.to_str().unwrap()));
    f.request("POST", "resources", Some(resource_body(true)))
        .await;
    first.json(200, json!({"ok":true}));
    let second = f.publication().await;
    let value = check(&second, 2);
    assert!(!offers(&value).is_empty());
    for offer in offers(&value) {
        assert_eq!(offer["resources"][0]["id"], resource().id());
        let role = offer["role"].as_str().unwrap();
        f.request(
            "POST",
            &format!("roles/{role}/publication"),
            Some(json!({"published":false})),
        )
        .await;
    }
    // A changed file is not reloaded into this original owner's identity/token.
    f.rewrite(|v| v["machine_generation"] = json!(99));
    fs::write(
        f.state.join("palpo.machine_token"),
        b"different-configured-token",
    )
    .unwrap();
    second.json(200, json!({"ok":true}));
    let third = f.publication().await;
    assert!(offers(&check(&third, 3)).is_empty());
    third.json(200, json!({"ok":true}));
    f.no_runner();
    #[cfg(unix)]
    {
        child.graceful().await;
        f.reopen();
    }
    #[cfg(not(unix))]
    let _ = &mut child; // actual Windows signal qualification is separate
    drop(child);
    f.fake.close().await;
}

#[tokio::test]
async fn native_palpo_service_original_publication() {
    let mut f = Fixture::new(true, true).await;
    let mut child = f.launch(true);
    let first = f.publication().await;
    assert!(!offers(&check(&first, 1)).is_empty());
    let original = first.body.clone();
    let pending = f.pending();
    assert_eq!(pending.0, 1);
    assert_eq!(pending.2.as_bytes(), original);
    assert_eq!(pending.3, "unknown");
    f.request("POST", "resources", Some(resource_body(false)))
        .await;
    drop(first); // original bytes reached real HTTPS; no acknowledgment
    let retry = f.publication().await;
    assert_eq!(retry.body, original);
    assert_eq!(f.pending(), pending);
    check(&retry, 1);
    retry.json(200, json!({"ok":true}));
    let next = f.publication().await;
    assert!(offers(&check(&next, 2)).is_empty());
    next.json(200, json!({"ok":true}));
    #[cfg(unix)]
    child.graceful().await;
    #[cfg(not(unix))]
    let _ = &mut child;
    drop(child);
    f.fake.close().await;
}

#[tokio::test]
async fn native_palpo_service_configuration_refusal() {
    for case in [
        "disabled",
        "malformed",
        "unknown_field",
        "duplicate_field",
        "http",
        "short_token",
        "oversized",
        "directory",
        "mismatch",
        "missing",
    ] {
        let mut f = Fixture::new(case != "missing", false).await;
        let path = f.state.join("palpo-transport.json");
        match case {
            "disabled" | "malformed" => fs::write(&path, b"{").unwrap(),
            "unknown_field" => f.rewrite(|v| v["registration_fingerprint"] = json!("a".repeat(64))),
            "duplicate_field" => {
                let value = fs::read_to_string(&path).unwrap();
                fs::write(&path, value.replacen('{', "{\"profile\":\"duplicate\",", 1)).unwrap();
            }
            // Plain HTTP is allowed only to a literal loopback address (ADR-042),
            // and the fake peer listens on 127.0.0.1. Point plain HTTP at a
            // non-loopback documentation address, which must be refused before
            // any connection is attempted.
            "http" => f.rewrite(|v| {
                v["endpoint"] = json!(v["endpoint"].as_str().unwrap().replacen(
                    "https://127.0.0.1:",
                    "http://192.0.2.1:",
                    1
                ))
            }),
            "short_token" => fs::write(f.state.join("palpo.machine_token"), b"short").unwrap(),
            "oversized" => fs::write(&path, vec![b' '; 16 * 1024 + 1]).unwrap(),
            "directory" => {
                fs::remove_file(&path).unwrap();
                fs::create_dir(&path).unwrap();
            }
            "mismatch" => f.rewrite(|v| v["registration"]["generation"] = json!(8)),
            "missing" => {}
            _ => unreachable!(),
        }
        let mut child = f.launch(case != "disabled");
        if case == "disabled" {
            let status = f.capabilities().await;
            assert_eq!(status["palpo_publication"]["state"], "disabled");
            assert_eq!(status["palpo_publication"]["configured"], false);
        } else if matches!(case, "mismatch" | "missing") {
            let status = f.terminal().await;
            assert_eq!(status["palpo_publication"]["state"], "unavailable");
            assert_eq!(
                status["palpo_publication"]["error"],
                if case == "mismatch" {
                    "generation"
                } else {
                    "custody"
                }
            );
        } else {
            child.refused().await;
        }
        // Refusal and disabled/terminal states never contact the peer: zero
        // admissions ever, proven by the counter across a derived quiet
        // window (covering the header-parse race) rather than a wall clock.
        f.fake.quiesced(0).await;
        f.untouched_registration(case != "missing");
        f.no_runner();
        #[cfg(unix)]
        if matches!(case, "disabled" | "mismatch" | "missing") {
            child.graceful().await;
        }
        drop(child);
        f.fake.close().await;
    }
}

#[tokio::test]
async fn native_palpo_service_cancel_custody() {
    let mut f = Fixture::new(true, true).await;
    let mut bootstrap = Bootstrap::open_with_options(
        &f.state,
        f.address,
        16,
        Options {
            development_driver: false,
            agent_driver: false,
            palpo_transport: true,
        },
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let signal = cancel.clone();
    let serving = tokio::spawn(Box::pin(async move {
        let result = bootstrap.serve(&signal).await;
        (result, bootstrap)
    }));
    let held = f.publication().await;
    check(&held, 1);
    // Hold one original request from each inbound lane too. Neither can issue
    // another poll while its own request is unacknowledged by this HTTPS peer.
    let matrix_or_work = f.fake.next().await;
    let other_lane = f.fake.next().await;
    assert!(matrix_or_work.target.contains("/poll?"));
    assert!(other_lane.target.contains("/poll?"));
    assert_ne!(matrix_or_work.target, other_lane.target);
    let original = f.pending();
    assert_eq!(original.3, "unknown");
    cancel.cancel();
    let (result, mut original_owner) = tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result, Ok(()));
    assert_eq!(original_owner.close().await, Ok(()));
    assert_eq!(f.pending(), original); // cancellation cannot acknowledge sent bytes
    f.reopen();
    // Sequencing point: `serving` completed above, so every transport loop
    // has unwound and no further request can be initiated. Admissions that
    // started parsing before cancellation land inside the derived window;
    // the counter proves none of them is a new send after this point.
    let settled = f.fake.requests();
    f.fake.quiesced(settled).await;
    drop(held);
    drop(matrix_or_work);
    drop(other_lane);
    f.fake.close().await;
}
