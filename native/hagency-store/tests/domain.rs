mod common;
use common::*;
use hagency_core::tasks::*;
use hagency_core::{
    allocation::Tokens,
    project::{CleanupState, EngagementState, Seat},
};
use hagency_store::{DomainRepository, DomainStore, EffectOutcome, EffectState, Error};
use serde_json::json;

/// A Palpo fleet is imported with no reception; the verified probe binds it
/// once at the same generation, and a different room is then a conflict.
#[test]
fn native_probe_binds_an_unbound_reception_once() {
    let (_dir, mut db) = setup();
    let mut reg = registration();
    let bound = reg.reception_room_id.clone();
    reg.reception_room_id = String::new();
    reg.fleet_id = format!("hf_{}", "b".repeat(32));
    reg.representative_mxid = format!("@{}_representative:{}", reg.fleet_id, reg.server_name);
    db.register(&reg).unwrap();
    assert!(matches!(
        db.bind_reception(&reg.fleet_id, reg.generation + 1, &bound),
        Err(Error::Generation)
    ));
    db.bind_reception(&reg.fleet_id, reg.generation, &bound)
        .unwrap();
    assert_eq!(
        db.provisioning_registration(&reg.fleet_id)
            .unwrap()
            .reception_room_id,
        bound
    );
    db.bind_reception(&reg.fleet_id, reg.generation, &bound)
        .unwrap();
    let other = bound.replacen('!', "!other", 1);
    assert!(matches!(
        db.bind_reception(&reg.fleet_id, reg.generation, &other),
        Err(Error::Conflict)
    ));
}

