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
    // ADR-193: Octos beside them, its keys assumed. Outside a fleet, where
    // Setup does not apply, its profiles are not read.
    assert_eq!(value["agents"][2]["kind"], json!("octos"));
    assert_eq!(value["agents"][2]["signInAssumed"], json!(true));
    assert!(value["agents"][2].get("profiles").is_none());
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

/// Scenario (ADR-193 decisions 7 and 8), on an imported fleet: Setup shows
/// Octos from the binary and home the runtime names, lists that home's
/// profiles with their primary models, and offers only the qualified
/// profiles the runtime allows. An offer creates an unpublished source that
/// runs the profile: framework octos, the profile's family as provider, its
/// model, and its ID. The Octos binary and home are fixture files, never the
/// user's.
#[tokio::test]
async fn native_setup_offers_the_allowed_octos_profiles() {
    let address = "127.0.0.1:13300".parse().unwrap();
    let f = Fixture::new(address, None);
    let state = f.root.path().join("state");
    let user = f.root.path().canonicalize().unwrap().join("octos-user");
    let home = user.join(".octos");
    std::fs::create_dir_all(home.join("profiles")).unwrap();
    let write = |id: &str, family: &str, model: &str| {
        std::fs::write(
            home.join("profiles").join(format!("{id}.json")),
            serde_json::to_vec(&json!({"id": id, "name": id, "config": {
                "llm": {"primary": {"family_id": family, "model_id": model}},
                "env_vars": {"SYNTHETIC_API_KEY": "synthetic-never-shown"}}}))
            .unwrap(),
        )
        .unwrap();
    };
    write("coding", "zai-coding", "glm-5.3-flash");
    // Qualified, but not one the runtime allows.
    write("kimi", "moonshot", "kimi-k3");
    // Not a model Hagency qualifies.
    write("other", "acme", "unknown-model");
    let binary = user.join("octos");
    std::fs::write(
        &binary,
        b"\xcf\xfa\xed\xfe fixture octos, never run as Octos",
    )
    .unwrap();
    std::fs::write(
        state.join("fleet-runtime.json"),
        serde_json::to_vec(&json!({"octos": {"executable": binary,
            "executable_sha256": "0".repeat(64),
            "local_octos": {"profile": "provider_owned_octos_v1", "preset": "local_octos",
                "seat": "local_octos_seat", "home": user, "octos_home": home,
                "profiles": ["coding"]}}}))
        .unwrap(),
    )
    .unwrap();
    let service = Service::new(
        f.app
            .clone()
            .with_palpo_fleet_import(state.clone(), address)
            .router(),
    );
    let cookie = lifecycle_session(&service).await;
    let mut answer = get("/console/api/setup", &cookie).send(&service).await;
    assert_eq!(answer.status_code, Some(StatusCode::OK));
    let value = answer.take_json::<Value>().await.unwrap();
    assert_eq!(value["applicable"], json!(true));
    assert!(!value.to_string().contains("synthetic-never-shown"));
    let octos = value["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["kind"] == "octos")
        .unwrap()
        .clone();
    assert_eq!(octos["found"], json!(true));
    assert_eq!(octos["path"], json!(binary));
    assert_eq!(octos["signInAssumed"], json!(true));
    assert_eq!(octos["signedIn"], json!(true));
    assert_eq!(
        octos["profiles"],
        json!([
            {"id": "coding", "family": "zai-coding", "model": "glm-5.3-flash", "tier": "medium"},
            {"id": "kimi", "family": "moonshot", "model": "kimi-k3", "tier": "strong"},
            {"id": "other", "family": "acme", "model": "unknown-model", "tier": null},
        ])
    );
    let offered: Vec<_> = value["offer"]["choices"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["framework"] == "octos")
        .cloned()
        .collect();
    assert_eq!(offered.len(), 1, "{offered:?}");
    assert_eq!(offered[0]["profile"], json!("coding"));
    assert_eq!(offered[0]["model"], json!("glm-5.3-flash"));
    assert_eq!(offered[0]["provider"], json!("zai-coding"));
    assert!(offered[0]["reasoning"].is_null());
    assert!(
        offered[0]["roles"]
            .as_array()
            .unwrap()
            .contains(&json!("coding"))
    );

    let offer = |body: Value| {
        let service = &service;
        let cookie = &cookie;
        async move {
            let mut answer = post("/console/api/setup/resource", cookie)
                .json(&body)
                .send(service)
                .await;
            let status = answer.status_code;
            (status, answer.take_json::<Value>().await.unwrap())
        }
    };
    // Refused: a profile the runtime does not allow, no profile, another
    // model than the profile's, a reasoning setting, a profile on Codex.
    for body in [
        json!({"framework": "octos", "model": "kimi-k3", "profile": "kimi"}),
        json!({"framework": "octos", "model": "glm-5.3-flash"}),
        json!({"framework": "octos", "model": "kimi-k3", "profile": "coding"}),
        json!({"framework": "octos", "model": "glm-5.3-flash", "profile": "coding",
            "reasoning": "high"}),
        json!({"framework": "codex", "model": "gpt-5.6-sol", "reasoning": "medium",
            "profile": "coding"}),
    ] {
        let (status, value) = offer(body.clone()).await;
        assert_eq!(status, Some(StatusCode::BAD_REQUEST), "{body}");
        assert_eq!(value["code"], json!("setup_unqualified_model"), "{body}");
    }
    // The user moved the profile to another model: it is not offered.
    write("coding", "deepseek", "deepseek-v-flash");
    let (status, _) =
        offer(json!({"framework": "octos", "model": "glm-5.3-flash", "profile": "coding"})).await;
    assert_eq!(status, Some(StatusCode::BAD_REQUEST));
    write("coding", "zai-coding", "glm-5.3-flash");
    let (status, value) = offer(json!({"framework": "octos", "model": "glm-5.3-flash",
        "profile": "coding", "tokens": 1000}))
    .await;
    assert_eq!(status, Some(StatusCode::OK), "{value}");
    let sources = f
        .domain
        .resource_configurations(String::new(), 64)
        .await
        .unwrap();
    let source = sources
        .iter()
        .find(|source| source.config.framework == "octos")
        .unwrap();
    assert_eq!(source.config.preset_id, "local_octos_coding");
    assert_eq!(source.config.seat_id, "local_octos_seat");
    assert_eq!(source.config.model, "glm-5.3-flash");
    assert_eq!(source.config.provider.as_deref(), Some("zai-coding"));
    assert_eq!(source.config.octos_profile.as_deref(), Some("coding"));
    assert!(source.config.reasoning.is_none());
    assert!(!source.config.published);
    // One source per profile.
    let (status, value) = offer(json!({"framework": "octos", "model": "glm-5.3-flash",
        "profile": "coding"}))
    .await;
    assert_eq!(status, Some(StatusCode::CONFLICT));
    assert_eq!(value["code"], json!("setup_source_exists"));
}
