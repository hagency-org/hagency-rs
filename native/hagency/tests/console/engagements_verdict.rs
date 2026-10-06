use super::*;

// The console verdict slice (board #16, parity backend-v2.js:15154-15221):
// the operator approves or refuses a pending engagement from the console.
// Approve reaches the SAME store verdict the Matrix intake reaches
// (`DomainStore::approve`: pending → reserved + a pending `provision_{id}`
// effect — provisioning enqueued); the acceptance scenario drives the HTTP
// route and asserts exactly that store state. Candidates is the retained
// GET read; refuse already exists (`POST /agents/{id}/refuse`) and gets one
// end-to-end pass here from the engagements surface's point of view.

/// The seeded store's effects rows, read directly.
fn effects(state: &std::path::Path) -> Vec<(String, String, String)> {
    let db = rusqlite::Connection::open(state.join("domain.sqlite3")).unwrap();
    let mut stmt = db
        .prepare("SELECT engagement_id,kind,state FROM effects ORDER BY rowid")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Scenario (acceptance): the operator approves a pending engagement in the
/// console and the same effect happens as the Matrix approval — the store
/// state becomes reserved and provisioning is enqueued.
#[tokio::test]
async fn native_engagement_verdict_approve_reserves_and_enqueues_provision() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let service = f.service();
    let state = f.root.path().join("state");
    let pending = f.new_engagement().await;

    // Candidates before the decision: one project-definition candidate,
    // unlocked, naming the stored resource.
    let read_only = session(&service).await;
    let mut listed = get(
        &format!("/console/api/engagements/{pending}/candidates"),
        &read_only,
    )
    .send(&service)
    .await;
    assert_eq!(listed.status_code, Some(StatusCode::OK));
    let body = listed.take_json::<Value>().await.unwrap();
    assert_eq!(body["locked"], false);
    assert_eq!(body["allocation"], json!({"kind": "project-definition"}));
    let candidate = body["candidates"]
        .as_array()
        .unwrap()
        .first()
        .cloned()
        .unwrap();
    assert_eq!(candidate["choice"], json!({"kind": "project-definition"}));
    assert_eq!(candidate["name"], "NewUsageWorker");
    assert_eq!(candidate["resource"], "private_usage_pool");
    assert_eq!(candidate["provision"], true);

    // TS parity (#31): there is no read-only login — one login is the whole
    // console. An anonymous caller is refused before any store job; every
    // logged-in session may decide. The anonymous caller needs no ticket at
    // all (the console's authenticate hoop rejects it without a cookie).
    let anonymous = TestClient::post(format!("{BASE}/console/api/engagements/{pending}/approve"))
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("origin", BASE, true)
        .add_header("sec-fetch-site", "same-origin", true)
        .json(&json!({"commandId": "cmd_verdict_1"}))
        .send(&service)
        .await;
    assert_eq!(anonymous.status_code, Some(StatusCode::UNAUTHORIZED));

    // Ticket issuance is rate-limited to one per second.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let cookie = lifecycle_session(&service).await;
    let mut response = post(
        &format!("/console/api/engagements/{pending}/approve"),
        &cookie,
    )
    .json(&json!({"commandId": "cmd_verdict_1"}))
    .send(&service)
    .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let body = response.take_json::<Value>().await.unwrap();
    assert_eq!(body["id"], pending);
    assert_eq!(body["state"], "reserved");
    assert_eq!(body["cleanup"], "not_required");
    assert_eq!(body.as_object().unwrap().len(), 3);

    // The store effect: the provision row for THIS engagement is enqueued.
    let rows = effects(&state);
    let provision = rows
        .iter()
        .find(|r| r.0 == pending && r.1 == "provision")
        .expect("provision effect enqueued");
    assert_eq!(provision.2, "pending");

    // Candidates after the decision: locked, the retained
    // `e.state !== 'pending'` word.
    let mut listed = get(
        &format!("/console/api/engagements/{pending}/candidates"),
        &read_only,
    )
    .send(&service)
    .await;
    assert_eq!(listed.status_code, Some(StatusCode::OK));
    let body = listed.take_json::<Value>().await.unwrap();
    assert_eq!(body["locked"], true);

    // A second approve with a new command id is the pending-only guard
    // (domain.rs:1212-1214): conflict, never a second reservation.
    let again = post(
        &format!("/console/api/engagements/{pending}/approve"),
        &cookie,
    )
    .json(&json!({"commandId": "cmd_verdict_2"}))
    .send(&service)
    .await;
    assert_eq!(again.status_code, Some(StatusCode::CONFLICT));
    f.close().await;
}

