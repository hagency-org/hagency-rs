use super::*;

// The console import of the Palpo owner download (TS parity: the "import Palpo
// authorized configuration" step of `mockup/app/projects/new/page.jsx`). The
// fixture's transport is not enabled, so the route saves without connecting;
// the start itself is the bootstrap handle's, exercised live.

fn fleet() -> String {
    format!("hf_{}", "c".repeat(32))
}
fn download(fleet: &str) -> Value {
    json!({"fleetId": fleet, "serverName": "example.test", "credentialVersion": 1,
        "registration": {"id": fleet, "url": "http://127.0.0.1:18080/api/relay/v2/x",
            "as_token": "as-token-value-secret", "hs_token": "hs-token-value-secret",
            "sender_localpart": format!("{fleet}_representative"),
            "namespaces": {"users": [{"exclusive": true, "regex": format!("^@{fleet}_[a-z0-9_]+:example\\.test$")}],
                "aliases": [], "rooms": []}, "rate_limited": true, "receive_ephemeral": false},
        "transport": {"mode": "outbound", "url": format!("https://palpo.example/api/fleet/v2/{fleet}"),
            "token": "machine-token-secret-0123456789", "generation": 1}})
}
fn body(configuration: &Value) -> Value {
    json!({"configuration": configuration.to_string(), "homeserver": "https://matrix.example.test"})
}

/// Scenario: the owner download is saved through the console — fleet row,
/// transport files and the project side's credential — and the answer carries
/// public facts only.
#[tokio::test]
async fn native_palpo_import_route_saves_the_owner_download() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let state = f.root.path().join("state");
    let service = Service::new(f.app.clone().with_palpo_import(state.clone()).router());
    let fleet = fleet();
    let anonymous = TestClient::post(format!("{BASE}/console/api/palpo/import"))
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("origin", BASE, true)
        .add_header("sec-fetch-site", "same-origin", true)
        .json(&body(&download(&fleet)))
        .send(&service)
        .await;
    assert_eq!(anonymous.status_code, Some(StatusCode::UNAUTHORIZED));
    assert!(
        !state.join("palpo-transport.json").exists(),
        "an anonymous import writes nothing"
    );

    let cookie = lifecycle_session(&service).await;
    let mut saved = post("/console/api/palpo/import", &cookie)
        .json(&body(&download(&fleet)))
        .send(&service)
        .await;
    assert_eq!(saved.status_code, Some(StatusCode::OK));
    let answer = saved.take_json::<Value>().await.unwrap();
    assert_eq!(answer["ok"], json!(true));
    assert_eq!(answer["fleetId"], json!(fleet));
    assert_eq!(answer["serverName"], json!("example.test"));
    assert_eq!(
        answer["representative"],
        json!(format!("@{fleet}_representative:example.test"))
    );
    assert_eq!(
        answer["receptionBound"],
        json!(false),
        "Palpo's probe binds the reception later"
    );
    assert_eq!(
        answer["started"],
        json!(false),
        "the fixture's transport is not enabled"
    );
    let text = answer.to_string();
    for secret in [
        "as-token-value-secret",
        "hs-token-value-secret",
        "machine-token-secret-0123456789",
    ] {
        assert!(!text.contains(secret), "the answer never carries a token");
    }
    // The three files the transport reads, and the fleet row.
    let transport: Value =
        serde_json::from_slice(&std::fs::read(state.join("palpo-transport.json")).unwrap())
            .unwrap();
    assert_eq!(transport["registration"]["fleetId"], json!(fleet));
    assert_eq!(
        std::fs::read_to_string(state.join("palpo.machine_token")).unwrap(),
        "machine-token-secret-0123456789"
    );
    let appservice: Value =
        serde_json::from_slice(&std::fs::read(state.join("palpo-appservice.json")).unwrap())
            .unwrap();
    assert_eq!(
        appservice["homeserver"],
        json!("https://matrix.example.test")
    );
    let db = rusqlite::Connection::open(state.join("domain.sqlite3")).unwrap();
    let rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM registrations WHERE fleet_id=?1",
            [&fleet],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);
    // The project side is listed and holds the App Service credential, so the
    // console's own verify (whoami as the representative) can run.
    let mut side = get("/console/api/project-sides/example.test", &cookie)
        .send(&service)
        .await;
    assert_eq!(side.status_code, Some(StatusCode::OK));
    let side = side.take_json::<Value>().await.unwrap().to_string();
    assert!(
        side.contains("matrix.example.test"),
        "the side records the homeserver: {side}"
    );
    assert!(
        !side.contains("as-token-value-secret"),
        "the side view never carries a token"
    );

    // A second engagement on the same server keeps separate credentials.
    let again = post("/console/api/palpo/import", &cookie)
        .json(&body(&download(&fleet)))
        .send(&service)
        .await;
    assert_eq!(again.status_code, Some(StatusCode::OK));
    let second = format!("hf_{}", "d".repeat(32));
    let mut second_download = download(&second);
    second_download["registration"]["as_token"] = json!("second-as-secret");
    second_download["transport"]["token"] = json!("second-machine-secret-0123456789");
    let other = post("/console/api/palpo/import", &cookie)
        .json(&body(&second_download))
        .send(&service)
        .await;
    assert_eq!(other.status_code, Some(StatusCode::OK));
    let secondary = state.join("palpo-engagements").join(&second);
    assert_eq!(
        std::fs::read_to_string(secondary.join("palpo.machine_token")).unwrap(),
        "second-machine-secret-0123456789"
    );
    assert_eq!(
        std::fs::read_to_string(state.join("palpo.machine_token")).unwrap(),
        "machine-token-secret-0123456789"
    );
    let first: Value =
        serde_json::from_slice(&std::fs::read(state.join("palpo-appservice.json")).unwrap())
            .unwrap();
    assert_eq!(first["as_token"], "as-token-value-secret");
    let rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM registrations WHERE fleet_id IN (?1,?2)",
            [&fleet, &second],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 2);
    f.close().await;
}

/// Scenario: what the CLI importer refuses, the route refuses before writing,
/// and names the field — never the value.
#[tokio::test]
async fn native_palpo_import_route_refuses_a_foreign_file() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let state = f.root.path().join("state");
    let service = Service::new(f.app.clone().with_palpo_import(state.clone()).router());
    let cookie = lifecycle_session(&service).await;
    let mut callback = download(&fleet());
    callback.as_object_mut().unwrap().remove("transport");
    let mut refused = post("/console/api/palpo/import", &cookie)
        .json(&body(&callback))
        .send(&service)
        .await;
    assert_eq!(refused.status_code, Some(StatusCode::BAD_REQUEST));
    let answer = refused.take_json::<Value>().await.unwrap();
    assert_eq!(answer["code"], json!("palpo_import_invalid"));
    assert!(answer["field"].as_str().unwrap().starts_with("transport"));
    let mut plain = body(&download(&fleet()));
    plain["homeserver"] = json!("http://matrix.example.test");
    let refused = post("/console/api/palpo/import", &cookie)
        .json(&plain)
        .send(&service)
        .await;
    assert_eq!(refused.status_code, Some(StatusCode::BAD_REQUEST));
    assert!(
        !state.join("palpo-transport.json").exists(),
        "a refused import writes nothing"
    );
    f.close().await;
}
