use super::*;
use hagency_store::resource_publication_revision;
fn write(path: &str, cookie: &str, edit: bool) -> salvo::test::RequestBuilder {
    let request = if edit {
        TestClient::patch(format!("{BASE}{path}"))
    } else {
        TestClient::post(format!("{BASE}{path}"))
    };
    request
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("origin", BASE, true)
        .add_header("sec-fetch-site", "same-origin", true)
        .add_header("cookie", cookie, true)
}
fn logout(cookie: &str) -> salvo::test::RequestBuilder {
    TestClient::delete(format!("{BASE}/console/session"))
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("origin", BASE, true)
        .add_header("sec-fetch-site", "same-origin", true)
        .add_header("cookie", cookie, true)
}
fn changes(r: &hagency_core::project::Resource) -> Value {
    json!({"expectedRevision":resource_publication_revision(r).unwrap(),"profileChange":{"kind":"preserve"},"ceilingChange":{"kind":"monthly","tokens":321}})
}
#[tokio::test]
async fn native_console_resource_configuration_authority() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let source = native_resource("private_configuration_authority");
    f.domain.put_resource(source.clone()).await.unwrap();
    let service = f.service();
    // TS parity: ONE login carries every action — the same session writes
    // configuration AND publication; no second link, no issuance wait.
    let operator = session(&service).await;
    let path = format!("/console/api/resources/{}/configuration", source.id());
    let input = changes(&source);
    // The scoped issue route is gone: one login is the whole console.
    assert_eq!(
        TestClient::post(format!(
            "{BASE}/api/native/v1/console/resource-configuration-access"
        ))
        .add_header("host", "127.0.0.1:13300", true)
        .bearer_auth(TOKEN)
        .send(&service)
        .await
        .status_code,
        Some(StatusCode::NOT_FOUND)
    );
    for key in [
        "presetId",
        "seatId",
        "framework",
        "provider",
        "name",
        "executionPolicy",
        "scope",
    ] {
        let mut malformed = input.clone();
        malformed[key] = json!("untrusted");
        assert_eq!(
            write(&path, &operator, true)
                .json(&malformed)
                .send(&service)
                .await
                .status_code,
            Some(StatusCode::BAD_REQUEST)
        );
    }
    assert_eq!(
        write(&path, &operator, true)
            .json(&input)
            .add_header("origin", "https://outside.invalid", true)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    let lock = rusqlite::Connection::open(f.root.path().join("state/domain.sqlite3")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut operation = Box::pin(write(&path, &operator, true).json(&input).send(&service));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(operation.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    // A pending command keeps logout busy (busy is not revocation).
    let busy = logout(&operator).send(&service).await;
    assert_eq!(busy.status_code, Some(StatusCode::TOO_MANY_REQUESTS));
    assert!(!busy.headers().contains_key("set-cookie"));
    lock.execute_batch("COMMIT").unwrap();
    assert_eq!(operation.await.status_code, Some(StatusCode::OK));
    // The write advanced the revision: replaying the SAME body is the
    // store's conflict word, never a scope word.
    assert_eq!(
        write(&path, &operator, true)
            .json(&input)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::CONFLICT)
    );
    assert_eq!(
        logout(&operator).send(&service).await.status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(
        write(&path, &operator, true)
            .json(&input)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    f.close().await;
}
#[tokio::test]
async fn native_console_resource_configuration_observations() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let mut resource = native_resource("private_editor_exact_source");
    resource.published = false;
    resource.ceiling = Some(serde_json::from_value(json!({"tokens":null})).unwrap());
    f.domain.put_resource(resource.clone()).await.unwrap();
    let service = f.service();
    let cookie = session(&service).await;
    let path = format!("/console/api/resources/{}/configuration", resource.id());
    let mut response = get(&path, &cookie).send(&service).await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let value = response.take_json::<Value>().await.unwrap();
    assert_eq!(value["resource"]["id"], resource.id());
    assert_eq!(value["resource"]["published"], false);
    assert!(value["resource"]["ceiling"].get("period").is_none());
    assert!(value["choices"].as_array().unwrap().len() <= 256);
    for private in [
        "private_editor_exact_source",
        "private_resource_account",
        "presetId",
        "seatId",
        "authHome",
        "apiKey",
        "operator.token",
    ] {
        assert!(!value.to_string().contains(private));
    }
    let mut list = get("/console/api/resources?limit=1", &cookie)
        .send(&service)
        .await;
    assert_eq!(
        list.take_json::<Value>().await.unwrap()["permissions"],
        json!({"publishResource":true,"configureResource":true})
    );
    for query in ["a=b", "resource_id=x", "limit=2&limit=3"] {
        assert_eq!(
            get(&format!("{path}?{query}"), &cookie)
                .send(&service)
                .await
                .status_code,
            Some(StatusCode::BAD_REQUEST)
        );
    }
    resource.model = "unknown-original-model".into();
    resource.reasoning = Some("unknown-original-reasoning".into());
    f.domain.put_resource(resource.clone()).await.unwrap();
    let mut response = get(&path, &cookie).send(&service).await;
    let value = response.take_json::<Value>().await.unwrap();
    assert_eq!(value["resource"]["model"], resource.model);
    assert!(value["modelTier"].is_null());
    for query in [
        "resource_id=x&source_resource_id=y",
        "source_resource_id=x&source_resource_id=y",
        "unknown=x",
    ] {
        assert_eq!(
            get(&format!("/console/resources/new/?{query}"), &cookie)
                .send(&service)
                .await
                .status_code,
            Some(StatusCode::BAD_REQUEST)
        );
    }
    let lock = rusqlite::Connection::open(f.root.path().join("state/domain.sqlite3")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut ahead = Box::pin(f.domain.put_resource(resource));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(ahead.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    let mut waiting = Box::pin(get(&path, &cookie).send(&service));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    f.console.retire();
    lock.execute_batch("COMMIT").unwrap();
    ahead.await.unwrap();
    assert_eq!(
        waiting.await.status_code,
        Some(StatusCode::SERVICE_UNAVAILABLE)
    );
    f.close().await;
}

#[tokio::test]
async fn native_console_resource_configuration_saves_engagement_and_model_together() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let source = native_resource("unified_configuration_source");
    f.domain.put_resource(source.clone()).await.unwrap();
    let fleet = common::registration().fleet_id;
    f.domain.configure_coordinator(serde_json::from_value(json!({"id":fleet,"server":"example.test","owner":"@provider:example.test","coordinator":"@coordinator:example.test","registrationGeneration":1,"delegationRevision":1,"delegationExpiresAtMs":now()+3600000,"state":"verified","allowSelfApproval":false,"coordinatorApprovalV1":true})).unwrap()).await.unwrap();
    let service = f.service();
    let cookie = session(&service).await;
    let mut input = changes(&source);
    input["sourceResourceId"] = json!(source.id());
    input["engagement"] = json!({"serverEngagementId":fleet,"allocationId":null,"expectedRevision":null,"eligibleManagers":["@owner:example.test"]});
    assert_eq!(
        write("/console/api/resources", "", false)
            .json(&input)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    let mut response = write("/console/api/resources", &cookie, false)
        .json(&input)
        .send(&service)
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let saved = response.take_json::<Value>().await.unwrap();
    let id = saved["resourceId"].as_str().unwrap();
    let mut read = get(
        &format!("/console/api/resources/{id}/configuration"),
        &cookie,
    )
    .send(&service)
    .await;
    let observation = read.take_json::<Value>().await.unwrap();
    assert_eq!(observation["resource"]["ceiling"]["tokens"], 321);
    assert_eq!(
        observation["resource"]["engagementResources"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let binding = &observation["resource"]["engagementResources"][0];
    assert_eq!(binding["serverEngagementId"], fleet);
    assert_eq!(binding["allocatedTokens"], 321);
    let current = f.domain.resource_configuration(id.into()).await.unwrap();
    let mut edit = changes(&current);
    edit["ceilingChange"]["tokens"] = json!(400);
    edit["engagement"] = json!({"serverEngagementId":fleet,"allocationId":binding["id"],"expectedRevision":1,"eligibleManagers":["@owner:example.test"]});
    let path = format!("/console/api/resources/{id}/configuration");
    assert_eq!(
        write(&path, &cookie, true)
            .json(&edit)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(
        write(&path, &cookie, true)
            .json(&edit)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::CONFLICT)
    );
    let grants = f
        .domain
        .server_engagement_resources(fleet, String::new(), 50)
        .await
        .unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0]["id"], binding["id"]);
    assert_eq!(grants[0]["revision"], 2);
    assert_eq!(grants[0]["allocatedTokens"], 400);
    f.close().await;
}