/// An agent revoked while its provisioning was still running never published a
/// transport, so no worker will claim its retirement. It is listed for the
/// provisioning host until settled, and settling completes its cleanup.
#[test]
fn native_revoked_unattached_agent_lists_its_retirement_until_settled() {
    let (_dir, mut db) = setup();
    let pool = resource("unattached_account", "unattached_seat", 100);
    db.put_resource(&pool).unwrap();
    let request = request("unattached_one", "Unattached", &pool, 40);
    let proof = proof(&request);
    let fleet = registration().fleet_id;
    db.admit(&proof, 1000).unwrap();
    db.approve("unattached_approve", &proof, 1000).unwrap();
    let engagement = request.engagement_id().unwrap();
    db.claim_effect_for(&format!("provision_{engagement}"))
        .unwrap()
        .unwrap();
    assert!(
        db.pending_unattached_retirements(&fleet)
            .unwrap()
            .is_empty()
    );
    let revoked = db.revoke("unattached_revoke", &engagement).unwrap();
    assert_eq!(revoked.cleanup, CleanupState::Pending);
    assert_eq!(
        db.pending_unattached_retirements(&fleet).unwrap(),
        vec![engagement.clone()]
    );
    assert!(
        db.pending_unattached_retirements("hf_other_fleet")
            .unwrap()
            .is_empty()
    );
    let effect = db
        .claim_effect_for(&format!("retire_{engagement}"))
        .unwrap()
        .unwrap();
    let settled = db
        .observe_effect(
            &effect.id,
            effect.fence,
            &EffectOutcome::Applied {
                receipt: r#"{"credential":"none"}"#.into(),
            },
        )
        .unwrap();
    assert_eq!(settled.cleanup, CleanupState::Complete);
    assert!(
        db.pending_unattached_retirements(&fleet)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn native_remote_retirement_inspects_the_original_uncertain_effect_after_restart() {
    let (dir, mut db) = setup();
    let pool = resource("remote_account", "remote_seat", 100);
    db.put_resource(&pool).unwrap();
    let request = request("remote_one", "Remote", &pool, 40);
    let proof = proof(&request);
    let id = request.engagement_id().unwrap();
    db.admit(&proof, 1000).unwrap();
    db.approve("remote_approve", &proof, 1000).unwrap();
    let provision = db
        .claim_effect_for(&format!("provision_{id}"))
        .unwrap()
        .unwrap();
    assert!(db.inspect_retirement_effect(&id).unwrap().is_none());
    db.observe_effect(
        &provision.id,
        provision.fence,
        &EffectOutcome::Applied {
            receipt: "provisioned".into(),
        },
    )
    .unwrap();
    db.revoke("remote_revoke", &id).unwrap();
    let effect = db
        .claim_effect_for(&format!("retire_{id}"))
        .unwrap()
        .unwrap();
    db.observe_effect(&effect.id, effect.fence, &EffectOutcome::Unknown)
        .unwrap();
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    let inspected = db.inspect_retirement_effect(&id).unwrap().unwrap();
    assert_eq!(inspected.id, effect.id);
    assert_eq!(inspected.fence, effect.fence);
    assert_eq!(inspected.payload, effect.payload);
    assert!(db.claim_effect_for(&effect.id).unwrap().is_none());
    assert_eq!(
        db.pending_identity_retirements(&registration().fleet_id)
            .unwrap(),
        vec![id.clone()]
    );
    assert!(
        db.pending_identity_retirements("hf_other")
            .unwrap()
            .is_empty()
    );
    let outcome = EffectOutcome::Applied {
        receipt: "verified original identity".into(),
    };
    assert_eq!(
        db.observe_effect(&effect.id, effect.fence, &outcome)
            .unwrap()
            .cleanup,
        CleanupState::Complete
    );
    assert!(db.inspect_retirement_effect(&id).unwrap().is_none());
    assert!(
        db.pending_identity_retirements(&registration().fleet_id)
            .unwrap()
            .is_empty()
    );
    db.observe_effect(&effect.id, effect.fence, &outcome)
        .unwrap();
}

/// A console verdict reserves the engagement and queues its provision effect
/// without claiming it; the host reads that backlog on its next turn and it
/// leaves the list the moment the effect is claimed.
#[test]
fn native_console_approval_leaves_a_pending_provision() {
    let (_dir, mut db) = setup();
    let pool = resource("console_account", "console_seat", 100);
    db.put_resource(&pool).unwrap();
    let request = request("console_one", "Console", &pool, 40);
    let proof = proof(&request);
    let fleet = registration().fleet_id;
    db.admit(&proof, 1000).unwrap();
    assert!(db.pending_provisions(&fleet).unwrap().is_empty());
    db.approve("console_approve", &proof, 1000).unwrap();
    let engagement = request.engagement_id().unwrap();
    assert_eq!(
        db.pending_provisions(&fleet).unwrap(),
        vec![engagement.clone()]
    );
    assert!(db.pending_provisions("hf_other_fleet").unwrap().is_empty());
    db.claim_effect_for(&format!("provision_{engagement}"))
        .unwrap()
        .unwrap();
    assert!(db.pending_provisions(&fleet).unwrap().is_empty());
}

#[tokio::test]
async fn native_provision_account_current_scope() {
    let (dir, mut db) = setup();
    let pool = resource("physical_account", "physical_seat", 100);
    db.put_resource(&pool).unwrap();
    let request = request("physical_one", "Physical", &pool, 40);
    let proof = proof(&request);
    db.admit(&proof, 1000).unwrap();
    db.approve("physical_approve", &proof, 1000).unwrap();
    let effect = db
        .claim_effect_for(&format!("provision_{}", request.engagement_id().unwrap()))
        .unwrap()
        .unwrap();
    let store = DomainStore::start(db, 16).unwrap();
    store
        .validate_provision_account(effect.clone(), registration())
        .await
        .unwrap();
    for kind in [
        "id",
        "fence",
        "payload",
        "engagement",
        "kind",
        "state",
        "registration",
    ] {
        let mut changed = effect.clone();
        let mut reg = registration();
        match kind {
            "id" => changed.id = "missing_effect".into(),
            "fence" => changed.fence += 1,
            "payload" => changed.payload["substituted"] = true.into(),
            "engagement" => changed.engagement_id = "en_substituted".into(),
            "kind" => changed.kind = "retire".into(),
            "state" => changed.state = EffectState::Pending,
            _ => reg.generation += 1,
        }
        assert!(
            store
                .validate_provision_account(changed, reg)
                .await
                .is_err(),
            "{kind}"
        );
    }
    let inspection = rusqlite::Connection::open(dir.path().join("state/domain.sqlite3")).unwrap();
    inspection
        .execute("UPDATE effects SET fence=fence+1 WHERE id=?1", [&effect.id])
        .unwrap();
    assert!(
        store
            .validate_provision_account(effect.clone(), registration())
            .await
            .is_err()
    );
    inspection
        .execute(
            "UPDATE effects SET fence=?1 WHERE id=?2",
            rusqlite::params![effect.fence, effect.id],
        )
        .unwrap();
    let payload = serde_json::to_string(&effect.payload).unwrap();
    inspection
        .execute(
            "UPDATE effects SET payload=?1 WHERE id=?2",
            rusqlite::params!["{}", effect.id],
        )
        .unwrap();
    assert!(
        store
            .validate_provision_account(effect.clone(), registration())
            .await
            .is_err()
    );
    inspection
        .execute(
            "UPDATE effects SET payload=?1 WHERE id=?2",
            rusqlite::params![payload, effect.id],
        )
        .unwrap();
    // Restore only synthetic fixture mutations; production never restores a
    // changed fence/payload or uses these tests as a retry/repair operation.
    store
        .validate_provision_account(effect.clone(), registration())
        .await
        .unwrap();
    store
        .revoke("physical_revoke".into(), effect.engagement_id.clone())
        .await
        .unwrap();
    assert!(
        store
            .validate_provision_account(effect.clone(), registration())
            .await
            .is_err()
    );
    store.shutdown().await.unwrap();
    let mut reopened = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert!(
        reopened
            .validate_provision_account(&effect, &registration())
            .is_err()
    );
    assert_eq!(
        reopened.get(&effect.engagement_id).unwrap().state,
        EngagementState::Revoked
    );
    assert_ne!(
        reopened.effect(&effect.id).unwrap().state,
        EffectState::Complete
    );
}

fn setup() -> (tempfile::TempDir, DomainRepository) {
    let dir = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    (dir, db)
}
#[test]
fn domain_request_replay_and_publication() {
    let (_dir, mut db) = setup();
    let mut pool = resource("preset", "seat", 0);
    db.put_resource(&pool).unwrap();
    assert_eq!(db.catalog("", 100).unwrap().len(), 1); // Publication defaults on.
    let a = request("one", "E\u{301}dison", &pool, 100);
    let original = db.admit(&proof(&a), 1000).unwrap(); // Zero budget does not deny intake.
    assert_eq!(original.agent_name.as_str(), "Édison");
    assert_eq!(original.project_name.as_deref(), Some("实际项目名称"));
    assert_eq!(
        db.resource_budget(&pool.id()).unwrap().pool.committed,
        Tokens::default()
    );
    assert_eq!(value(db.admit(&proof(&a), 1001).unwrap()), value(&original));
    let mut changed = a.clone();
    changed.requested_tokens = 101.try_into().unwrap();
    assert!(matches!(
        db.admit(&proof(&changed), 1001),
        Err(Error::Conflict)
    ));
    let mut duplicate = request("two", "Édison", &pool, 100);
    assert!(matches!(
        db.admit(&proof(&duplicate), 1000),
        Err(Error::Conflict)
    ));
    duplicate.target_project_id = "another_project".into();
    duplicate.target_room_id = "!another:example.test".into();
    let other = db.admit(&proof(&duplicate), 1000).unwrap();
    assert_ne!(other.runtime_name, original.runtime_name);
    pool.published = false;
    db.put_resource(&pool).unwrap();
    assert!(db.catalog("", 100).unwrap().is_empty());
    assert_eq!(value(db.admit(&proof(&a), 1000).unwrap()), value(&original));
    let novel = request("three", "小白", &pool, 100);
    assert!(matches!(
        db.admit(&proof(&novel), 1000),
        Err(Error::Unqualified)
    ));
    assert!(matches!(
        db.approve("approve", &proof(&a), 1000),
        Err(Error::Unqualified)
    ));
    pool.published = true;
    pool.reasoning = Some("low".into());
    db.put_resource(&pool).unwrap();
    assert!(matches!(
        db.approve("approve", &proof(&a), 1000),
        Err(Error::Unqualified)
    ));
    db.reject("reject", &original.id).unwrap();
    pool.reasoning = Some("medium".into());
    db.put_resource(&pool).unwrap();
    db.admit(&proof(&request("replacement", "Édison", &pool, 1)), 1000)
        .unwrap();
    let projection = serde_json::to_string(&db.engagements("", 100).unwrap()).unwrap();
    for private in [
        "ownerDmRoomId",
        "!private:",
        "presetId",
        "seatId",
        "@owner:",
    ] {
        assert!(!projection.contains(private));
    }
}

#[tokio::test]
async fn domain_reservations_are_atomic() {
    let (dir, mut db) = setup();
    let pool = resource("preset_a", "shared", 100);
    let other = resource("preset_b", "shared", 100);
    db.put_resource(&pool).unwrap();
    db.put_resource(&other).unwrap();
    let seat: Seat = serde_json::from_value(
        json!({"id":"shared","declaration":{"quotaTokens":150,"period":"monthly"}}),
    )
    .unwrap();
    db.put_seat(&seat).unwrap();
    let a = request("one", "小白", &pool, 60);
    let b = request("two", "edison", &pool, 60);
    let c = request("three", "other", &other, 100);
    for request in [&a, &b, &c] {
        db.admit(&proof(request), 1000).unwrap();
    }
    // Trigger failure after the engagement was mutated but before the outbox intent exists.
    let inspection = rusqlite::Connection::open(dir.path().join("state/domain.sqlite3")).unwrap();
    inspection.execute_batch("CREATE TRIGGER fail_outbox BEFORE INSERT ON effects BEGIN SELECT RAISE(ABORT,'fixture disk failure'); END;").unwrap();
    assert!(db.approve("fail", &proof(&a), 1000).is_err());
    assert_eq!(
        db.get(&a.engagement_id().unwrap()).unwrap().state,
        EngagementState::Pending
    );
    assert_eq!(
        u64::from(db.resource_budget(&pool.id()).unwrap().pool.committed),
        0
    );
    assert_eq!(
        inspection
            .query_row("SELECT COUNT(*) FROM decisions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    inspection
        .execute_batch("DROP TRIGGER fail_outbox")
        .unwrap();
    let store = DomainStore::start(db, 16).unwrap();
    // Two asynchronous approvals race through the actual single transactional writer.
    let (first, second) = tokio::join!(
        store.approve("approve_a".into(), proof(&a), 1000),
        store.approve("approve_b".into(), proof(&b), 1000)
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let (winner, command, request) = if let Ok(a_result) = first {
        // The pool ceiling is declared, so the loser is an over-commit with the
        // binding-draw wording, not an unknown-capacity refusal.
        assert!(matches!(second, Err(Error::OverCommit { .. })));
        (a_result, "approve_a", &a)
    } else {
        (second.unwrap(), "approve_b", &b)
    };
    assert_eq!(
        value(
            store
                .approve(command.into(), proof(request), 1000)
                .await
                .unwrap()
        ),
        value(&winner)
    );
    let budget = store.resource_budget(pool.id()).await.unwrap();
    assert_eq!(u64::from(budget.pool.committed), 60);
    assert_eq!(u64::from(budget.remaining_tokens.unwrap()), 40);
    // Other pool still has 100 but its shared seat has only 90: the seat is
    // the binding side, which is the resource-pool refusal this identity
    // predates the ceiling split for and still carries.
    assert!(matches!(
        store.approve("approve_c".into(), proof(&c), 1000).await,
        Err(Error::InsufficientCapacity)
    ));
    store
        .put_seat(Seat {
            id: "shared".into(),
            declaration: None,
        })
        .await
        .unwrap();
    store
        .approve("approve_c".into(), proof(&c), 1000)
        .await
        .unwrap(); // No invented seat quota.
    let budget = store.resource_budget(pool.id()).await.unwrap();
    assert_eq!(u64::from(budget.remaining_tokens.unwrap()), 40);
    assert_eq!(u64::from(budget.seat.committed), 160);
    assert!(budget.seat.remaining.is_none());
    store
        .revoke("revoke".into(), winner.id.clone())
        .await
        .unwrap();
    assert_eq!(
        u64::from(
            store
                .resource_budget(other.id())
                .await
                .unwrap()
                .pool
                .committed
        ),
        100
    );
    store.shutdown().await.unwrap();
    let reopened = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert_eq!(
        reopened.get(&winner.id).unwrap().state,
        EngagementState::Revoked
    );
    assert_eq!(
        reopened.get(&c.engagement_id().unwrap()).unwrap().state,
        EngagementState::Reserved
    );
}

#[test]
fn domain_effect_recovery_and_revocation() {
    let (dir, mut db) = setup();
    let pool = resource("preset", "seat", 100);
    db.put_resource(&pool).unwrap();
    let request = request("one", "小白", &pool, 50);
    let proof = proof(&request);
    let engagement = db.admit(&proof, 1000).unwrap();
    db.approve("approve", &proof, 1000).unwrap();
    let effect = db.claim_effect().unwrap().unwrap();
    assert_eq!(effect.state, EffectState::Started);
    assert_eq!(effect.payload["runtimeName"], engagement.runtime_name);
    assert!(db.claim_effect().unwrap().is_none());
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert_eq!(db.effect(&effect.id).unwrap().state, EffectState::Uncertain);
    assert!(db.claim_effect().unwrap().is_none());
    assert_eq!(
        u64::from(db.resource_budget(&pool.id()).unwrap().pool.committed),
        50
    );
    assert!(matches!(
        db.observe_effect(
            &effect.id,
            effect.fence + 1,
            &EffectOutcome::Applied {
                receipt: "observed_identity".into()
            }
        ),
        Err(Error::Generation)
    ));
    db.observe_effect(&effect.id, effect.fence, &EffectOutcome::Unknown)
        .unwrap();
    let observed = EffectOutcome::Applied {
        receipt: "observed_identity".into(),
    };
    assert_eq!(
        db.observe_effect(&effect.id, effect.fence, &observed)
            .unwrap()
            .state,
        EngagementState::Active
    );
    assert_eq!(
        db.observe_effect(&effect.id, effect.fence, &observed)
            .unwrap()
            .state,
        EngagementState::Active
    );
    let revoked = db.revoke("revoke", &engagement.id).unwrap();
    assert_eq!(revoked.cleanup, CleanupState::Pending);
    assert_eq!(
        u64::from(db.resource_budget(&pool.id()).unwrap().pool.committed),
        0
    );
    assert!(matches!(
        db.observe_effect(&effect.id, effect.fence, &observed),
        Err(Error::Generation)
    ));
    let cleanup = db.claim_effect().unwrap().unwrap();
    assert_eq!(cleanup.kind, "retire");
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert_eq!(
        db.get(&engagement.id).unwrap().cleanup,
        CleanupState::Uncertain
    );
    assert!(db.claim_effect().unwrap().is_none());
    assert_eq!(
        db.observe_effect(&cleanup.id, cleanup.fence, &EffectOutcome::Unknown)
            .unwrap()
            .cleanup,
        CleanupState::Uncertain
    );
    assert!(db.claim_effect().unwrap().is_none());
    assert!(matches!(
        db.retry_cleanup("premature_retry", &engagement.id),
        Err(Error::State)
    ));
    db.observe_effect(
        &cleanup.id,
        cleanup.fence,
        &EffectOutcome::NotApplied {
            receipt: "confirmed_no_retirement_applied".into(),
        },
    )
    .unwrap();
    assert!(db.claim_effect().unwrap().is_none());
    db.retry_cleanup("retry_cleanup", &engagement.id).unwrap();
    let retried = db.claim_effect().unwrap().unwrap();
    assert!(retried.fence > cleanup.fence);
    assert!(matches!(
        db.observe_effect(&cleanup.id, cleanup.fence, &EffectOutcome::Unknown),
        Err(Error::Generation)
    ));
    let completed = db
        .observe_effect(
            &cleanup.id,
            retried.fence,
            &EffectOutcome::Applied {
                receipt: "observed_deactivated_without_erasing_history".into(),
            },
        )
        .unwrap();
    assert_eq!(completed.cleanup, CleanupState::Complete);
    assert_eq!(db.engagements("", 100).unwrap().len(), 1);
    assert_eq!(
        value(db.revoke("revoke", &engagement.id).unwrap()),
        value(revoked)
    );
    assert_eq!(
        db.get(&engagement.id).unwrap().cleanup,
        CleanupState::Complete
    );
}

#[tokio::test]
async fn native_provision_claim_exact_owner() {
    let (dir, mut db) = setup();
    let pool = resource("inline_preset", "inline_seat", 100);
    db.put_resource(&pool).unwrap();
    let first = request("inline_one", "InlineOne", &pool, 40);
    let second = request("inline_two", "InlineTwo", &pool, 40);
    for (index, request) in [&first, &second].iter().enumerate() {
        let proof = proof(request);
        db.admit(&proof, 1000).unwrap();
        db.approve(&format!("inline_approve_{index}"), &proof, 1000)
            .unwrap();
    }
    let mut ids = [
        format!("provision_{}", first.engagement_id().unwrap()),
        format!("provision_{}", second.engagement_id().unwrap()),
    ];
    ids.sort();
    let untouched = db.effect(&ids[0]).unwrap();
    let store = DomainStore::start(db, 16).unwrap();
    assert!(
        store
            .claim_effect_for("missing_effect".into())
            .await
            .unwrap()
            .is_none()
    );
    let (a, b) = tokio::join!(
        store.claim_effect_for(ids[1].clone()),
        store.claim_effect_for(ids[1].clone())
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(usize::from(a.is_some()) + usize::from(b.is_some()), 1);
    let owned = a.or(b).unwrap();
    assert_eq!(owned.id, ids[1]);
    assert_eq!(owned.state, EffectState::Started);
    assert_eq!(owned.fence, 1);
    assert!(
        store
            .claim_effect_for(ids[1].clone())
            .await
            .unwrap()
            .is_none()
    );
    // A real claim still has no physical outcome. Shutdown/reopen preserves
    // original uncertainty rather than fabricating an Active engagement.
    store.shutdown().await.unwrap();
    let mut reopened = DomainRepository::open(&dir.path().join("state")).unwrap();
    let other = reopened.effect(&ids[0]).unwrap();
    assert_eq!(other.state, untouched.state);
    assert_eq!(other.fence, untouched.fence);
    assert_eq!(
        reopened.effect(&owned.id).unwrap().state,
        EffectState::Uncertain
    );
    assert_eq!(
        reopened.get(&owned.engagement_id).unwrap().state,
        EngagementState::Reserved
    );
    assert!(reopened.claim_effect_for(&owned.id).unwrap().is_none());
    assert_eq!(
        reopened.claim_effect_for(&ids[0]).unwrap().unwrap().id,
        ids[0]
    );
}

#[test]
fn native_provision_claim_recovery_fences() {
    let (dir, mut db) = setup();
    let pool = resource("inline_preset", "inline_seat", 100);
    db.put_resource(&pool).unwrap();
    let first = request("inline_one", "InlineOne", &pool, 40);
    let second = request("inline_two", "InlineTwo", &pool, 40);
    for (index, request) in [&first, &second].iter().enumerate() {
        let proof = proof(request);
        db.admit(&proof, 1000).unwrap();
        db.approve(&format!("inline_approve_{index}"), &proof, 1000)
            .unwrap();
    }
    let id = format!("provision_{}", first.engagement_id().unwrap());
    let other_id = format!("provision_{}", second.engagement_id().unwrap());
    let sql = rusqlite::Connection::open(dir.path().join("state/domain.sqlite3")).unwrap();
    // Bound fixture IDs come from the actual domain, never external SQL text.
    sql.execute_batch(&format!("CREATE TRIGGER inline_claim_abort BEFORE UPDATE ON effects WHEN OLD.id='{id}' AND NEW.state='started' BEGIN SELECT RAISE(ABORT,'fixture inline claim rollback'); END;")).unwrap();
    assert!(db.claim_effect_for(&id).is_err());
    assert_eq!(db.effect(&id).unwrap().state, EffectState::Pending);
    assert_eq!(db.effect(&id).unwrap().fence, 0);
    assert_eq!(db.effect(&other_id).unwrap().state, EffectState::Pending);
    assert_eq!(db.effect(&other_id).unwrap().fence, 0);
    sql.execute_batch("DROP TRIGGER inline_claim_abort")
        .unwrap();
    sql.execute(
        "UPDATE effects SET fence=?2 WHERE id=?1",
        rusqlite::params![id, hagency_core::JSON_SAFE_MAX],
    )
    .unwrap();
    assert!(matches!(db.claim_effect_for(&id), Err(Error::State)));
    assert_eq!(db.effect(&id).unwrap().state, EffectState::Pending);
    assert_eq!(db.effect(&id).unwrap().fence, hagency_core::JSON_SAFE_MAX);
    assert_eq!(db.effect(&other_id).unwrap().fence, 0);
    assert!(db.claim_effect_for("").is_err());
    assert!(db.claim_effect_for(&"x".repeat(129)).is_err());
    assert!(db.claim_effect_for("bad/effect").is_err());
    // Revocation cancels only the original provision and creates its separate
    // retire intent. Exact provision lookup must not claim that cleanup.
    db.revoke("inline_revoke", &second.engagement_id().unwrap())
        .unwrap();
    assert!(db.claim_effect_for(&other_id).unwrap().is_none());
    let mut next = registration();
    next.generation += 1;
    db.register(&next).unwrap();
    // Old-registration pending custody is not available, even with capacity
    // to increment its restored fixture fence.
    sql.execute("UPDATE effects SET fence=0 WHERE id=?1", [&id])
        .unwrap();
    assert!(db.claim_effect_for(&id).unwrap().is_none());
    assert_eq!(db.effect(&id).unwrap().state, EffectState::Pending);
    assert_eq!(db.effect(&id).unwrap().fence, 0);
}

#[test]
fn native_ceiling_no_ceiling_distinct_from_over_commit() {
    let (_dir, mut db) = setup();
    // Unknown capacity is not an over-commit: a declared ceiling whose seat
    // declaration mismatches the pool period leaves remaining unknown, which
    // the retained JavaScript refuses as no_ceiling.
    let pool = resource("preset_no_ceiling", "mismatched_seat", 100);
    db.put_resource(&pool).unwrap();
    let seat: Seat = serde_json::from_value(
        json!({"id":"mismatched_seat","declaration":{"quotaTokens":50,"period":"daily"}}),
    )
    .unwrap();
    db.put_seat(&seat).unwrap();
    let a = request("no_ceiling", "NoCeilingWorker", &pool, 10);
    db.admit(&proof(&a), 1000).unwrap();
    match db.approve("approve_no_ceiling", &proof(&a), 1000) {
        Err(Error::NoCeiling) => {}
        Err(Error::OverCommit { message }) => {
            panic!("unknown capacity must not carry over-commit wording: {message}")
        }
        Err(Error::InsufficientCapacity) => {
            panic!("unknown capacity must be named, not the pool-refusal variant")
        }
        other => panic!("expected NoCeiling, got {other:?}"),
    }
    // The mirror on the same store: a known ceiling the allocation exceeds
    // refuses as over_commit and names the draw, the preset and the period.
    let pool = resource("preset_over_commit", "aligned_seat", 100);
    db.put_resource(&pool).unwrap();
    let b = request("over_commit", "OverCommitWorker", &pool, 500);
    db.admit(&proof(&b), 1000).unwrap();
    match db.approve("approve_over_commit", &proof(&b), 1000) {
        Err(Error::NoCeiling) => panic!("a declared ceiling must not read as unknown"),
        Err(Error::OverCommit { message }) => {
            assert!(message.contains("would exceed the 100 left on OverCommitWorker"));
            assert!(message.contains("its ceiling is 100 per monthly"));
            assert!(message.contains("nothing has been measured"));
            assert!(message.contains("preset_over_commit"));
        }
        Err(Error::InsufficientCapacity) => {
            panic!("a ceiling exceeded by the allocation must be named, not pooled")
        }
        other => panic!("expected OverCommit, got {other:?}"),
    }
}

/// The agent roster (board #22, TS `backend-v2.js:11696`): one row per
/// AGENT — every agent the service knows appears, including one whose
/// engagement is admit-only — with `online` derived from REAL worker
/// state (a live dispatch in one of the agent's sessions) and
/// `last_seen_ms` from the newest attempt clock, null never zero.
#[test]
fn domain_agent_roster_is_agent_keyed_with_real_worker_state() {
    let (_dir, mut db) = setup();
    let pool = resource("roster_pool", "roster_seat", 1000);
    db.put_resource(&pool).unwrap();
    // OnlineWorker: admitted, approved, effect observed, session, task,
    // dispatch claimed and STARTED — the attempt clock is 1003.
    let online_proof = proof(&request("roster_one", "OnlineWorker", &pool, 100));
    let engagement = db.admit(&online_proof, 1000).unwrap().id;
    db.approve("approve_roster_one", &online_proof, 1000)
        .unwrap();
    let effect = db.claim_effect().unwrap().unwrap();
    db.observe_effect(
        &effect.id,
        effect.fence,
        &EffectOutcome::Applied {
            receipt: "roster fixture".into(),
        },
    )
    .unwrap();
    db.register_session(&SessionBinding {
        id: "roster_session".into(),
        engagement_id: engagement.clone(),
        room_id: "!project:example.test".into(),
        thread_root: None,
    })
    .unwrap();
    db.create_canonical_task("roster_task", "roster_session", "Roster work", 1001)
        .unwrap();
    db.register_workspace("roster_workspace").unwrap();
    db.enqueue_dispatch(&DispatchInput {
        id: "roster_dispatch".into(),
        session_id: "roster_session".into(),
        task_id: Some("roster_task".into()),
        resources: vec![ResourceLease {
            id: "roster_workspace".into(),
            exclusive: true,
        }],
        payload: json!({}),
    })
    .unwrap();
    let cap = db
        .claim_dispatch("roster_runner", 1002, 60000, 120000, 128)
        .unwrap()
        .unwrap();
    let scope = db.owned_dispatch_scope(&cap, 1003).unwrap();
    db.start_owned_dispatch(&cap, scope.fingerprint(), 1004)
        .unwrap();
    // IdleWorker: admit-only, no session — known but never staffed.
    db.admit(
        &proof(&request("roster_two", "IdleWorker", &pool, 100)),
        1000,
    )
    .unwrap();

    let roster = db.agent_roster().unwrap();
    assert_eq!(roster.len(), 2, "one row per agent the service knows");
    let online = roster.iter().find(|r| r.name == "OnlineWorker").unwrap();
    assert_eq!(online.state, EngagementState::Active);
    assert!(
        online.online,
        "a live started dispatch is real worker state"
    );
    assert_eq!(online.last_seen_ms, Some(1002), "the newest attempt clock");
    assert_eq!(online.last_activity_ms, Some(1002));
    assert_eq!(online.engagement_id, engagement);
    let idle = roster.iter().find(|r| r.name == "IdleWorker").unwrap();
    assert_eq!(idle.state, EngagementState::Pending);
    assert!(!idle.online, "no session means no live dispatch");
    assert_eq!(idle.last_seen_ms, None, "never attempted: null, not zero");
    assert_eq!(idle.last_activity_ms, None);
}

/// The agent detail (board #22, TS `backend-v2.js:12155`): the agent-keyed
/// identity plus resource, rooms, current dispatch and recent tasks; an
/// unknown agent is None (the route's 404).
#[test]
fn domain_agent_detail_projects_rooms_dispatch_and_tasks() {
    let (_dir, mut db) = setup();
    let pool = resource("detail_pool", "detail_seat", 1000);
    db.put_resource(&pool).unwrap();
    assert!(
        db.agent_detail("Nobody").unwrap().is_none(),
        "unknown agent: 404"
    );
    let detail_proof = proof(&request("detail_one", "DetailWorker", &pool, 100));
    let engagement = db.admit(&detail_proof, 1000).unwrap().id;
    db.approve("approve_detail_one", &detail_proof, 1000)
        .unwrap();
    let effect = db.claim_effect().unwrap().unwrap();
    db.observe_effect(
        &effect.id,
        effect.fence,
        &EffectOutcome::Applied {
            receipt: "detail fixture".into(),
        },
    )
    .unwrap();
    db.register_session(&SessionBinding {
        id: "detail_session".into(),
        engagement_id: engagement.clone(),
        room_id: "!project:example.test".into(),
        thread_root: None,
    })
    .unwrap();
    db.create_canonical_task("detail_task", "detail_session", "Detail work", 1001)
        .unwrap();
    db.register_workspace("detail_workspace").unwrap();
    db.enqueue_dispatch(&DispatchInput {
        id: "detail_dispatch".into(),
        session_id: "detail_session".into(),
        task_id: Some("detail_task".into()),
        resources: vec![ResourceLease {
            id: "detail_workspace".into(),
            exclusive: true,
        }],
        payload: json!({}),
    })
    .unwrap();
    let cap = db
        .claim_dispatch("detail_runner", 1002, 60000, 120000, 128)
        .unwrap()
        .unwrap();
    let scope = db.owned_dispatch_scope(&cap, 1003).unwrap();
    db.start_owned_dispatch(&cap, scope.fingerprint(), 1004)
        .unwrap();

    let detail = db.agent_detail("DetailWorker").unwrap().unwrap();
    assert_eq!(detail.name, "DetailWorker");
    assert_eq!(detail.state, EngagementState::Active);
    assert!(detail.online);
    assert_eq!(detail.last_seen_ms, Some(1002));
    assert_eq!(detail.engagements, 1);
    assert_eq!(detail.engagement_id, engagement);
    assert_eq!(detail.rooms.len(), 1);
    assert_eq!(detail.rooms[0].room_id, "!project:example.test");
    assert_eq!(detail.rooms[0].dispatch_state.as_deref(), Some("started"));
    let dispatch = detail.dispatch.as_ref().unwrap();
    assert_eq!(dispatch.dispatch_state.as_deref(), Some("started"));
    assert_eq!(dispatch.dispatch_id.as_deref(), Some("detail_dispatch"));
    assert_eq!(detail.tasks.len(), 1);
    assert_eq!(detail.tasks[0].id, "detail_task");
    assert_eq!(detail.tasks[0].title, "Detail work");
    // Admit-only agent: known, no rooms, no dispatch, no tasks.
    db.admit(
        &proof(&request("detail_two", "QuietWorker", &pool, 100)),
        1000,
    )
    .unwrap();
    let quiet = db.agent_detail("QuietWorker").unwrap().unwrap();
    assert!(!quiet.online);
    assert_eq!(quiet.last_seen_ms, None);
    assert!(quiet.dispatch.is_none());
    assert!(quiet.rooms.is_empty());
    assert!(quiet.tasks.is_empty());
}

/// ADR-187 §C: the first master key observed for an owner is pinned; the same
/// key is accepted again; a different key is refused and recorded for the
/// operator, never adopted; only an operator re-pin changes the anchor.
#[test]
fn native_owner_anchor_is_pinned_on_first_use_and_never_replaced() {
    let (_dir, mut db) = setup();
    let owner = "@owner:example.test";
    let first = "A".repeat(43);
    let other = "B".repeat(43);
    assert!(db.owner_anchor(owner).unwrap().is_none());
    let pinned = db.observe_owner_anchor(owner, &first, 10).unwrap();
    assert_eq!(
        (pinned.master_key.as_str(), pinned.source.as_str()),
        (first.as_str(), "first_use")
    );
    assert_eq!(
        db.observe_owner_anchor(owner, &first, 20)
            .unwrap()
            .pinned_at,
        10
    );
    assert!(matches!(
        db.observe_owner_anchor(owner, &other, 30),
        Err(Error::Conflict)
    ));
    let held = db.owner_anchor(owner).unwrap().unwrap();
    assert_eq!(
        held.master_key, first,
        "a different key never replaces the pin"
    );
    assert_eq!(held.mismatch_key.as_deref(), Some(other.as_str()));
    let repinned = db.repin_owner_anchor(owner, &other, 40).unwrap();
    assert_eq!(
        (repinned.master_key.as_str(), repinned.source.as_str()),
        (other.as_str(), "operator")
    );
    assert_eq!(repinned.mismatch_key, None);
    assert!(db.observe_owner_anchor("not-an-mxid", &first, 50).is_err());
    assert!(db.observe_owner_anchor(owner, "short", 50).is_err());
    assert_eq!(db.owner_anchors().unwrap().len(), 1);
}
