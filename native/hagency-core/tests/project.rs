use hagency_core::project::{AgentDefinition, Resource, RuntimePluginIdentity, public_resource_id};
use serde_json::Value;

#[test]
fn project_identity_vectors_match_javascript() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../fixtures/project-identities.json")).unwrap();
    assert_eq!(
        public_resource_id(fixture["preset"].as_str().unwrap()),
        fixture["resourceId"]
    );
    for vector in fixture["vectors"].as_array().unwrap() {
        let result = serde_json::from_value::<AgentDefinition>(vector["definition"].clone())
            .map_err(|e| e.to_string())
            .and_then(|v| v.validate().map(|()| v).map_err(|e| e.to_string()));
        if vector["rejected"] == true {
            assert!(result.is_err(), "{vector}");
            continue;
        }
        let definition = result.unwrap();
        assert_eq!(
            serde_json::to_value(&definition).unwrap(),
            vector["normalized"]
        );
        let context = &fixture["context"];
        assert_eq!(
            definition
                .runtime_name(
                    context["fleetId"].as_str().unwrap(),
                    context["targetProjectId"].as_str().unwrap(),
                    context["requestId"].as_str().unwrap()
                )
                .unwrap(),
            vector["runtimeName"],
            "{vector}"
        );
    }
}

#[test]
fn runtime_plugin_identity_round_trips_and_rejects_stale_digest_shape() {
    let mut resource: Resource = serde_json::from_value(serde_json::json!({
        "presetId":"plugin_preset",
        "seatId":"plugin_seat",
        "framework":"custom",
        "model":"model",
        "provider":"provider",
        "reasoning":null,
        "runtimePlugin":{"pluginId":"example.plugin","adapterId":"adapter","packageSha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","settingsSha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
        "ceiling":null
    })).unwrap();
    resource.validate().unwrap();
    assert_eq!(
        resource.runtime_plugin.as_ref().unwrap().adapter_id,
        "adapter"
    );
    resource.roles = vec!["review".into()];
    resource.ceiling = serde_json::from_value(serde_json::json!({
        "tokens": 100,
        "period": "monthly"
    }))
    .unwrap();
    resource.validate().unwrap();
    assert!(resource.provisionable());
    assert!(resource.qualifies("review"));
    let stale = RuntimePluginIdentity {
        package_sha256: "A".repeat(64),
        ..resource.runtime_plugin.take().unwrap()
    };
    assert!(stale.validate().is_err());
}
