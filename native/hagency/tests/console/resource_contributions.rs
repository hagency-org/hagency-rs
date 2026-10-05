use super::*;
use hagency_store::resource_publication_revision;

fn post(path: &str, cookie: &str) -> salvo::test::RequestBuilder {
    TestClient::post(format!("{BASE}{path}"))
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("origin", BASE, true)
        .add_header("sec-fetch-site", "same-origin", true)
        .add_header("cookie", cookie, true)
}
#[tokio::test]
async fn native_console_contribution_reserves_once_and_revokes_without_refunding() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let resource = native_resource("contribution_resource");
    f.domain.put_resource(resource.clone()).await.unwrap();
    let service = f.service();
    let cookie = session(&service).await;
    let registration = common::registration();
    let mut targets = get("/console/api/palpo/contribution-targets", &cookie)
        .send(&service)
        .await;
    assert_eq!(targets.status_code, Some(StatusCode::OK));
    let targets: Value = targets.take_json().await.unwrap();
    assert_eq!(targets["targets"][0]["fleetId"], registration.fleet_id);
    assert_eq!(targets["targets"][0]["issuer"], registration.server_name);
    assert_eq!(
        targets["targets"][0]["registrationGeneration"],
        registration.generation
    );
    assert_private(&targets);
    let path = format!("/console/api/resources/{}/contributions", resource.id());
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 86400000;
    let input = json!({"requestId":"console_contribution","expectedResourceRevision":resource_publication_revision(&resource).unwrap(),
        "fleetId":registration.fleet_id,"registrationGeneration":registration.generation,
        "limits":{"tokens":400,"maxAgents":4,"maxRatePerDay":100},"expiresAtMs":expires});
    assert_eq!(
        post(&path, "")
            .json(&input)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    let mut first = post(&path, &cookie).json(&input).send(&service).await;
    assert_eq!(first.status_code, Some(StatusCode::OK));
    let first: Value = first.take_json().await.unwrap();
    assert_eq!(first["state"], "active");
    assert_eq!(first["grant"]["limits"]["tokens"], 400);
    let mut replay = post(&path, &cookie).json(&input).send(&service).await;
    assert_eq!(replay.status_code, Some(StatusCode::OK));
    assert_eq!(replay.take_json::<Value>().await.unwrap(), first);
    let mut changed = input.clone();
    changed["limits"]["tokens"] = json!(401);
    assert_eq!(
        post(&path, &cookie)
            .json(&changed)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::CONFLICT)
    );
    let mut page = get(&path, &cookie).send(&service).await;
    assert_eq!(page.status_code, Some(StatusCode::OK));
    let page: Value = page.take_json().await.unwrap();
    assert_eq!(page["contributions"].as_array().unwrap().len(), 1);
    assert_private(&page);
    for secret in [
        "private_resource_account",
        "presetId",
        "seatId",
        "as_token",
        "hs_token",
    ] {
        assert!(!page.to_string().contains(secret));
    }
    let revoke = format!("{path}/{}/revoke", first["grant"]["id"].as_str().unwrap());
    let body =
        json!({"expectedResourceRevision":input["expectedResourceRevision"],"expectedRevision":1});
    let mut result = post(&revoke, &cookie).json(&body).send(&service).await;
    assert_eq!(result.status_code, Some(StatusCode::OK));
    assert_eq!(
        result.take_json::<Value>().await.unwrap()["state"],
        "revoked"
    );
    assert_eq!(
        post(&revoke, &cookie)
            .json(&body)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    // Revocation does not erase the contribution or silently free its capacity.
    let (budget, _) = f
        .domain
        .resource_headroom(resource.id(), expires - 1000)
        .await
        .unwrap();
    assert_eq!(u64::from(budget.pool.committed), 400);
    f.close().await;
}

#[tokio::test]
async fn native_console_contribution_requires_exact_generation_finite_limits_and_current_resource()
{
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let resource = native_resource("contribution_resource");
    f.domain.put_resource(resource.clone()).await.unwrap();
    let service = f.service();
    let cookie = session(&service).await;
    let registration = common::registration();
    let path = format!("/console/api/resources/{}/contributions", resource.id());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let input = json!({"requestId":"bounded","expectedResourceRevision":resource_publication_revision(&resource).unwrap(),"fleetId":registration.fleet_id,
        "registrationGeneration":registration.generation,"limits":{"tokens":100,"maxAgents":1,"maxRatePerDay":100},"expiresAtMs":now+86400000});
    for (pointer, value, expected) in [
        (
            "/registrationGeneration",
            json!(999),
            StatusCode::BAD_REQUEST,
        ),
        ("/limits/tokens", json!(0), StatusCode::BAD_REQUEST),
        ("/limits/maxAgents", json!(10001), StatusCode::BAD_REQUEST),
        (
            "/expectedResourceRevision",
            json!("0".repeat(64)),
            StatusCode::CONFLICT,
        ),
        ("/expiresAtMs", json!(now - 1), StatusCode::CONFLICT),
        ("/limits/tokens", json!(5001), StatusCode::CONFLICT),
    ] {
        let mut bad = input.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        assert_eq!(
            post(&path, &cookie)
                .json(&bad)
                .send(&service)
                .await
                .status_code,
            Some(expected),
            "{pointer}"
        );
    }
    let mut bad = input.clone();
    bad["administratorOverride"] = json!(true);
    assert_eq!(
        post(&path, &cookie)
            .json(&bad)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    assert!(
        f.domain
            .resource_contributions(resource.id(), String::new(), 16)
            .await
            .unwrap()
            .is_empty()
    );
    f.close().await;
}
