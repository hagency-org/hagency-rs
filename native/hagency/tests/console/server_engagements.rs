use super::*;

fn contribute(path: &str, cookie: &str) -> salvo::test::RequestBuilder {
    TestClient::put(format!("{BASE}{path}"))
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("origin", BASE, true)
        .add_header("sec-fetch-site", "same-origin", true)
        .add_header("cookie", cookie, true)
}

#[tokio::test]
async fn native_console_engagement_pages_only_offer_an_existing_next_page() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let service = f.service();
    let cookie = session(&service).await;
    let mut empty = get("/console/api/server-engagements", &cookie)
        .send(&service)
        .await;
    let empty = empty.take_json::<Value>().await.unwrap();
    assert_eq!(empty["engagements"], json!([]));
    assert!(empty["nextCursor"].is_null());
    let fleet = common::registration().fleet_id;
    let now = now();
    for id in [fleet.to_string(), format!("hf_{}", "b".repeat(32))] {
        if id != fleet {
            let mut registration = common::registration();
            registration.fleet_id = id.clone();
            registration.representative_mxid = format!("@{id}_representative:example.test");
            registration.reception_room_id = "!second_reception:example.test".into();
            f.domain.register(registration).await.unwrap();
        }
        f.domain.configure_coordinator(serde_json::from_value(json!({"id":id,"server":"example.test","owner":"@provider:example.test","coordinator":"@coordinator:example.test",
            "registrationGeneration":1,"delegationRevision":1,"delegationExpiresAtMs":now+3600000,"state":"verified","allowSelfApproval":false,"coordinatorApprovalV1":true})).unwrap()).await.unwrap();
    }
    let resource_path = format!("/console/api/server-engagements/{fleet}/resources");
    for suffix in ["a", "b"] {
        let resource = native_resource(&format!("pagination_{suffix}"));
        f.domain.put_resource(resource.clone()).await.unwrap();
        let mut response = contribute(&resource_path, &cookie).json(&json!({"id":format!("grant_{suffix}"),"serverEngagementId":fleet,"resourceId":resource.id(),"revision":1,"allocatedTokens":100,"eligibleManagers":["@owner:example.test"]})).send(&service).await;
        let code = response.status_code;
        let body = response.take_json::<Value>().await.unwrap();
        assert_eq!(code, Some(StatusCode::OK), "{body}");
        let request_id = format!("request_{suffix}");
        let definition = common::request(&request_id, "PaginationAgent", &resource, 10);
        let digest =
            hagency_core::canonical::digest(&serde_json::to_value(&definition).unwrap()).unwrap();
        let mut payload = serde_json::to_value(definition).unwrap();
        payload["coordinatorApproval"] = json!({"context":{"version":1,"commandId":format!("command_{suffix}"),"serverEngagementId":fleet,"registrationGeneration":1,
            "delegationRevision":1,"actor":"@coordinator:example.test","issuedAtMs":now,"expiresAtMs":now+300000},
            "request":{"id":request_id,"revision":1,"serverEngagementId":fleet,"projectId":"project_one","projectRevision":1,"resourceAllocationId":format!("grant_{suffix}"),
            "projectOwner":"@owner:example.test","requester":"@owner:example.test","definitionDigest":digest,"requestedTokens":10},"allocatedTokens":10});
        assert_eq!(
            f.domain
                .receive_coordinator_agent(fleet.clone(), payload)
                .await
                .unwrap()["state"],
            "refused"
        );
    }
    for (path, key) in [
        ("/console/api/server-engagements".to_owned(), "engagements"),
        (resource_path, "resources"),
        (
            format!("/console/api/server-engagements/{fleet}/decisions"),
            "decisions",
        ),
    ] {
        let mut whole = get(&path, &cookie).send(&service).await;
        let whole = whole.take_json::<Value>().await.unwrap();
        assert_eq!(whole[key].as_array().unwrap().len(), 2, "{whole}");
        assert!(whole["nextCursor"].is_null());
        let mut first = get(&format!("{path}?limit=1"), &cookie)
            .send(&service)
            .await;
        let first = first.take_json::<Value>().await.unwrap();
        assert_eq!(first[key], json!([whole[key][0]]));
        assert_eq!(first["nextCursor"], whole[key][0]["id"]);
        let after = first["nextCursor"].as_str().unwrap();
        let mut last = get(&format!("{path}?limit=1&after={after}"), &cookie)
            .send(&service)
            .await;
        let last = last.take_json::<Value>().await.unwrap();
        assert_eq!(last[key], json!([whole[key][1]]));
        assert!(
            last["nextCursor"].is_null(),
            "a full final page must not offer an empty next page: {last}"
        );
    }
    f.close().await;
}

