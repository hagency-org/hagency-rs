use super::*;

// ADR-197 console surface: engagement upgrade status, pin/unpin, rebind,
// rollback and the resource rollout, backed by the store's own guards.

const FROZEN: &str =
    r#"{"pluginId":"p","adapterId":"a","packageSha256":"aa","settingsSha256":"bb"}"#;
const TARGET: &str =
    r#"{"pluginId":"p","adapterId":"a","packageSha256":"cc","settingsSha256":"bb"}"#;

/// Freeze a runtime plugin on the seeded engagement's provision effect.
fn freeze_runtime(f: &Fixture) -> rusqlite::Connection {
    let db = rusqlite::Connection::open(f.root.path().join("state/domain.sqlite3")).unwrap();
    db.execute(
        "UPDATE effects SET payload=json_set(payload,'$.resource.runtimePlugin',json(?2)) WHERE engagement_id=?1 AND kind='provision'",
        rusqlite::params![f.engagement, FROZEN],
    )
    .unwrap();
    db
}

/// Mark the engagement `upgrade_pending` toward `TARGET`.
fn mark_pending(db: &rusqlite::Connection, f: &Fixture, resource: &str) {
    db.execute(
        "INSERT INTO engagement_runtime_upgrades(engagement_id,resource_id,state,target_runtime,marked_at,updated_at) VALUES(?1,?2,'upgrade_pending',?3,1,1)",
        rusqlite::params![f.engagement, resource, TARGET],
    )
    .unwrap();
}

fn up(id: &str, tail: &str) -> String {
    format!("/console/api/engagements/{id}/upgrade{tail}")
}

#[tokio::test]
async fn console_engagement_upgrade_routes_require_operator_auth() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let service = f.service();
    let anonymous = TestClient::get(format!("{BASE}{}", up(&f.engagement, "")))
        .add_header("host", "127.0.0.1:13300", true)
        .send(&service)
        .await;
    assert_eq!(anonymous.status_code, Some(StatusCode::UNAUTHORIZED));
    for path in [
        up(&f.engagement, "/pin"),
        up(&f.engagement, "/rebind"),
        "/console/api/resources/any/rollout/start".to_string(),
    ] {
        let anonymous = TestClient::post(format!("{BASE}{path}"))
            .add_header("host", "127.0.0.1:13300", true)
            .add_header("origin", BASE, true)
            .send(&service)
            .await;
        assert_eq!(
            anonymous.status_code,
            Some(StatusCode::UNAUTHORIZED),
            "{path}"
        );
    }
    // Without the console origin a signed-in session is still refused.
    let cookie = lifecycle_session(&service).await;
    let foreign = TestClient::post(format!("{BASE}{}", up(&f.engagement, "/pin")))
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("origin", "http://evil.example", true)
        .add_header("cookie", cookie, true)
        .send(&service)
        .await;
    assert_ne!(foreign.status_code, Some(StatusCode::OK));
    f.close().await;
}

#[tokio::test]
async fn console_engagement_upgrade_pin_unpin_and_status() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    freeze_runtime(&f);
    let service = f.service();
    let cookie = lifecycle_session(&service).await;
    let mut status = get(&up(&f.engagement, ""), &cookie).send(&service).await;
    assert_eq!(status.status_code, Some(StatusCode::OK));
    let body = status.take_json::<Value>().await.unwrap();
    assert_eq!(body["engagementId"], f.engagement);
    assert!(body["state"].is_null());
    let mut pinned = post(&up(&f.engagement, "/pin"), &cookie)
        .send(&service)
        .await;
    assert_eq!(pinned.status_code, Some(StatusCode::OK));
    let audit = pinned.take_json::<Value>().await.unwrap();
    assert_eq!(audit["action"], "pin");
    assert_eq!(audit["actor"], "console_operator");
    let mut status = get(&up(&f.engagement, ""), &cookie).send(&service).await;
    let body = status.take_json::<Value>().await.unwrap();
    assert_eq!(body["state"], "pinned");
    let mut refused = post(&up(&f.engagement, "/rebind"), &cookie)
        .send(&service)
        .await;
    assert_eq!(refused.status_code, Some(StatusCode::CONFLICT));
    assert_eq!(
        refused.take_json::<Value>().await.unwrap()["code"],
        "pinned"
    );
    let unpinned = post(&up(&f.engagement, "/unpin"), &cookie)
        .send(&service)
        .await;
    assert_eq!(unpinned.status_code, Some(StatusCode::OK));
    let again = post(&up(&f.engagement, "/unpin"), &cookie)
        .send(&service)
        .await;
    assert_eq!(again.status_code, Some(StatusCode::CONFLICT));
    f.close().await;
}

