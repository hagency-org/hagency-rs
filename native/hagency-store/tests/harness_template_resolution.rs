//! ADR-198: a request on a harness-plugin resource either freezes a template
//! that validates against that plugin (matching definition, else the
//! resource default) or is refused up front with a named reason. It never
//! becomes an agent that can only fail at host admission.
mod common;
use common::*;
use hagency_core::project::HarnessPluginIdentity;
use hagency_plugin::{HARNESS_TEMPLATE_SCHEMA, HarnessTemplateDocument, harness_template_digest};
use hagency_store::{DomainRepository, Error};
use serde_json::json;
use std::collections::BTreeMap;

fn identity() -> HarnessPluginIdentity {
    HarnessPluginIdentity {
        plugin_id: "example.harness".into(),
        harness_id: "managed".into(),
        package_sha256: "a".repeat(64),
        settings_sha256: "b".repeat(64),
    }
}
fn template(target: &str) -> HarnessTemplateDocument {
    let identity = identity();
    let mut values = BTreeMap::new();
    values.insert("target".into(), json!(target));
    let mut document = HarnessTemplateDocument {
        schema: HARNESS_TEMPLATE_SCHEMA.into(),
        plugin_id: identity.plugin_id,
        harness_id: identity.harness_id,
        package_sha256: identity.package_sha256,
        settings_sha256: identity.settings_sha256,
        template_sha256: String::new(),
        values,
    };
    document.template_sha256 = harness_template_digest(&document).unwrap();
    document
}
fn setup() -> (
    tempfile::TempDir,
    DomainRepository,
    hagency_core::project::Resource,
) {
    let dir = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    let mut pool = resource("default_pool", "default_seat", 1000);
    pool.harness_plugin = Some(identity());
    db.put_resource(&pool).unwrap();
    (dir, db, pool)
}
fn define(
    db: &mut DomainRepository,
    pool: &hagency_core::project::Resource,
    name: &str,
    target: &str,
) {
    db.edit_agent_definition(
        &pool.id(),
        None,
        Some(
            &serde_json::from_value(
                json!({"name":name,"role":"coding","harnessTemplate":template(target)}),
            )
            .unwrap(),
        ),
        1000,
    )
    .unwrap();
}
fn target_of(engagement: &hagency_core::project::Engagement) -> String {
    engagement.harness_template.as_ref().unwrap().values["target"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn provision_resolves_template_from_matching_definition() {
    let (_dir, mut db, pool) = setup();
    define(&mut db, &pool, "named_agent", "by-definition");
    db.set_resource_default_harness_template(&pool.id(), Some(&template("by-default")), 1001)
        .unwrap();
    let engagement = db
        .admit(
            &proof(&request("request_named", "named_agent", &pool, 100)),
            1002,
        )
        .unwrap();
    assert_eq!(target_of(&engagement), "by-definition");
}

#[test]
fn provision_falls_back_to_resource_default_template() {
    let (_dir, mut db, pool) = setup();
    db.set_resource_default_harness_template(&pool.id(), Some(&template("by-default")), 1001)
        .unwrap();
    // A Rinx-style request: only a name and resource, no matching definition.
    let engagement = db
        .admit(
            &proof(&request("request_rinx", "qore_orchestrator", &pool, 100)),
            1002,
        )
        .unwrap();
    assert_eq!(target_of(&engagement), "by-default");
    assert_eq!(
        db.resource_default_harness_template(&pool.id())
            .unwrap()
            .unwrap()
            .template_sha256,
        template("by-default").template_sha256
    );
}

#[test]
fn provision_refused_harness_template_missing_before_any_identity() {
    let (_dir, mut db, pool) = setup();
    let error = db
        .admit(
            &proof(&request("request_none", "qore_orchestrator", &pool, 100)),
            1002,
        )
        .unwrap_err();
    match &error {
        Error::HarnessTemplate { code, message } => {
            assert_eq!(*code, "harness_template_missing");
            assert!(message.contains("qore_orchestrator"));
            assert!(message.contains("default harness template"));
        }
        other => panic!("expected a named harness template refusal, got {other:?}"),
    }
    // Nothing was reserved: no engagement row exists for the request.
    assert!(db.engagements("", 10).unwrap().is_empty());
    // Clearing the default restores the refusal.
    db.set_resource_default_harness_template(&pool.id(), Some(&template("t")), 1003)
        .unwrap();
    db.set_resource_default_harness_template(&pool.id(), None, 1004)
        .unwrap();
    assert!(matches!(
        db.admit(
            &proof(&request("request_none", "qore_orchestrator", &pool, 100)),
            1005
        ),
        Err(Error::HarnessTemplate {
            code: "harness_template_missing",
            ..
        })
    ));
}

#[test]
fn provision_refused_harness_template_invalid_names_source() {
    let (dir, mut db, pool) = setup();
    db.set_resource_default_harness_template(&pool.id(), Some(&template("by-default")), 1001)
        .unwrap();
    // The resource's harness plugin changed after the default was saved.
    let sql = rusqlite::Connection::open(dir.path().join("state/domain.sqlite3")).unwrap();
    let mut stale = template("by-default");
    stale.settings_sha256 = "c".repeat(64);
    stale.template_sha256 = harness_template_digest(&stale).unwrap();
    sql.execute(
        "UPDATE resource_default_harness_templates SET template=?1",
        [serde_json::to_string(&stale).unwrap()],
    )
    .unwrap();
    let error = db
        .admit(
            &proof(&request("request_stale", "qore_orchestrator", &pool, 100)),
            1002,
        )
        .unwrap_err();
    match &error {
        Error::HarnessTemplate { code, message } => {
            assert_eq!(*code, "harness_template_invalid");
            assert!(message.contains("default harness template"));
            assert!(!message.contains("by-default"), "setting values stay out");
        }
        other => panic!("expected a named harness template refusal, got {other:?}"),
    }
    assert!(db.engagements("", 10).unwrap().is_empty());
}

#[test]
fn provision_refusal_reaches_palpo_request_result() {
    let (_dir, mut db, pool) = setup();
    let error = db
        .admit(
            &proof(&request("request_palpo", "qore_orchestrator", &pool, 100)),
            1002,
        )
        .unwrap_err();
    // The reason Palpo's coordinator receipt (and so Rinx) reads.
    assert_eq!(
        hagency_store::coordinator::coordinator_refusal_reason(&error),
        Some("harness_template_missing")
    );
}

#[test]
fn resource_default_template_change_does_not_alter_existing_engagements() {
    let (_dir, mut db, pool) = setup();
    db.set_resource_default_harness_template(&pool.id(), Some(&template("first")), 1001)
        .unwrap();
    let first = db
        .admit(
            &proof(&request("request_first", "agent_one", &pool, 100)),
            1002,
        )
        .unwrap();
    db.set_resource_default_harness_template(&pool.id(), Some(&template("second")), 1003)
        .unwrap();
    let again = db.get(&first.id).unwrap();
    assert_eq!(target_of(&again), "first");
    let second = db
        .admit(
            &proof(&request("request_second", "agent_two", &pool, 100)),
            1004,
        )
        .unwrap();
    assert_eq!(target_of(&second), "second");
}

#[test]
fn default_template_needs_a_matching_harness_resource() {
    let (_dir, mut db, pool) = setup();
    let mut mismatched = template("x");
    mismatched.plugin_id = "other.harness".into();
    mismatched.template_sha256 = harness_template_digest(&mismatched).unwrap();
    assert!(
        db.set_resource_default_harness_template(&pool.id(), Some(&mismatched), 1001)
            .is_err()
    );
    let plain = resource("plain_pool", "plain_seat", 1000);
    db.put_resource(&plain).unwrap();
    assert!(matches!(
        db.set_resource_default_harness_template(&plain.id(), Some(&template("x")), 1001),
        Err(Error::Unqualified)
    ));
    // A resource without a harness plugin is untouched by the refusal.
    db.admit(
        &proof(&request("request_plain", "plain_agent", &plain, 100)),
        1002,
    )
    .unwrap();
}

#[test]
fn console_marks_engagement_with_null_template_cannot_run() {
    let (dir, mut db, pool) = setup();
    db.set_resource_default_harness_template(&pool.id(), Some(&template("t")), 1001)
        .unwrap();
    let healthy = db
        .admit(&proof(&request("request_ok", "agent_ok", &pool, 100)), 1002)
        .unwrap();
    let broken = db
        .admit(
            &proof(&request("request_null", "agent_null", &pool, 100)),
            1003,
        )
        .unwrap();
    // The 2026-11-16 shape: a live engagement that froze no template.
    let sql = rusqlite::Connection::open(dir.path().join("state/domain.sqlite3")).unwrap();
    sql.execute(
        "UPDATE engagements SET projection=json_remove(projection,'$.harnessTemplate') WHERE id=?1",
        [&broken.id],
    )
    .unwrap();
    let labels = db.engagement_labels("", None, 10).unwrap();
    let view = |id: &str| {
        serde_json::to_value(labels.iter().find(|label| label.id == id).unwrap()).unwrap()
    };
    assert_eq!(view(&broken.id)["cannotRun"], "harness_template_missing");
    assert!(view(&healthy.id).get("cannotRun").is_none());
}