#[tokio::test]
async fn native_console_engagement_contribution_reserves_capacity_and_rejects_cross_binding() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let fleet = common::registration().fleet_id;
    let policy = json!({"id":fleet,"server":"example.test","owner":"@provider:example.test","coordinator":"@coordinator:example.test",
        "registrationGeneration":1,"delegationRevision":1,"delegationExpiresAtMs":now()+3600000,"state":"verified","allowSelfApproval":false,"coordinatorApprovalV1":true});
    f.domain
        .configure_coordinator(serde_json::from_value(policy).unwrap())
        .await
        .unwrap();
    let parent = native_resource("contribution_parent");
    f.domain.put_resource(parent.clone()).await.unwrap();
    let service = f.service();
    let path = format!("/console/api/server-engagements/{fleet}/resources");
    let input = json!({"id":"engagement_grant","serverEngagementId":fleet,"resourceId":parent.id(),"revision":1,"allocatedTokens":3000,"eligibleManagers":["@manager:example.test"]});
    assert_eq!(
        contribute(&path, "")
            .json(&input)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    let cookie = session(&service).await;
    let mut changed = input.clone();
    changed["serverEngagementId"] = json!("wrong_engagement");
    assert_eq!(
        contribute(&path, &cookie)
            .json(&changed)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    for _ in 0..2 {
        let mut response = contribute(&path, &cookie).json(&input).send(&service).await;
        let code = response.status_code;
        let details = response.take_json::<Value>().await.unwrap();
        assert_eq!(code, Some(StatusCode::OK), "{details}");
    }
    let mut response = get(&path, &cookie).send(&service).await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let rows = response.take_json::<Value>().await.unwrap();
    assert_eq!(rows["resources"][0]["allocatedTokens"], 3000);
    assert_eq!(rows["resources"][0]["remainingTokens"], 3000);
    assert!(rows["nextCursor"].is_null());
    assert!(!rows.to_string().contains("private_resource_account"));
    let mut increased = input.clone();
    increased["revision"] = json!(2);
    increased["allocatedTokens"] = json!(5001);
    let mut refused = contribute(&path, &cookie)
        .json(&increased)
        .send(&service)
        .await;
    assert_eq!(refused.status_code, Some(StatusCode::CONFLICT));
    assert_eq!(
        refused.take_json::<Value>().await.unwrap()["code"],
        "contribution_capacity_exceeded"
    );
    increased["allocatedTokens"] = json!(4500);
    assert_eq!(
        contribute(&path, &cookie)
            .json(&increased)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(
        get(&format!("{path}?limit=abc"), &cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    assert_eq!(
        get(&format!("{path}?limit=51"), &cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    let mut list = get("/console/api/server-engagements", &cookie)
        .send(&service)
        .await;
    assert_eq!(
        list.take_json::<Value>().await.unwrap()["engagements"][0]["state"],
        "verified"
    );
    f.close().await;
}

#[tokio::test]
async fn native_console_delegation_suspend_is_authorized_idempotent_and_does_not_release_capacity()
{
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let fleet = common::registration().fleet_id;
    f.domain.configure_coordinator(serde_json::from_value(json!({"id":fleet,"server":"example.test","owner":"@provider:example.test","coordinator":"@coordinator:example.test","registrationGeneration":1,"delegationRevision":1,"delegationExpiresAtMs":now()+3600000,"state":"verified","allowSelfApproval":false,"coordinatorApprovalV1":true})).unwrap()).await.unwrap();
    let parent = native_resource("suspension_parent");
    f.domain.put_resource(parent.clone()).await.unwrap();
    let service = f.service();
    let path = format!("/console/api/server-engagements/{fleet}/delegation");
    let change = json!({"serverEngagementId":fleet,"expectedRevision":1,"coordinatorMxid":"@coordinator:example.test","delegationExpiresAtMs":now()+3600000,"allowSelfApproval":false,"state":"suspended","exportMxids":[]});
    assert_eq!(
        contribute(&path, "")
            .json(&change)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    let cookie = session(&service).await;
    let resources = format!("/console/api/server-engagements/{fleet}/resources");
    let grant = json!({"id":"suspension_grant","serverEngagementId":fleet,"resourceId":parent.id(),"revision":1,"allocatedTokens":3000,"eligibleManagers":["@owner:example.test"]});
    assert_eq!(
        contribute(&resources, &cookie)
            .json(&grant)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    for _ in 0..2 {
        let mut response = contribute(&path, &cookie)
            .json(&change)
            .send(&service)
            .await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        let value = response.take_json::<Value>().await.unwrap();
        assert_eq!(value["engagement"]["delegationRevision"], 2);
        assert_eq!(value["publication"], "queued");
    }
    let mut conflict = change.clone();
    conflict["state"] = json!("revoked");
    assert_eq!(
        contribute(&path, &cookie)
            .json(&conflict)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::CONFLICT)
    );
    assert_eq!(
        f.domain
            .coordinator_authority(fleet)
            .await
            .unwrap()
            .unwrap()
            .delegation_revision,
        2.try_into().unwrap()
    );
    let mut response = get(&resources, &cookie).send(&service).await;
    assert_eq!(
        response.take_json::<Value>().await.unwrap()["resources"][0]["allocatedTokens"],
        3000
    );
    f.close().await;
}