/// Scenario: approving a request larger than the remaining allocation is a
/// named verdict refusal (`over_commit`), not an unreadable engagement. The
/// console showed "the native API is unavailable" for this (live, 2026-09-30).
#[tokio::test]
async fn native_engagement_verdict_over_commit_is_named() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let service = f.service();
    let pending = f.new_engagement_requesting("over_the_ceiling", 5000).await;
    let cookie = lifecycle_session(&service).await;
    let mut response = post(
        &format!("/console/api/engagements/{pending}/approve"),
        &cookie,
    )
    .json(&json!({"commandId": "cmd_over_commit"}))
    .send(&service)
    .await;
    assert_eq!(response.status_code, Some(StatusCode::CONFLICT));
    let body = response.take_json::<Value>().await.unwrap();
    assert_eq!(body["code"], "over_commit");
    f.close().await;
}

/// Scenario: the operator refuses a pending engagement from the console; the
/// existing refuse route ends it rejected with no provision work.
#[tokio::test]
async fn native_engagement_verdict_refuse_rejects_pending() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let service = f.service();
    let state = f.root.path().join("state");
    let pending = f.new_engagement().await;
    let cookie = lifecycle_session(&service).await;
    let mut response = post(&format!("/console/api/agents/{pending}/refuse"), &cookie)
        .json(&json!({"commandId": "cmd_refuse_1"}))
        .send(&service)
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let body = response.take_json::<Value>().await.unwrap();
    assert_eq!(body["engagement"]["id"], pending);
    assert_eq!(body["engagement"]["state"], "rejected");
    // No provision row was ever enqueued for the refused request.
    let rows = effects(&state);
    assert!(rows.iter().all(|r| r.0 != pending));
    f.close().await;
}

/// Scenario: once the fleet has a coordinator (ADR-191), the console can
/// neither refuse nor approve its pending request; either would be a second
/// verdict on the coordinator's decision. The request stays pending.
#[tokio::test]
async fn native_engagement_verdict_leaves_a_coordinator_request_to_the_coordinator() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let service = f.service();
    let pending = f.new_engagement().await;
    let fleet = common::registration().fleet_id;
    f.domain
        .configure_coordinator(
            serde_json::from_value(json!({"id":fleet,"server":"example.test",
                "owner":"@provider:example.test","coordinator":"@coordinator:example.test",
                "registrationGeneration":1,"delegationRevision":1,
                "delegationExpiresAtMs":now()+3600000,"state":"verified",
                "allowSelfApproval":false,"coordinatorApprovalV1":true}))
            .unwrap(),
        )
        .await
        .unwrap();
    let cookie = lifecycle_session(&service).await;
    let mut refused = post(&format!("/console/api/agents/{pending}/refuse"), &cookie)
        .json(&json!({"commandId": "cmd_refuse_coordinated"}))
        .send(&service)
        .await;
    assert_eq!(refused.status_code, Some(StatusCode::CONFLICT));
    assert_eq!(
        refused.take_json::<Value>().await.unwrap()["code"],
        "coordinator_managed"
    );
    let mut approved = post(
        &format!("/console/api/engagements/{pending}/approve"),
        &cookie,
    )
    .json(&json!({"commandId": "cmd_approve_coordinated"}))
    .send(&service)
    .await;
    assert_eq!(approved.status_code, Some(StatusCode::CONFLICT));
    assert_eq!(
        approved.take_json::<Value>().await.unwrap()["code"],
        "coordinator_managed"
    );
    let state: String = rusqlite::Connection::open(f.root.path().join("state/domain.sqlite3"))
        .unwrap()
        .query_row(
            "SELECT state FROM engagements WHERE id=?1",
            [&pending],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "pending");
    f.close().await;
}

