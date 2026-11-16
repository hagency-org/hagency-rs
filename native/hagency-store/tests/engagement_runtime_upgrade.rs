//! ADR-197: a resource runtime change marks engagements `upgrade_pending`
//! instead of refusing, and never stops them.
mod common;
use common::*;
use hagency_core::project::{HarnessPluginIdentity, Resource, RuntimePluginIdentity};
use hagency_plugin::{HARNESS_TEMPLATE_SCHEMA, HarnessTemplateDocument, harness_template_digest};
use hagency_store::{
    DomainRepository, EffectOutcome, Error, PluginRuntimeQualification, UpgradeState,
};
use serde_json::json;
use std::collections::BTreeMap;

fn runtime(package: char, settings: char) -> RuntimePluginIdentity {
    RuntimePluginIdentity {
        plugin_id: "example.runtime".into(),
        adapter_id: "herdr".into(),
        package_sha256: package.to_string().repeat(64),
        settings_sha256: settings.to_string().repeat(64),
    }
}
fn qualify(db: &mut DomainRepository, runtime: &RuntimePluginIdentity) {
    db.put_plugin_runtime_qualification(&PluginRuntimeQualification {
        plugin_id: runtime.plugin_id.clone(),
        adapter_id: runtime.adapter_id.clone(),
        package_sha256: runtime.package_sha256.clone(),
        settings_sha256: runtime.settings_sha256.clone(),
        framework: "herdr".into(),
        model: "herdr".into(),
        provider: "herdr".into(),
        reasoning: None,
        roles: vec!["coding".into()],
        evidence_digest: "c".repeat(64),
        qualified_at: 1,
        evidence: json!({"passed": true}),
    })
    .unwrap();
}
fn template() -> HarnessTemplateDocument {
    let mut values = BTreeMap::new();
    values.insert("target".into(), json!("managed"));
    let mut template = HarnessTemplateDocument {
        schema: HARNESS_TEMPLATE_SCHEMA.into(),
        plugin_id: "example.harness".into(),
        harness_id: "managed".into(),
        package_sha256: "a".repeat(64),
        settings_sha256: "b".repeat(64),
        template_sha256: String::new(),
        values,
    };
    template.template_sha256 = harness_template_digest(&template).unwrap();
    template
}
struct Fixture {
    _root: tempfile::TempDir,
    db: DomainRepository,
    pool: Resource,
    engagement: String,
}
fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    let old = runtime('a', 'b');
    qualify(&mut db, &old);
    qualify(&mut db, &runtime('d', 'e'));
    let mut pool = resource("pool", "seat", 1000);
    pool.framework = "herdr".into();
    pool.model = "herdr".into();
    pool.provider = Some("herdr".into());
    pool.reasoning = None;
    pool.roles = vec!["coding".into()];
    pool.runtime_plugin = Some(old);
    pool.harness_plugin = Some(HarnessPluginIdentity {
        plugin_id: "example.harness".into(),
        harness_id: "managed".into(),
        package_sha256: "a".repeat(64),
        settings_sha256: "b".repeat(64),
    });
    db.put_resource(&pool).unwrap();
    db.set_resource_default_harness_template(&pool.id(), Some(&template()), 999)
        .unwrap();
    let proof = proof(&request("upgrade", "Worker", &pool, 100));
    let engagement = db.admit(&proof, 1000).unwrap();
    db.approve("approve", &proof, 1000).unwrap();
    let effect = db.claim_effect().unwrap().unwrap();
    db.observe_effect(
        &effect.id,
        effect.fence,
        &EffectOutcome::Applied {
            receipt: "fixture".into(),
        },
    )
    .unwrap();
    Fixture {
        _root: root,
        db,
        pool,
        engagement: engagement.id,
    }
}
fn state_of(db: &DomainRepository, id: &str) -> String {
    db.engagements("", 10)
        .unwrap()
        .into_iter()
        .find(|e| e.id == id)
        .map(|e| format!("{:?}", e.state).to_lowercase())
        .unwrap()
}

#[test]
fn resource_runtime_change_marks_engagements_upgrade_pending() {
    let mut f = fixture();
    assert_eq!(state_of(&f.db, &f.engagement), "active");
    assert!(f.db.engagement_runtime_upgrades(None).unwrap().is_empty());
    let mut changed = f.pool.clone();
    changed.runtime_plugin = Some(runtime('d', 'e'));
    f.db.put_resource(&changed).unwrap();
    let marked =
        f.db.engagement_runtime_upgrades(Some(&f.pool.id()))
            .unwrap();
    assert_eq!(marked.len(), 1);
    assert_eq!(marked[0].engagement_id, f.engagement);
    assert_eq!(marked[0].state, UpgradeState::UpgradePending);
    assert_eq!(marked[0].target_runtime, runtime('d', 'e'));
    assert_eq!(marked[0].frozen_runtime, Some(runtime('a', 'b')));
    // The engagement is not stopped and its frozen package is still referenced.
    assert_eq!(state_of(&f.db, &f.engagement), "active");
    assert_eq!(
        f.db.engagement_runtime_references().unwrap(),
        vec![("example.runtime".to_owned(), "a".repeat(64), 1)]
    );
    // A settings-only change of the same package marks too, and moves the target.
    let mut settings_only = changed.clone();
    settings_only.runtime_plugin = Some(runtime('d', 'f'));
    qualify(&mut f.db, &runtime('d', 'f'));
    f.db.put_resource(&settings_only).unwrap();
    let marked = f.db.engagement_runtime_upgrades(None).unwrap();
    assert_eq!(marked.len(), 1);
    assert_eq!(marked[0].target_runtime, runtime('d', 'f'));
    // Returning to the frozen runtime clears the mark.
    f.db.put_resource(&f.pool).unwrap();
    assert!(f.db.engagement_runtime_upgrades(None).unwrap().is_empty());
}

#[test]
fn resource_non_runtime_change_still_refused_while_in_use() {
    let mut f = fixture();
    let mut other_model = f.pool.clone();
    other_model.model = "other".into();
    assert!(matches!(f.db.put_resource(&other_model), Err(Error::State)));
    let mut other_adapter = f.pool.clone();
    other_adapter.runtime_plugin = Some(RuntimePluginIdentity {
        adapter_id: "other".into(),
        ..runtime('d', 'e')
    });
    assert!(matches!(
        f.db.put_resource(&other_adapter),
        Err(Error::State)
    ));
    let mut no_runtime = f.pool.clone();
    no_runtime.runtime_plugin = None;
    assert!(matches!(f.db.put_resource(&no_runtime), Err(Error::State)));
    assert!(f.db.engagement_runtime_upgrades(None).unwrap().is_empty());
}
