use super::*;

fn contribute(path: &str, cookie: &str) -> salvo::test::RequestBuilder {
    TestClient::put(format!("{BASE}{path}"))
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("origin", BASE, true)
        .add_header("sec-fetch-site", "same-origin", true)
        .add_header("cookie", cookie, true)
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
    assert_eq!(rows["nextCursor"], "engagement_grant");
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