/// Scenario: the audit read lists the newest decisions newest-first with the
/// retained entry shape (backend-v2.js:14980-14983, listAudit
/// lib/engagement-store.js:804-806).
#[tokio::test]
async fn native_engagement_verdict_audit_lists_newest_first() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let service = f.service();
    // Ticket issuance is rate-limited to one per second.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let cookie = lifecycle_session(&service).await;
    let approved = f.new_engagement().await;
    let response = post(
        &format!("/console/api/engagements/{approved}/approve"),
        &cookie,
    )
    .json(&json!({"commandId": "cmd_audit_1"}))
    .send(&service)
    .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let refused = {
        // A DISTINCT engagement: the shared new_engagement helper replays one
        // fixed request id, so the second pending row is admitted directly
        // with its own id and agent name (admit's live-name collision guard).
        let pool = common::resource("private_usage_pool", "private_usage_seat", 1000);
        f.domain
            .admit(
                common::proof(&common::request(
                    "audit_refuse_request",
                    "AuditRefuseWorker",
                    &pool,
                    100,
                )),
                1000,
            )
            .await
            .unwrap()
            .id
    };
    let response = post(&format!("/console/api/agents/{refused}/refuse"), &cookie)
        .json(&json!({"commandId": "cmd_audit_2"}))
        .send(&service)
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));

    let mut listed = get("/console/api/engagements/audit", &cookie)
        .send(&service)
        .await;
    assert_eq!(listed.status_code, Some(StatusCode::OK));
    let body = listed.take_json::<Value>().await.unwrap();
    let audit = body["audit"].as_array().unwrap();
    assert!(audit.len() >= 2);
    // Newest first: the refusal is the most recent decision.
    assert_eq!(audit[0]["type"], "engagement.rejected");
    assert_eq!(audit[0]["engagementId"], refused);
    // Every entry carries the retained {type, at, ...detail} shape.
    for entry in audit {
        assert!(entry["at"].as_u64().unwrap() > 0);
        assert!(entry["engagementId"].is_string());
        assert!(entry["state"].is_string());
    }
    // The approval we drove is present with its retained word.
    assert!(
        audit.iter().any(|e| {
            e["type"] == "engagement.approved" && e["engagementId"] == approved.as_str()
        })
    );

    // The retained clamp (lib/engagement-store.js:805 Math.min(limit, 2000)):
    // an over-cap limit clamps, it is never a failure state TS did not have.
    let response = get("/console/api/engagements/audit?limit=2001", &cookie)
        .send(&service)
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    f.close().await;
}

/// Scenario: an unknown engagement answers 404, the retained
/// `'Engagement not found'` word.
#[tokio::test]
async fn native_engagement_verdict_unknown_engagement_is_not_found() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let service = f.service();
    // Ticket issuance is rate-limited to one per second (shared across the
    // concurrent console tests); one ticket here, reused for the read.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let cookie = lifecycle_session(&service).await;
    let response = post(
        "/console/api/engagements/en_does_not_exist/approve",
        &cookie,
    )
    .json(&json!({"commandId": "cmd_missing"}))
    .send(&service)
    .await;
    assert_eq!(response.status_code, Some(StatusCode::NOT_FOUND));
    // Reads stay scope-free: the same session may read candidates.
    let response = get(
        "/console/api/engagements/en_does_not_exist/candidates",
        &cookie,
    )
    .send(&service)
    .await;
    assert_eq!(response.status_code, Some(StatusCode::NOT_FOUND));
    f.close().await;
}