#[tokio::test]
async fn console_engagement_upgrade_refusal_reasons() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let service = f.service();
    let cookie = lifecycle_session(&service).await;
    let mut rebind = post(&up(&f.engagement, "/rebind"), &cookie)
        .send(&service)
        .await;
    assert_eq!(rebind.status_code, Some(StatusCode::CONFLICT));
    let body = rebind.take_json::<Value>().await.unwrap();
    assert_eq!(body["code"], "not_marked");
    assert!(body["message"].as_str().unwrap().contains("no pending"));
    let mut rollback = post(&up(&f.engagement, "/rollback"), &cookie)
        .send(&service)
        .await;
    assert_eq!(rollback.status_code, Some(StatusCode::CONFLICT));
    assert_eq!(
        rollback.take_json::<Value>().await.unwrap()["code"],
        "no_previous"
    );
    // No frozen runtime plugin: pin is a store state conflict, not a 500.
    let mut pin = post(&up(&f.engagement, "/pin"), &cookie)
        .send(&service)
        .await;
    assert_eq!(pin.status_code, Some(StatusCode::CONFLICT));
    assert_eq!(
        pin.take_json::<Value>().await.unwrap()["code"],
        "upgrade_state_conflict"
    );
    let missing = get(&up("engagement_nonexistent", ""), &cookie)
        .send(&service)
        .await;
    assert_eq!(missing.status_code, Some(StatusCode::NOT_FOUND));
    let body = post(&up(&f.engagement, "/pin"), &cookie)
        .json(&json!({"x": 1}))
        .send(&service)
        .await;
    assert_eq!(body.status_code, Some(StatusCode::BAD_REQUEST));
    f.close().await;
}

#[tokio::test]
async fn console_resource_rollout_start_and_status() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let db = freeze_runtime(&f);
    let service = f.service();
    let cookie = lifecycle_session(&service).await;
    let mut status = get(&up(&f.engagement, ""), &cookie).send(&service).await;
    let resource = status.take_json::<Value>().await.unwrap()["resourceId"]
        .as_str()
        .unwrap()
        .to_string();
    // Nothing is marked yet: a rollout has no target to start.
    let none = post(
        &format!("/console/api/resources/{resource}/rollout/start"),
        &cookie,
    )
    .send(&service)
    .await;
    assert_eq!(none.status_code, Some(StatusCode::NOT_FOUND));
    mark_pending(&db, &f, &resource);
    let rollout = |tail: &str| format!("/console/api/resources/{resource}/rollout{tail}");
    let mut idle = get(&rollout(""), &cookie).send(&service).await;
    assert_eq!(idle.status_code, Some(StatusCode::OK));
    assert_eq!(idle.take_json::<Value>().await.unwrap()["state"], "none");
    let started = post(&rollout("/start"), &cookie).send(&service).await;
    assert_eq!(started.status_code, Some(StatusCode::OK));
    let mut running = get(&rollout(""), &cookie).send(&service).await;
    let body = running.take_json::<Value>().await.unwrap();
    assert_eq!(body["state"], "running");
    assert_eq!(body["pending"][0]["engagementId"], f.engagement);
    // The seeded engagement has a running dispatch: refused by name.
    let mut refused = post(&up(&f.engagement, "/rebind"), &cookie)
        .send(&service)
        .await;
    assert_eq!(refused.status_code, Some(StatusCode::CONFLICT));
    assert_eq!(
        refused.take_json::<Value>().await.unwrap()["code"],
        "dispatch_started"
    );
    let bad = post(&rollout("/canary"), &cookie)
        .json(&json!({"detail": "no verdict"}))
        .send(&service)
        .await;
    assert_eq!(bad.status_code, Some(StatusCode::BAD_REQUEST));
    f.close().await;
}
