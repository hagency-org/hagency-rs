//! Rinx ADR 0010: real SQLite reservations, scoped decisions and replay.
mod common;
use common::*;
use hagency_core::{authority::VerifiedRequest, project::EngagementState, project_grants::*};
use hagency_store::{DomainRepository, DomainStore, Error, ProjectAgentDecision};

fn setup() -> (
    tempfile::TempDir,
    DomainRepository,
    ResourceDelegation,
    ProjectGrant,
) {
    let dir = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    let pool = resource("grant_pool", "grant_seat", 1000);
    db.put_resource(&pool).unwrap();
    let root = ResourceDelegation {
        v: 1,
        id: "contribution_one".into(),
        revision: 1,
        fleet_id: registration().fleet_id,
        registration_generation: 1,
        issuer: "example.test".into(),
        resource_id: pool.id(),
        limits: GrantLimits {
            tokens: 800,
            max_agents: 4,
            max_rate_per_day: 1000,
        },
        expires_at_ms: 10_000,
    };
    let grant = ProjectGrant {
        v: 1,
        id: "grant_one".into(),
        revision: 1,
        delegation_id: root.id.clone(),
        delegation_revision: 1,
        project_id: "project_one".into(),
        room_id: "!project:example.test".into(),
        owner_mxid: "@owner:example.test".into(),
        administrator_mxids: vec!["@projectadmin:example.test".into()],
        allow_self_approval: false,
        limits: GrantLimits {
            tokens: 500,
            max_agents: 3,
            max_rate_per_day: 500,
        },
        expires_at_ms: 5000,
    };
    (dir, db, root, grant)
}
fn decision() -> ProjectAgentDecision {
    ProjectAgentDecision {
        grant_id: "grant_one".into(),
        grant_revision: 1,
        actor_mxid: "@projectadmin:example.test".into(),
        requester_mxid: "@owner:example.test".into(),
    }
}
fn admitted(db: &mut DomainRepository, id: &str, name: &str, tokens: u64) -> VerifiedRequest {
    let mut r = request(
        id,
        name,
        &resource("grant_pool", "grant_seat", 1000),
        tokens,
    );
    r.rate_per_day = Some(50.try_into().unwrap());
    let p = proof(&r);
    db.admit(&p, 1000).unwrap();
    p
}
fn reserve(db: &mut DomainRepository, root: &ResourceDelegation, grant: &ProjectGrant) {
    db.delegate_resource(root, 1000).unwrap();
    db.reserve_project_grant(grant, &registration(), 1000)
        .unwrap();
}
#[test]
fn delegation_reserves_real_capacity_before_any_agent_exists() {
    let (_dir, mut db, root, grant) = setup();
    reserve(&mut db, &root, &grant);
    assert_eq!(
        db.resource_ceiling(&root.resource_id, 1000)
            .unwrap()
            .reserved,
        800
    );
    assert_eq!(
        u64::from(
            db.resource_budget(&root.resource_id)
                .unwrap()
                .pool
                .committed
        ),
        800
    );
    let legacy = admitted(&mut db, "legacy", "Legacy", 300);
    assert!(matches!(
        db.approve("manual", &legacy, 1000),
        Err(Error::GrantAuthority)
    ));
    let mut more = root.clone();
    more.id = "another_contribution".into();
    more.limits.tokens = 201;
    assert!(db.delegate_resource(&more, 1000).is_err());
    let mut project = grant.clone();
    project.id = "grant_two".into();
    project.project_id = "project_two".into();
    project.room_id = "!other:example.test".into();
    project.limits.tokens = 301;
    project.limits.max_agents = 1;
    assert!(matches!(
        db.reserve_project_grant(&project, &registration(), 1000),
        Err(Error::InsufficientCapacity)
    ));
}
#[test]
fn project_decision_and_top_up_consume_one_reservation_and_replay_once() {
    let (dir, mut db, root, grant) = setup();
    reserve(&mut db, &root, &grant);
    let p = admitted(&mut db, "request_one", "First", 200);
    let e = db
        .approve_project_agent("approve_one", &p, 1000, 200, &decision())
        .unwrap();
    assert_eq!(e.state, EngagementState::Reserved);
    assert_eq!(
        db.resource_ceiling(&root.resource_id, 1000)
            .unwrap()
            .reserved,
        800
    );
    assert_eq!(
        u64::from(
            db.resource_budget(&root.resource_id)
                .unwrap()
                .pool
                .committed
        ),
        800
    );
    assert_eq!(db.engagement_headroom(&e.id, 1000).unwrap(), Some(200));
    assert!(
        db.approve_project_agent("approve_one", &p, 1000, 200, &decision())
            .is_ok()
    );
    assert!(matches!(
        db.approve_project_agent("approve_one", &p, 1000, 201, &decision()),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        db.raise_allocation("manual_topup", &e.id, 10, 1000),
        Err(Error::GrantAuthority)
    ));
    let raised = db
        .raise_project_agent_allocation("topup_one", &e.id, 100, 1000, &decision())
        .unwrap();
    assert_eq!(u64::from(raised.allocation()), 300);
    assert_eq!(
        db.resource_ceiling(&root.resource_id, 1000)
            .unwrap()
            .reserved,
        800
    );
    drop(db);
    // Display-history retention must not erase a financial command's receipt.
    let sql = rusqlite::Connection::open(dir.path().join("state/domain.sqlite3")).unwrap();
    sql.execute("DELETE FROM decisions", []).unwrap();
    drop(sql);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert_eq!(
        u64::from(
            db.raise_project_agent_allocation("topup_one", &e.id, 100, 1000, &decision())
                .unwrap()
                .allocation()
        ),
        300
    );
    assert!(matches!(
        db.raise_project_agent_allocation("topup_over", &e.id, 201, 1000, &decision()),
        Err(Error::InsufficientCapacity)
    ));
    assert_eq!(u64::from(db.get(&e.id).unwrap().allocation()), 300);
    let sql = rusqlite::Connection::open(dir.path().join("state/domain.sqlite3")).unwrap();
    assert_eq!(
        sql.query_row(
            "SELECT SUM(debited_tokens) FROM project_grant_agents",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        300
    );
    assert_eq!(
        sql.query_row(
            "SELECT COUNT(*) FROM effects WHERE kind='provision'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        1
    );
}
#[test]
fn unassigned_actor_wrong_requester_cross_project_and_stale_revision_cannot_decide() {
    let (_dir, mut db, root, grant) = setup();
    reserve(&mut db, &root, &grant);
    let p = admitted(&mut db, "request_one", "First", 100);
    for invalid in [
        ProjectAgentDecision {
            actor_mxid: "@serveradmin:example.test".into(),
            ..decision()
        },
        ProjectAgentDecision {
            requester_mxid: "@stranger:example.test".into(),
            ..decision()
        },
        ProjectAgentDecision {
            grant_revision: 2,
            ..decision()
        },
    ] {
        assert!(matches!(
            db.approve_project_agent("forged", &p, 1000, 100, &invalid),
            Err(Error::GrantAuthority)
        ));
    }
    assert!(matches!(
        db.approve("console_bypass", &p, 1000),
        Err(Error::GrantAuthority)
    ));
    let mut request = p.request().clone();
    request.target_project_id = "project_two".into();
    request.target_room_id = "!second:example.test".into();
    request.request_id = "cross".into();
    request.source_event_id = "$cross".into();
    let cross = proof(&request);
    db.admit(&cross, 1000).unwrap();
    assert!(matches!(
        db.approve_project_agent("cross", &cross, 1000, 100, &decision()),
        Err(Error::GrantAuthority)
    ));
    assert_eq!(
        db.get(&p.request().engagement_id().unwrap()).unwrap().state,
        EngagementState::Pending
    );
}
#[test]
fn self_approval_requires_an_explicit_project_policy() {
    let (_dir, mut db, root, mut grant) = setup();
    grant.administrator_mxids = vec![grant.owner_mxid.clone()];
    reserve(&mut db, &root, &grant);
    let p = admitted(&mut db, "self", "First", 100);
    let scope = ProjectAgentDecision {
        actor_mxid: grant.owner_mxid.clone(),
        ..decision()
    };
    assert!(matches!(
        db.approve_project_agent("self", &p, 1000, 100, &scope),
        Err(Error::GrantAuthority)
    ));
    let (_dir, mut allowed, root, mut grant) = setup();
    grant.allow_self_approval = true;
    grant.administrator_mxids = vec![grant.owner_mxid.clone()];
    reserve(&mut allowed, &root, &grant);
    let p = admitted(&mut allowed, "self", "First", 100);
    assert!(
        allowed
            .approve_project_agent("self", &p, 1000, 100, &scope)
            .is_ok()
    );
}
#[test]
fn expiry_revocation_and_registration_rotation_fence_new_decisions_without_freeing_capacity() {
    let (_dir, mut db, root, grant) = setup();
    reserve(&mut db, &root, &grant);
    let p = admitted(&mut db, "pending", "First", 100);
    assert!(matches!(
        db.approve_project_agent("expired", &p, 5000, 100, &decision()),
        Err(Error::GrantExpired)
    ));
    db.revoke_resource_delegation(&root.id, 1, 1001).unwrap();
    assert!(matches!(
        db.approve_project_agent("revoked", &p, 1001, 100, &decision()),
        Err(Error::GrantRevoked)
    ));
    assert_eq!(
        db.resource_ceiling(&root.resource_id, 1001)
            .unwrap()
            .reserved,
        800
    );
    assert!(matches!(
        db.delegate_resource(&root, 1001),
        Err(Error::GrantRevoked)
    ));
    let (_dir, mut db, root, grant) = setup();
    reserve(&mut db, &root, &grant);
    let mut reg = registration();
    reg.generation = 2;
    db.register(&reg).unwrap();
    assert!(db.reserve_project_grant(&grant, &reg, 1001).is_err());
    assert_eq!(
        db.resource_ceiling(&root.resource_id, 1001)
            .unwrap()
            .reserved,
        800
    );
}
#[test]
fn finite_wire_limits_and_grant_identity_are_not_optional() {
    let (_dir, mut db, root, grant) = setup();
    reserve(&mut db, &root, &grant);
    let mut altered = grant.clone();
    altered.limits.tokens += 1;
    assert!(matches!(
        db.reserve_project_grant(&altered, &registration(), 1000),
        Err(Error::Conflict)
    ));
    let mut invalid = root.clone();
    invalid.limits.tokens = 0;
    assert!(invalid.validate(&registration(), 1000).is_err());
    invalid.limits.tokens = hagency_core::JSON_SAFE_MAX + 1;
    assert!(invalid.validate(&registration(), 1000).is_err());
    let mut value = serde_json::to_value(&root).unwrap();
    value.as_object_mut().unwrap().remove("limits");
    assert!(serde_json::from_value::<ResourceDelegation>(value).is_err());
}
#[tokio::test]
async fn concurrent_decisions_cannot_overdraw_the_same_project_grant() {
    let (_dir, mut db, mut root, mut grant) = setup();
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    root.expires_at_ms = now + 60_000;
    grant.expires_at_ms = now + 30_000;
    grant.limits.tokens = 100;
    reserve(&mut db, &root, &grant);
    let a = admitted(&mut db, "a", "First", 70);
    let b = admitted(&mut db, "b", "Second", 70);
    let fresh = |p: VerifiedRequest| {
        let mut facts = observation(p.request());
        facts.observed_at_ms = now;
        hagency_core::authority::verify_request(&registration(), p.request().clone(), facts)
            .unwrap()
    };
    let a = fresh(a);
    let b = fresh(b);
    let store = DomainStore::start(db, 8).unwrap();
    let (first, second) = tokio::join!(
        store.approve_project_agent("approve_a".into(), a, 70, decision()),
        store.approve_project_agent("approve_b".into(), b, 70, decision())
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert!(
        matches!(first, Err(Error::InsufficientCapacity))
            || matches!(second, Err(Error::InsufficientCapacity))
    );
    store.shutdown().await.unwrap();
}

#[test]
fn revocation_cancels_queued_provision_and_fences_late_completion() {
    use hagency_core::project::CleanupState;
    use hagency_store::{EffectOutcome, EffectState};
    for started in [false, true] {
        let (dir, mut db, root, grant) = setup();
        reserve(&mut db, &root, &grant);
        let p = admitted(&mut db, "retirement", "Retirement", 200);
        let e = db
            .approve_project_agent("approve", &p, 1000, 200, &decision())
            .unwrap();
        let effect_id = format!("provision_{}", e.id);
        let claimed = if started {
            db.claim_effect_for_at(&effect_id, 1001).unwrap()
        } else {
            None
        };
        db.revoke_resource_delegation(&root.id, 1, 1002).unwrap();
        db.revoke_resource_delegation(&root.id, 1, 1003).unwrap();
        assert_eq!(db.get(&e.id).unwrap().state, EngagementState::Revoked);
        assert_eq!(db.effect(&effect_id).unwrap().state, EffectState::Cancelled);
        assert!(db.claim_effect_for_at(&effect_id, 1003).unwrap().is_none());
        if let Some(claimed) = claimed {
            assert!(
                db.observe_effect_at(
                    &effect_id,
                    claimed.fence,
                    &EffectOutcome::Applied {
                        receipt: "late".into()
                    },
                    1004
                )
                .is_err()
            );
            assert_eq!(db.get(&e.id).unwrap().cleanup, CleanupState::Pending);
            assert_eq!(
                db.effect(&format!("retire_{}", e.id)).unwrap().state,
                EffectState::Pending
            );
        } else {
            assert!(matches!(
                db.effect(&format!("retire_{}", e.id)),
                Err(Error::NotFound)
            ));
        }
        drop(db);
        let db = DomainRepository::open(&dir.path().join("state")).unwrap();
        assert_eq!(
            db.resource_ceiling(&root.resource_id, 1004)
                .unwrap()
                .reserved,
            800
        );
    }
}

#[test]
fn expiry_is_checked_before_claim_and_completion_even_without_a_background_sweep() {
    use hagency_store::{EffectOutcome, EffectState};
    let (_dir, mut db, root, grant) = setup();
    reserve(&mut db, &root, &grant);
    let p = admitted(&mut db, "expired", "Expired", 200);
    let e = db
        .approve_project_agent("approve", &p, 1000, 200, &decision())
        .unwrap();
    let effect_id = format!("provision_{}", e.id);
    let claimed = db.claim_effect_for_at(&effect_id, 1001).unwrap().unwrap();
    assert!(matches!(
        db.observe_effect_at(
            &effect_id,
            claimed.fence,
            &EffectOutcome::Applied {
                receipt: "late".into()
            },
            5000
        ),
        Err(Error::GrantExpired)
    ));
    db.reconcile_project_grants(5000).unwrap();
    assert_eq!(db.get(&e.id).unwrap().state, EngagementState::Revoked);
    assert_eq!(db.effect(&effect_id).unwrap().state, EffectState::Cancelled);
    let retirement = db
        .claim_effect_for_at(&format!("retire_{}", e.id), 5001)
        .unwrap()
        .unwrap();
    // Expiry refuses provisioning, but must never refuse physical cleanup.
    db.observe_effect_at(
        &retirement.id,
        retirement.fence,
        &EffectOutcome::Applied {
            receipt: "removed".into(),
        },
        5002,
    )
    .unwrap();
    assert_eq!(
        db.resource_ceiling(&root.resource_id, 5002)
            .unwrap()
            .reserved,
        800
    );

    let (_dir, mut db, root, grant) = setup();
    reserve(&mut db, &root, &grant);
    let p = admitted(&mut db, "unclaimed", "Unclaimed", 200);
    let e = db
        .approve_project_agent("approve", &p, 1000, 200, &decision())
        .unwrap();
    assert!(
        db.claim_effect_for_at(&format!("provision_{}", e.id), 5000)
            .unwrap()
            .is_none()
    );
    assert_eq!(db.get(&e.id).unwrap().state, EngagementState::Revoked);
}

#[test]
fn agent_slots_and_daily_rate_are_aggregate_project_limits() {
    for (agents, rate) in [(1, 500), (3, 75)] {
        let (_dir, mut db, root, mut grant) = setup();
        grant.limits.max_agents = agents;
        grant.limits.max_rate_per_day = rate;
        reserve(&mut db, &root, &grant);
        let p = admitted(&mut db, "first", "First", 100);
        db.approve_project_agent("first", &p, 1000, 100, &decision())
            .unwrap();
        let p = admitted(&mut db, "second", "Second", 100);
        assert!(matches!(
            db.approve_project_agent("second", &p, 1000, 100, &decision()),
            Err(Error::InsufficientCapacity)
        ));
    }
}

#[test]
fn different_resource_on_the_same_seat_cannot_reuse_reserved_tokens() {
    // This uses the same declared provider account with two model presets.
    let (_dir, mut db, root, grant) = setup();
    let seat = serde_json::from_value(serde_json::json!({"id":"grant_seat","declaration":{"quotaTokens":1000,"period":"monthly"}})).unwrap();
    db.put_seat(&seat).unwrap();
    reserve(&mut db, &root, &grant);
    let other = resource("other_model", "grant_seat", 1000);
    db.put_resource(&other).unwrap();
    let mut root2 = root.clone();
    root2.id = "other_delegation".into();
    root2.resource_id = other.id();
    root2.limits.tokens = 201;
    assert!(db.delegate_resource(&root2, 1000).is_err());
}

#[test]
fn reassignment_fences_old_decisions_without_changing_owner_or_capacity() {
    let (dir, mut db, root, grant) = setup();
    reserve(&mut db, &root, &grant);
    let p = admitted(&mut db, "reassigned", "Reassigned", 200);
    let admins = vec!["@replacement:example.test".into()];
    let updated = db
        .update_project_administrators(&grant.id, 1, &admins, false, &registration(), 1001)
        .unwrap();
    assert_eq!(updated.owner_mxid, grant.owner_mxid);
    assert_eq!(updated.limits, grant.limits);
    assert_eq!(updated.revision, 2);
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert_eq!(
        db.update_project_administrators(&grant.id, 1, &admins, false, &registration(), 1001)
            .unwrap(),
        updated
    );
    assert!(matches!(
        db.approve_project_agent("old", &p, 1001, 200, &decision()),
        Err(Error::GrantAuthority)
    ));
    let mut scope = decision();
    scope.grant_revision = 2;
    assert!(matches!(
        db.approve_project_agent("still_old_actor", &p, 1001, 200, &scope),
        Err(Error::GrantAuthority)
    ));
    scope.actor_mxid = admins[0].clone();
    let agent = db
        .approve_project_agent("new", &p, 1001, 200, &scope)
        .unwrap();
    let mut other_issuer = registration();
    other_issuer.fleet_id = "different_fleet".into();
    assert!(matches!(
        db.revoke_project_grant(&grant.id, 2, &other_issuer, 1002),
        Err(Error::GrantAuthority)
    ));
    db.revoke_project_grant(&grant.id, 2, &registration(), 1002)
        .unwrap();
    assert_eq!(db.get(&agent.id).unwrap().state, EngagementState::Revoked);
    assert_eq!(
        db.resource_ceiling(&root.resource_id, 1002)
            .unwrap()
            .reserved,
        800
    );
}

#[test]
fn revoked_running_agent_cannot_continue_using_an_existing_runner_capability() {
    use hagency_core::tasks::{DispatchInput, SessionBinding};
    use hagency_store::EffectOutcome;
    let (_dir, mut db, mut root, mut grant) = setup();
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    root.expires_at_ms = now + 60_000;
    grant.expires_at_ms = now + 30_000;
    reserve(&mut db, &root, &grant);
    let p = admitted(&mut db, "running", "Running", 200);
    let agent = db
        .approve_project_agent("approve", &p, 1000, 200, &decision())
        .unwrap();
    let provision = db
        .claim_effect_for_at(&format!("provision_{}", agent.id), now)
        .unwrap()
        .unwrap();
    db.observe_effect_at(
        &provision.id,
        provision.fence,
        &EffectOutcome::Applied {
            receipt: "physical_identity".into(),
        },
        now,
    )
    .unwrap();
    db.register_session(&SessionBinding {
        id: "grant_session".into(),
        engagement_id: agent.id.clone(),
        room_id: grant.room_id.clone(),
        thread_root: Some("$root".into()),
    })
    .unwrap();
    db.enqueue_dispatch(&DispatchInput {
        id: "grant_dispatch".into(),
        session_id: "grant_session".into(),
        task_id: None,
        resources: vec![],
        payload: serde_json::json!({"text":"work"}),
    })
    .unwrap();
    let capability = db
        .claim_dispatch("runner", now, 60_000, 120_000, 1)
        .unwrap()
        .unwrap();
    db.start_dispatch(&capability, now + 1).unwrap();
    db.revoke_project_grant(&grant.id, 1, &registration(), now + 2)
        .unwrap();
    assert!(db.check_runner(&capability, now + 3).is_err());
    assert!(
        db.complete_dispatch(&capability, &serde_json::json!({"text":"late"}), now + 3)
            .is_err()
    );
}

#[tokio::test]
async fn project_reservation_checks_expiry_after_the_real_sqlite_lock() {
    use std::time::Duration;
    let now = || {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap()
    };
    // Model a short lock crossing expiry, within the production 100 ms busy
    // timeout. Rebuild only when scheduler delay did not model that window.
    for attempt in 0..5 {
        let (dir, mut db, mut root, mut grant) = setup();
        root.expires_at_ms = now() + 60_000;
        grant.expires_at_ms = now() + 1000;
        db.delegate_resource(&root, now()).unwrap();
        let store = DomainStore::start(db, 8).unwrap();
        let mut sql = rusqlite::Connection::open(dir.path().join("state/domain.sqlite3")).unwrap();
        tokio::time::sleep(Duration::from_millis(
            grant.expires_at_ms.saturating_sub(now()).saturating_sub(60),
        ))
        .await;
        let lock = sql
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let taken = now();
        if taken + 15 >= grant.expires_at_ms {
            lock.commit().unwrap();
            store.shutdown().await.unwrap();
            assert!(attempt < 4, "could not model contention before expiry");
            continue;
        }
        let operation = store.reserve_project_grant(grant.clone(), registration());
        tokio::pin!(operation);
        assert!(
            tokio::time::timeout(Duration::from_millis(5), &mut operation)
                .await
                .is_err()
        );
        if now() >= grant.expires_at_ms {
            lock.commit().unwrap();
            let _ = operation.await;
            store.shutdown().await.unwrap();
            assert!(attempt < 4, "scheduler missed the pre-expiry lock window");
            continue;
        }
        tokio::time::sleep(Duration::from_millis(
            grant.expires_at_ms.saturating_sub(now()) + 5,
        ))
        .await;
        let held = now().saturating_sub(taken);
        lock.commit().unwrap();
        let result = operation.await;
        store.shutdown().await.unwrap();
        if held >= 90 {
            assert!(
                attempt < 4,
                "scheduler exceeded the SQLite contention window"
            );
            continue;
        }
        assert!(
            matches!(result, Err(Error::GrantExpired)),
            "a grant expired during lock contention must not be reserved: {result:?}"
        );
        assert!(
            !matches!(result, Err(Error::Sqlite(_))),
            "must decide after acquiring the lock, not fail because of contention"
        );
        assert_eq!(
            sql.query_row("SELECT COUNT(*) FROM project_grants", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            0
        );
        return;
    }
}

#[test]
fn held_contribution_cannot_move_to_another_account_or_ignore_a_lower_ceiling() {
    let (_dir, mut db, root, grant) = setup();
    reserve(&mut db, &root, &grant);
    let mut pool = resource("grant_pool", "another_seat", 1000);
    assert!(matches!(db.put_resource(&pool), Err(Error::State)));
    pool = resource("grant_pool", "grant_seat", 700);
    db.put_resource(&pool).unwrap();
    let p = admitted(&mut db, "lower_limit", "LowerLimit", 100);
    assert!(matches!(
        db.approve_project_agent("approve", &p, 1000, 100, &decision()),
        Err(Error::InsufficientCapacity)
    ));
    assert_eq!(
        db.resource_ceiling(&root.resource_id, 1000)
            .unwrap()
            .reserved,
        800
    );
}
