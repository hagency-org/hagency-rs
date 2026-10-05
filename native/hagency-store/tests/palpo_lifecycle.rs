mod common;
use common::*;
use hagency_core::{project::CleanupState, replies::*, tasks::*};
use hagency_store::{DomainRepository, EffectOutcome, Error};
use serde_json::json;
use std::collections::BTreeSet;

fn setup(provision: bool) -> (tempfile::TempDir, DomainRepository, String, String) {
    let root = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    let pool = resource("retirement", "seat", 1000);
    db.put_resource(&pool).unwrap();
    let proof = proof(&request("retire_request", "Worker", &pool, 100));
    let id = db.admit(&proof, 1000).unwrap().id;
    db.approve("approve", &proof, 1000).unwrap();
    let mxid = format!(
        "@{}_agent_actual_identity:example.test",
        registration().fleet_id
    );
    if provision {
        let effect = db.claim_effect().unwrap().unwrap();
        db.observe_effect(
            &effect.id,
            effect.fence,
            &EffectOutcome::Applied {
                receipt: "fixture provision".into(),
            },
        )
        .unwrap();
        db.observe_matrix_transport(
            &MatrixTransportObservation {
                engagement_id: id.clone(),
                registration_generation: 1,
                generation: 1,
                sender_mxid: mxid.clone(),
                device_id: "DEVICE".into(),
            },
            1001,
        )
        .unwrap();
    }
    (root, db, id, mxid)
}
fn local_complete(db: &mut DomainRepository, id: &str) {
    let effect = db
        .claim_effect_for(&format!("retire_{id}"))
        .unwrap()
        .unwrap();
    db.observe_effect(
        &effect.id,
        effect.fence,
        &EffectOutcome::Applied {
            receipt: "fixture leave/logout".into(),
        },
    )
    .unwrap();
}

#[test]
fn retirement_uses_recorded_identity_and_waits_for_local_proof_then_persists_exact_remote_receipt()
{
    let (root, mut db, id, mxid) = setup(true);
    assert!(matches!(
        db.palpo_retirement_target(&registration(), &id),
        Err(Error::State)
    ));
    let view = db.palpo_agent_lifecycle(&registration(), &id).unwrap();
    assert_eq!(view.agent_mxid.as_deref(), Some(mxid.as_str()));
    assert_eq!(view.spent_tokens_lower_bound, None);
    assert!(!view.runtime_stopped);
    db.revoke("revoke", &id).unwrap();
    assert!(matches!(
        db.palpo_retirement_target(&registration(), &id),
        Err(Error::State)
    ));
    local_complete(&mut db, &id);
    let target = db.palpo_retirement_target(&registration(), &id).unwrap();
    assert_eq!(target.agent_mxid, mxid);
    let mut wrong = target.clone();
    wrong.agent_mxid = "@someone_else:example.test".into();
    assert!(matches!(
        db.confirm_palpo_retirement(&registration(), &wrong, 2000),
        Err(Error::Conflict)
    ));
    db.confirm_palpo_retirement(&registration(), &target, 2000)
        .unwrap();
    db.confirm_palpo_retirement(&registration(), &target, 2001)
        .unwrap();
    drop(db);
    let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
    assert!(
        db.palpo_agent_lifecycle(&registration(), &id)
            .unwrap()
            .matrix_retired
    );
    let mut rotated = registration();
    rotated.generation = 2;
    db.register(&rotated).unwrap();
    assert!(matches!(
        db.confirm_palpo_retirement(&registration(), &target, 2002),
        Err(Error::Generation)
    ));
    assert!(db.palpo_agent_lifecycle(&rotated, &id).is_err());
}

#[test]
fn unprovisioned_removal_requires_no_invented_matrix_identity() {
    let (_root, mut db, id, _) = setup(false);
    db.revoke("cancel_before_provision", &id).unwrap();
    let view = db.palpo_agent_lifecycle(&registration(), &id).unwrap();
    assert!(view.runtime_stopped);
    assert_eq!(view.local_cleanup, CleanupState::NotRequired);
    assert_eq!(view.agent_mxid, None);
    assert!(!view.matrix_retired);
    assert!(matches!(
        db.palpo_retirement_target(&registration(), &id),
        Err(Error::State)
    ));
}

