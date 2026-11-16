//! ADR-197: guarded per-engagement rebind, canary rollout, rollback, pin, audit.
mod common;
use common::*;
use hagency_core::project::{HarnessPluginIdentity, Resource, RuntimePluginIdentity};
use hagency_plugin::{HARNESS_TEMPLATE_SCHEMA, HarnessTemplateDocument, harness_template_digest};
use hagency_store::{
    DomainRepository, EffectOutcome, PluginRuntimeQualification, RebindRefusal, RolloutStep,
    UpgradeState,
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
const ACTOR: &str = "operator";
struct Fixture {
    root: tempfile::TempDir,
    db: DomainRepository,
    pool: Resource,
    engagements: Vec<String>,
}
fn fixture(count: usize) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    let old = runtime('a', 'b');
    qualify(&mut db, &old);
    qualify(&mut db, &runtime('d', 'e'));
    let mut pool = resource("pool", "seat", 10000);
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
    let mut engagements = Vec::new();
    for index in 0..count {
        let proof = proof(&request(
            &format!("upgrade{index}"),
            &format!("Worker{index}"),
            &pool,
            100,
        ));
        let engagement = db.admit(&proof, 1000).unwrap();
        db.approve(&format!("approve{index}"), &proof, 1000)
            .unwrap();
        let effect = db.claim_effect().unwrap().unwrap();
        db.observe_effect(
            &effect.id,
            effect.fence,
            &EffectOutcome::Applied {
                receipt: "fixture".into(),
            },
        )
        .unwrap();
        engagements.push(engagement.id);
    }
    // The rollout walks engagements in id order.
    engagements.sort();
    Fixture {
        root,
        db,
        pool,
        engagements,
    }
}
impl Fixture {
    fn mark(&mut self) {
        let mut changed = self.pool.clone();
        changed.runtime_plugin = Some(runtime('d', 'e'));
        self.db.put_resource(&changed).unwrap();
    }
    fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.root.path().join("state/domain.sqlite3")).unwrap()
    }
    fn frozen(&self, id: &str) -> RuntimePluginIdentity {
        let value: String = self
            .sql()
            .query_row(
                "SELECT json_extract(payload,'$.resource.runtimePlugin') FROM effects WHERE engagement_id=?1 AND kind='provision'",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str(&value).unwrap()
    }
    fn session(&self, engagement: &str) -> String {
        let id = format!("session_{engagement}");
        self.sql()
            .execute(
                "INSERT INTO runner_sessions(id,engagement_id,binding) VALUES(?1,?2,'{}')",
                [&id, engagement],
            )
            .unwrap();
        id
    }
    fn dispatch(&self, session: &str, state: &str) -> String {
        let id = format!("dispatch_{session}_{state}");
        self.sql()
            .execute(
                "INSERT INTO runner_dispatches(id,session_id,input,digest,state,fence) VALUES(?1,?2,'{}','d',?3,1)",
                rusqlite::params![id, session, state],
            )
            .unwrap();
        id
    }
    fn rebind(&mut self, engagement: &str) -> hagency_store::RebindResult {
        self.db
            .rebind_engagement_runtime(engagement, ACTOR, 2000)
            .unwrap()
    }
    /// Everything that is identity, allocation or budget, with the frozen
    /// runtime removed from the provision effect.
    fn identity(&self, id: &str) -> String {
        self.sql()
            .query_row(
                "SELECT json_array(e.id,e.fleet_id,e.generation,e.request_id,e.digest,e.context,e.evidence,e.project_id,e.name,e.resource_id,e.preset_id,e.seat_id,e.tokens,e.state,e.projection,e.allocated_tokens,\
                 json_remove(f.payload,'$.resource.runtimePlugin'),f.fence,f.state) \
                 FROM engagements e JOIN effects f ON f.engagement_id=e.id AND f.kind='provision' WHERE e.id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
    }
}
fn refusal(result: &hagency_store::RebindResult) -> Option<RebindRefusal> {
    assert!(result.applied != result.refusal.is_some());
    result.refusal
}

#[test]
fn engagement_rebind_applies_at_idle_and_keeps_identity() {
    let mut f = fixture(1);
    let id = f.engagements[0].clone();
    f.mark();
    let before = f.identity(&id);
    let result = f.rebind(&id);
    assert!(result.applied, "{result:?}");
    assert_eq!(f.frozen(&id), runtime('d', 'e'));
    assert_eq!(f.identity(&id), before);
    assert!(f.db.engagement_runtime_upgrades(None).unwrap().is_empty());
    // The new frozen package is referenced, and the previous one is held by the rollback slot.
    assert_eq!(
        f.db.engagement_runtime_references().unwrap(),
        vec![
            ("example.runtime".to_owned(), "a".repeat(64), 1),
            ("example.runtime".to_owned(), "d".repeat(64), 1)
        ]
    );
}

#[test]
fn engagement_rebind_writes_audit_record() {
    let mut f = fixture(1);
    let id = f.engagements[0].clone();
    f.mark();
    f.rebind(&id);
    let audit = f.db.engagement_runtime_rebinds(Some(&id), 10).unwrap();
    assert_eq!(audit.len(), 1);
    let row = &audit[0];
    assert_eq!(row.action, "rebind");
    assert_eq!(row.outcome, "applied");
    assert_eq!(row.actor, ACTOR);
    assert_eq!(row.at, 2000);
    assert_eq!(
        row.old_package_sha256.as_deref(),
        Some("a".repeat(64).as_str())
    );
    assert_eq!(
        row.new_package_sha256.as_deref(),
        Some("d".repeat(64).as_str())
    );
    assert_eq!(
        row.old_settings_sha256.as_deref(),
        Some("b".repeat(64).as_str())
    );
    assert_eq!(
        row.new_settings_sha256.as_deref(),
        Some("e".repeat(64).as_str())
    );
    // Append-only.
    let sql = f.sql();
    assert!(
        sql.execute("UPDATE engagement_runtime_rebinds SET actor='x'", [])
            .is_err()
    );
    assert!(
        sql.execute("DELETE FROM engagement_runtime_rebinds", [])
            .is_err()
    );
}

#[test]
fn engagement_rebind_refused_while_dispatch_leased_started_or_parked() {
    for (state, expected) in [
        ("leased", RebindRefusal::DispatchLeased),
        ("started", RebindRefusal::DispatchStarted),
        ("parked", RebindRefusal::DispatchParked),
    ] {
        let mut f = fixture(1);
        let id = f.engagements[0].clone();
        f.mark();
        let session = f.session(&id);
        let dispatch = f.dispatch(&session, state);
        let result = f.rebind(&id);
        assert_eq!(refusal(&result), Some(expected));
        assert_eq!(f.frozen(&id), runtime('a', 'b'));
        // Never silent redo: the dispatch is exactly as it was, nothing queued.
        let (st, fence): (String, u64) = f
            .sql()
            .query_row(
                "SELECT state,fence FROM runner_dispatches WHERE id=?1",
                [&dispatch],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((st.as_str(), fence), (state, 1));
        let total: u64 = f
            .sql()
            .query_row("SELECT COUNT(*) FROM runner_dispatches", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 1);
        let audit = f.db.engagement_runtime_rebinds(Some(&id), 10).unwrap();
        assert_eq!(audit[0].outcome, format!("refused:{}", expected.as_str()));
        // Still marked, so the rollout re-checks later.
        assert_eq!(f.db.engagement_runtime_upgrades(None).unwrap().len(), 1);
    }
}

#[test]
fn engagement_rebind_refused_with_unresolved_dispatch() {
    let mut f = fixture(1);
    let id = f.engagements[0].clone();
    f.mark();
    let session = f.session(&id);
    // A dispatch interrupted while running: outcome unknown, awaiting the operator.
    let dispatch = f.dispatch(&session, "started");
    f.sql()
        .execute(
            "UPDATE runner_dispatches SET state='outcome_unknown' WHERE id=?1",
            [&dispatch],
        )
        .unwrap();
    let result = f.rebind(&id);
    assert_eq!(refusal(&result), Some(RebindRefusal::UnresolvedDispatch));
    assert_eq!(f.frozen(&id), runtime('a', 'b'));
    let (state, queued): (String, u64) = f
        .sql()
        .query_row(
            "SELECT (SELECT state FROM runner_dispatches WHERE id=?1),(SELECT COUNT(*) FROM runner_dispatches WHERE state='queued')",
            [&dispatch],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((state.as_str(), queued), ("outcome_unknown", 0));
}

#[test]
fn engagement_rebind_refused_with_pending_outgoing_custody() {
    let mut f = fixture(1);
    let id = f.engagements[0].clone();
    f.mark();
    let session = f.session(&id);
    f.sql()
        .execute(
            "INSERT INTO command_notices(id,session_id,transaction_id,digest,body,route,source_event_id,state,created_at,updated_at) \
             VALUES('n1',?1,'tx1','d','b','{}','$e','uncertain',1,1)",
            [&session],
        )
        .unwrap();
    let result = f.rebind(&id);
    assert_eq!(
        refusal(&result),
        Some(RebindRefusal::OutgoingCustodyPending)
    );
    assert_eq!(f.frozen(&id), runtime('a', 'b'));
    // Once the custody settles, the same rebind goes through.
    f.sql()
        .execute("UPDATE command_notices SET state='delivered'", [])
        .unwrap();
    assert!(f.rebind(&id).applied);
}

#[test]
fn engagement_rebind_requires_same_plugin_adapter_framework_and_qualification() {
    let mut f = fixture(1);
    let id = f.engagements[0].clone();
    f.mark();
    // Different adapter is not a rebind.
    f.sql()
        .execute(
            "UPDATE engagement_runtime_upgrades SET target_runtime=json_set(target_runtime,'$.adapterId','other')",
            [],
        )
        .unwrap();
    assert_eq!(refusal(&f.rebind(&id)), Some(RebindRefusal::PluginMismatch));
    f.sql()
        .execute(
            "UPDATE engagement_runtime_upgrades SET target_runtime=json_set(target_runtime,'$.adapterId','herdr')",
            [],
        )
        .unwrap();
    // The new package qualified for another role only.
    f.sql()
        .execute(
            "UPDATE plugin_runtime_qualifications SET roles='[\"review\"]' WHERE package_sha256=?1",
            ["d".repeat(64)],
        )
        .unwrap();
    assert_eq!(refusal(&f.rebind(&id)), Some(RebindRefusal::Unqualified));
    // Qualification revoked entirely.
    f.db.revoke_plugin_runtime_qualifications("example.runtime")
        .unwrap();
    assert_eq!(refusal(&f.rebind(&id)), Some(RebindRefusal::Unqualified));
    assert_eq!(f.frozen(&id), runtime('a', 'b'));
    // Not marked at all.
    let mut other = fixture(1);
    let other_id = other.engagements[0].clone();
    assert_eq!(
        refusal(&other.rebind(&other_id)),
        Some(RebindRefusal::NotMarked)
    );
}

#[test]
fn engagement_rollout_halts_after_failed_canary() {
    let mut f = fixture(3);
    let ids = f.engagements.clone();
    f.mark();
    let step = f.db.rollout_step(&f.pool.id(), ACTOR, 2000).unwrap();
    assert!(
        matches!(&step, RolloutStep::Rebound { engagement_id, canary: true } if *engagement_id == ids[0])
    );
    // Nothing else moves until the canary passes.
    let step = f.db.rollout_step(&f.pool.id(), ACTOR, 2001).unwrap();
    assert!(
        matches!(&step, RolloutStep::AwaitingCanary { engagement_id } if *engagement_id == ids[0])
    );
    assert_eq!(f.frozen(&ids[1]), runtime('a', 'b'));
    let status =
        f.db.record_canary_result(&f.pool.id(), false, ACTOR, Some("probe failed"), 2002)
            .unwrap();
    assert_eq!(status.state, "halted");
    assert!(matches!(
        f.db.rollout_step(&f.pool.id(), ACTOR, 2003).unwrap(),
        RolloutStep::Halted
    ));
    // The rest stay pending on their old, still referenced package.
    for id in &ids[1..] {
        assert_eq!(f.frozen(id), runtime('a', 'b'));
    }
    assert_eq!(f.db.engagement_runtime_upgrades(None).unwrap().len(), 2);
    let audit = f.db.engagement_runtime_rebinds(Some(&ids[0]), 10).unwrap();
    assert_eq!(audit[0].outcome, "halted");
}

#[test]
fn engagement_rollout_continues_after_canary_success() {
    let mut f = fixture(3);
    let ids = f.engagements.clone();
    f.mark();
    f.db.rollout_step(&f.pool.id(), ACTOR, 2000).unwrap();
    // One task completes on the new package: the canary passes by itself.
    let session = f.session(&ids[0]);
    let dispatch = f.dispatch(&session, "completed");
    f.sql()
        .execute(
            "INSERT INTO runner_attempts(dispatch_id,fence,runner_id,outcome,capability_hash,created_at) VALUES(?1,1,'r','completed','h',2500)",
            [&dispatch],
        )
        .unwrap();
    let step = f.db.rollout_step(&f.pool.id(), ACTOR, 3000).unwrap();
    assert!(
        matches!(&step, RolloutStep::Rebound { engagement_id, canary: false } if *engagement_id == ids[1])
    );
    assert_eq!(f.frozen(&ids[0]), runtime('d', 'e'));
    assert_eq!(f.frozen(&ids[1]), runtime('d', 'e'));
    assert_eq!(f.frozen(&ids[2]), runtime('a', 'b'));
    f.db.rollout_step(&f.pool.id(), ACTOR, 3001).unwrap();
    assert!(matches!(
        f.db.rollout_step(&f.pool.id(), ACTOR, 3002).unwrap(),
        RolloutStep::Complete
    ));
    assert!(f.db.engagement_runtime_upgrades(None).unwrap().is_empty());
}

#[test]
fn engagement_rollout_skips_busy_engagement_and_retries_later() {
    let mut f = fixture(2);
    let ids = f.engagements.clone();
    f.mark();
    let session = f.session(&ids[0]);
    f.dispatch(&session, "started");
    let step = f.db.rollout_step(&f.pool.id(), ACTOR, 2000).unwrap();
    assert!(
        matches!(&step, RolloutStep::Rebound { engagement_id, canary: true } if *engagement_id == ids[1])
    );
    assert_eq!(f.frozen(&ids[0]), runtime('a', 'b'));
}

#[test]
fn engagement_rollback_restores_previous_package_one_step() {
    let mut f = fixture(1);
    let id = f.engagements[0].clone();
    f.mark();
    assert_eq!(
        refusal(&f.db.rollback_engagement_runtime(&id, ACTOR, 1500).unwrap()),
        Some(RebindRefusal::NoPrevious)
    );
    f.rebind(&id);
    let before = f.identity(&id);
    // Rollback obeys the idle boundary.
    let session = f.session(&id);
    let dispatch = f.dispatch(&session, "parked");
    let busy = f.db.rollback_engagement_runtime(&id, ACTOR, 2100).unwrap();
    assert_eq!(refusal(&busy), Some(RebindRefusal::DispatchParked));
    f.sql()
        .execute(
            "UPDATE runner_dispatches SET state='completed' WHERE id=?1",
            [&dispatch],
        )
        .unwrap();
    let result = f.db.rollback_engagement_runtime(&id, ACTOR, 2200).unwrap();
    assert!(result.applied, "{result:?}");
    assert_eq!(result.audit.outcome, "rolled_back");
    assert_eq!(f.frozen(&id), runtime('a', 'b'));
    assert_eq!(f.identity(&id), before);
    // One step only; the package it left is visible as halted, not re-queued.
    let again = f.db.rollback_engagement_runtime(&id, ACTOR, 2300).unwrap();
    assert_eq!(refusal(&again), Some(RebindRefusal::NoPrevious));
    let marked = f.db.engagement_runtime_upgrades(None).unwrap();
    assert_eq!(marked[0].state, UpgradeState::Halted);
    assert_eq!(refusal(&f.rebind(&id)), Some(RebindRefusal::RolloutHalted));
}

#[test]
fn engagement_pin_skips_rollout_and_holds_revision() {
    let mut f = fixture(2);
    let ids = f.engagements.clone();
    f.db.pin_engagement_runtime(&ids[0], ACTOR, 1500).unwrap();
    f.mark();
    let marked = f.db.engagement_runtime_upgrades(None).unwrap();
    assert!(
        marked
            .iter()
            .any(|m| m.engagement_id == ids[0] && m.state == UpgradeState::Pinned)
    );
    assert_eq!(refusal(&f.rebind(&ids[0])), Some(RebindRefusal::Pinned));
    // The rollout takes the other one as canary and never the pinned one.
    let step = f.db.rollout_step(&f.pool.id(), ACTOR, 2000).unwrap();
    assert!(
        matches!(&step, RolloutStep::Rebound { engagement_id, .. } if *engagement_id == ids[1])
    );
    f.db.record_canary_result(&f.pool.id(), true, ACTOR, None, 2001)
        .unwrap();
    assert!(matches!(
        f.db.rollout_step(&f.pool.id(), ACTOR, 2002).unwrap(),
        RolloutStep::Complete
    ));
    assert_eq!(f.frozen(&ids[0]), runtime('a', 'b'));
    // The pinned engagement keeps its revision referenced.
    assert!(f.db.engagement_runtime_references().unwrap().contains(&(
        "example.runtime".to_owned(),
        "a".repeat(64),
        2
    )));
    // Unpin returns it to the queue.
    f.db.unpin_engagement_runtime(&ids[0], ACTOR, 2100).unwrap();
    assert!(f.rebind(&ids[0]).applied);
    let audit = f.db.engagement_runtime_rebinds(Some(&ids[0]), 10).unwrap();
    let actions: Vec<&str> = audit.iter().map(|a| a.action.as_str()).collect();
    assert_eq!(actions, ["rebind", "unpin", "rebind", "pin"]);
}
