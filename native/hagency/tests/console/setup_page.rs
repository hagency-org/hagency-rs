use super::*;

// ADR-189: the console's setup page. The fixture host has no imported fleet,
// so the status reads and the configure step is refused as not-a-fleet.

/// Scenario: the setup status lists the coding agents found on this machine
/// and the state of each step; it needs a console session.
#[tokio::test]
async fn native_setup_status_reports_agents_and_steps() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let state = f.root.path().join("state");
    let service = Service::new(f.app.clone().with_palpo_import(state).router());
    let anonymous = TestClient::get(format!("{BASE}/console/api/setup"))
        .add_header("host", "127.0.0.1:13300", true)
        .send(&service)
        .await;
    assert_eq!(anonymous.status_code, Some(StatusCode::UNAUTHORIZED));

    let cookie = lifecycle_session(&service).await;
    let mut answer = get("/console/api/setup", &cookie).send(&service).await;
    assert_eq!(answer.status_code, Some(StatusCode::OK));
    let value = answer.take_json::<Value>().await.unwrap();
    assert_eq!(value["ok"], json!(true));
    // The fixture is not an imported fleet: the banner stays hidden.
    assert_eq!(value["applicable"], json!(false));
    assert_eq!(value["agents"][0]["kind"], json!("codex"));
    assert!(value["agents"][0]["found"].is_boolean());
    assert!(value["agents"][0]["signedIn"].is_boolean());
    assert_eq!(value["agents"][0]["signInAssumed"], json!(false));
    // ADR-192: Claude Code beside Codex; its sign-in is assumed, never checked.
    assert_eq!(value["agents"][1]["kind"], json!("claude"));
    assert!(value["agents"][1]["found"].is_boolean());
    assert_eq!(value["agents"][1]["signInAssumed"], json!(true));
    // Each choice names its coding agent; Claude choices carry no reasoning.
    // The fixture already holds a Codex source; Claude Code can still be
    // offered, so the page keeps its offer form for Claude choices.
    assert_eq!(value["offer"]["sourceFrameworks"], json!(["codex"]));
    let choices = value["offer"]["choices"].as_array().unwrap();
    assert!(choices.iter().any(|c| c["framework"] == "codex"));
    let claude: Vec<_> = choices
        .iter()
        .filter(|c| c["framework"] == "claude")
        .collect();
    assert!(!claude.is_empty());
    assert!(claude.iter().all(|c| c["reasoning"].is_null()));
    assert_eq!(value["runtimeConfigured"], json!(false));
    assert_eq!(value["palpo"]["imported"], json!(false));
}

/// Scenario: configuring the runtime is a fleet step; a host that is not an
/// imported fleet refuses it by name and writes nothing.
#[tokio::test]
async fn native_setup_check_is_refused_outside_a_fleet() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let state = f.root.path().join("state");
    let service = Service::new(f.app.clone().with_palpo_import(state.clone()).router());
    let cookie = lifecycle_session(&service).await;
    let mut refused = post("/console/api/setup/check", &cookie)
        .send(&service)
        .await;
    assert_eq!(refused.status_code, Some(StatusCode::CONFLICT));
    let value = refused.take_json::<Value>().await.unwrap();
    assert_eq!(value["code"], json!("setup_not_fleet"));
    assert!(!state.join("fleet-runtime.json").exists());
}

/// Scenario: offering the first resource is a fleet step; outside a fleet it
/// is refused by name and no resource is created.
#[tokio::test]
async fn native_setup_offer_is_refused_outside_a_fleet() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let state = f.root.path().join("state");
    let service = Service::new(f.app.clone().with_palpo_import(state).router());
    let cookie = lifecycle_session(&service).await;
    let mut before = get("/console/api/setup", &cookie).send(&service).await;
    let before = before.take_json::<Value>().await.unwrap()["offer"]["resources"].clone();
    let mut refused = post("/console/api/setup/resource", &cookie)
        .json(&json!({"model": "gpt-5.6-sol", "reasoning": "medium"}))
        .send(&service)
        .await;
    assert_eq!(refused.status_code, Some(StatusCode::CONFLICT));
    let value = refused.take_json::<Value>().await.unwrap();
    assert_eq!(value["code"], json!("setup_not_fleet"));
    let mut status = get("/console/api/setup", &cookie).send(&service).await;
    let value = status.take_json::<Value>().await.unwrap();
    assert_eq!(
        value["offer"]["resources"], before,
        "no resource was created"
    );
    assert!(!value["offer"]["choices"].as_array().unwrap().is_empty());
}