#[test]
fn uncertain_cleanup_and_live_dispatch_custody_never_prove_runtime_stopped() {
    let (_root, mut db, id, mxid) = setup(true);
    db.observe_matrix_room(
        &MatrixRoomObservation {
            engagement_id: id.clone(),
            registration_generation: 1,
            transport_generation: 1,
            room_id: "!project:example.test".into(),
            generation: 1,
            privacy: RoomPrivacy::Group {},
            joined: BTreeSet::from([mxid, "@owner:example.test".into()]),
            invite_only: true,
            encrypted: false,
        },
        1002,
    )
    .unwrap();
    let binding = SessionBinding {
        id: "session".into(),
        engagement_id: id.clone(),
        room_id: "!project:example.test".into(),
        thread_root: None,
    };
    db.resolve_verified_matrix_session(&binding, 1003).unwrap();
    db.enqueue_dispatch(&DispatchInput {
        id: "running".into(),
        session_id: binding.id,
        task_id: None,
        resources: vec![],
        payload: json!({"instruction":"work"}),
    })
    .unwrap();
    let cap = db
        .claim_dispatch("runner", 1005, 60000, 120000, 8)
        .unwrap()
        .unwrap();
    db.start_dispatch(&cap, 1006).unwrap();
    db.revoke("revoke", &id).unwrap();
    local_complete(&mut db, &id);
    assert!(
        !db.palpo_agent_lifecycle(&registration(), &id)
            .unwrap()
            .runtime_stopped
    );
    assert!(db.palpo_retirement_target(&registration(), &id).is_err());
}

#[test]
fn cleanup_failure_can_be_distinguished_from_uncertain_effects() {
    let (_root, mut db, id, _) = setup(true);
    db.revoke("revoke", &id).unwrap();
    let effect = db
        .claim_effect_for(&format!("retire_{id}"))
        .unwrap()
        .unwrap();
    db.observe_effect(&effect.id, effect.fence, &EffectOutcome::Unknown)
        .unwrap();
    let view = db.palpo_agent_lifecycle(&registration(), &id).unwrap();
    assert_eq!(view.local_cleanup, CleanupState::Uncertain);
    assert!(!view.cleanup_retryable);
    assert_eq!(view.cleanup_attempt, effect.fence);
    db.observe_effect(
        &effect.id,
        effect.fence,
        &EffectOutcome::NotApplied {
            receipt: "fixture definitive refusal".into(),
        },
    )
    .unwrap();
    assert!(
        db.palpo_agent_lifecycle(&registration(), &id)
            .unwrap()
            .cleanup_retryable
    );
    db.retry_cleanup("retry", &id).unwrap();
    local_complete(&mut db, &id);
    assert!(
        db.palpo_agent_lifecycle(&registration(), &id)
            .unwrap()
            .cleanup_attempt
            > effect.fence
    );
    assert!(
        db.palpo_agent_lifecycle(&registration(), &id)
            .unwrap()
            .runtime_stopped
    );
}

#[test]
fn schema_62_upgrade_preserves_original_engagement_and_has_no_remote_proof() {
    let (root, db, id, _) = setup(true);
    drop(db);
    let sql = rusqlite::Connection::open(root.path().join("state/domain.sqlite3")).unwrap();
    sql.execute_batch("ALTER TABLE project_grants DROP COLUMN released_at; DROP TABLE palpo_agent_retirements; PRAGMA user_version=62;")
        .unwrap();
    drop(sql);
    for _ in 0..2 {
        let db = DomainRepository::open(&root.path().join("state")).unwrap();
        let view = db.palpo_agent_lifecycle(&registration(), &id).unwrap();
        assert!(!view.matrix_retired);
        assert_eq!(view.allocated_tokens, 100);
    }
}

#[test]
fn conflicting_retirement_journal_never_counts_as_success() {
    let (root, mut db, id, _) = setup(true);
    db.revoke("revoke", &id).unwrap();
    local_complete(&mut db, &id);
    let target = db.palpo_retirement_target(&registration(), &id).unwrap();
    let mut other = target.clone();
    other.agent_mxid = "@different:example.test".into();
    let sql = rusqlite::Connection::open(root.path().join("state/domain.sqlite3")).unwrap();
    sql.execute(
        "INSERT INTO palpo_agent_retirements VALUES(?1,1,?2,2000)",
        rusqlite::params![id, serde_json::to_string(&other).unwrap()],
    )
    .unwrap();
    assert!(matches!(
        db.confirm_palpo_retirement(&registration(), &target, 2001),
        Err(Error::Conflict)
    ));
    assert!(
        !db.palpo_agent_lifecycle(&registration(), &id)
            .unwrap()
            .matrix_retired
    );
}
