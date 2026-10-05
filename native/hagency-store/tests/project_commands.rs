mod common;
use common::*;
use hagency_core::{authority::*, project_commands::*, project_grants::*};
use hagency_store::{DomainRepository, Error};
use serde_json::json;

fn setup() -> (tempfile::TempDir, DomainRepository, ProjectGrant) {
    let dir = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    let pool = resource("commands", "seat", 1000);
    db.put_resource(&pool).unwrap();
    let limits = GrantLimits {
        tokens: 800,
        max_agents: 4,
        max_rate_per_day: 1000,
    };
    let root = ResourceDelegation {
        v: 1,
        id: "contribution".into(),
        revision: 1,
        fleet_id: registration().fleet_id,
        registration_generation: 1,
        issuer: "example.test".into(),
        resource_id: pool.id(),
        limits: limits.clone(),
        expires_at_ms: 100_000,
    };
    db.delegate_resource(&root, 1000).unwrap();
    let grant = ProjectGrant {
        v: 1,
        id: "project_grant".into(),
        revision: 1,
        delegation_id: root.id,
        delegation_revision: 1,
        project_id: "project_one".into(),
        room_id: "!project:example.test".into(),
        owner_mxid: "@owner:example.test".into(),
        administrator_mxids: vec!["@admin:example.test".into()],
        allow_self_approval: false,
        limits,
        expires_at_ms: 90_000,
    };
    (dir, db, grant)
}
fn command(id: &str, operation: ProjectOperation) -> ProjectCommand {
    ProjectCommand {
        v: 1,
        command_id: id.into(),
        fleet_id: registration().fleet_id,
        registration_generation: 1,
        issuer: "example.test".into(),
        actor_mxid: "@admin:example.test".into(),
        expires_at_ms: 20_000,
        operation,
    }
}
fn authorization(command: &ProjectCommand) -> ProjectAuthorization {
    ProjectAuthorization {
        v: 1,
        command_id: command.command_id.clone(),
        command_digest: command.digest().unwrap(),
        allowed: true,
        valid_until_ms: 2000,
    }
}
fn apply(
    db: &mut DomainRepository,
    command: &ProjectCommand,
    proof: Option<&VerifiedRequest>,
) -> ProjectReceipt {
    db.apply_project_command(
        command,
        &registration(),
        &authorization(command),
        proof,
        1000,
    )
    .unwrap()
}
fn proof_for_agent(_db: &mut DomainRepository) -> VerifiedRequest {
    let mut request = request(
        "agent_request",
        "Littlewhite",
        &resource("commands", "seat", 1000),
        200,
    );
    request.rate_per_day = Some(100.try_into().unwrap());
    proof(&request)
}

#[test]
fn fresh_removal_command_retries_only_definitively_failed_cleanup() {
    use hagency_store::EffectOutcome;
    let (_dir, mut db, grant) = setup();
    let reserve = command(
        "reserve",
        ProjectOperation::ReserveProject {
            grant: grant.clone(),
        },
    );
    apply(&mut db, &reserve, None);
    let proof = proof_for_agent(&mut db);
    let id = proof.request().engagement_id().unwrap();
    let approve = command(
        "approve",
        ProjectOperation::ApproveAgent {
            grant_id: grant.id.clone(),
            grant_revision: 1,
            request: proof.request().clone(),
            allocated_tokens: 200,
        },
    );
    apply(&mut db, &approve, Some(&proof));
    let effect = db
        .claim_effect_for_at(&format!("provision_{id}"), 1001)
        .unwrap()
        .unwrap();
    db.observe_effect_at(
        &effect.id,
        effect.fence,
        &EffectOutcome::Applied {
            receipt: "provisioned".into(),
        },
        1002,
    )
    .unwrap();
    let operation = ProjectOperation::RevokeAgent {
        grant_id: grant.id,
        grant_revision: 1,
        engagement_id: id.clone(),
    };
    let remove = command("remove", operation.clone());
    let original_receipt = apply(&mut db, &remove, None);
    let effect = db
        .claim_effect_for_at(&format!("retire_{id}"), 1003)
        .unwrap()
        .unwrap();
    db.observe_effect_at(&effect.id, effect.fence, &EffectOutcome::Unknown, 1004)
        .unwrap();
    let uncertain = command("do_not_retry_uncertain", operation.clone());
    apply(&mut db, &uncertain, None);
    assert!(db.claim_effect_for_at(&effect.id, 1005).unwrap().is_none());
    db.observe_effect_at(
        &effect.id,
        effect.fence,
        &EffectOutcome::NotApplied {
            receipt: "definitely not performed".into(),
        },
        1006,
    )
    .unwrap();
    // Replaying the original immutable receipt does not reset a physical effect.
    assert_eq!(apply(&mut db, &remove, None), original_receipt);
    assert!(db.claim_effect_for_at(&effect.id, 1007).unwrap().is_none());
    let retry = command("retry_failed_cleanup", operation);
    let receipt = apply(&mut db, &retry, None);
    assert!(
        matches!(&receipt.outcome, ProjectOutcome::Applied { result: ProjectResult::Agent { state, .. } } if state == "revoked")
    );
    let fresh = db.claim_effect_for_at(&effect.id, 1008).unwrap().unwrap();
    assert!(fresh.fence > effect.fence);
    db.observe_effect_at(
        &fresh.id,
        fresh.fence,
        &EffectOutcome::NotApplied {
            receipt: "still unavailable".into(),
        },
        1009,
    )
    .unwrap();
    assert_eq!(apply(&mut db, &retry, None), receipt);
    assert!(db.claim_effect_for_at(&fresh.id, 1010).unwrap().is_none());
}
#[test]
fn reserve_approve_and_top_up_have_durable_business_receipts() {
    let (dir, mut db, grant) = setup();
    let reserve = command(
        "reserve",
        ProjectOperation::ReserveProject {
            grant: grant.clone(),
        },
    );
    assert!(matches!(
        apply(&mut db, &reserve, None).outcome,
        ProjectOutcome::Applied {
            result: ProjectResult::Grant { .. }
        }
    ));
    let proof = proof_for_agent(&mut db);
    let approve = command(
        "approve",
        ProjectOperation::ApproveAgent {
            grant_id: grant.id.clone(),
            grant_revision: 1,
            request: proof.request().clone(),
            allocated_tokens: 200,
        },
    );
    let receipt = apply(&mut db, &approve, Some(&proof));
    assert!(
        matches!(&receipt.outcome, ProjectOutcome::Applied { result: ProjectResult::Agent { state, allocated_tokens: 200, .. } } if state == "reserved")
    );
    let id = proof.request().engagement_id().unwrap();
    let topup = command(
        "topup",
        ProjectOperation::TopUpAgent {
            grant_id: grant.id.clone(),
            grant_revision: 1,
            engagement_id: id.clone(),
            requester_mxid: grant.owner_mxid.clone(),
            add_tokens: 50,
        },
    );
    let topup_receipt = apply(&mut db, &topup, None);
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert!(db.is_project_command_agent(&registration(), &id).unwrap());
    assert_eq!(apply(&mut db, &topup, None), topup_receipt);
    assert_eq!(u64::from(db.get(&id).unwrap().allocation()), 250);
    assert_eq!(
        db.pending_project_receipts(&registration()).unwrap().len(),
        3
    );
    // A command's acknowledged historical result survives later revocation and
    // expiry; it cannot resurrect the agent or reserve/debit capacity again.
    db.revoke_project_grant(&grant.id, 1, &registration(), 1001)
        .unwrap();
    assert_eq!(
        db.apply_project_command(
            &approve,
            &registration(),
            &authorization(&approve),
            None,
            30_000
        )
        .unwrap(),
        receipt
    );
    assert_eq!(
        db.get(&id).unwrap().state,
        hagency_core::project::EngagementState::Revoked
    );
    let pending = db.pending_project_receipts(&registration()).unwrap();
    db.mark_project_receipts_published(&registration(), &pending)
        .unwrap();
    assert!(
        db.pending_project_receipts(&registration())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.project_command_receipt(&topup, &registration()).unwrap(),
        Some(topup_receipt)
    );
}
#[test]
fn business_receipt_and_reservation_roll_back_together() {
    let (dir, mut db, grant) = setup();
    let command = command(
        "reserve",
        ProjectOperation::ReserveProject {
            grant: grant.clone(),
        },
    );
    let sql = rusqlite::Connection::open(dir.path().join("state/domain.sqlite3")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_receipt BEFORE INSERT ON project_command_receipts BEGIN SELECT RAISE(ABORT,'receipt failure'); END;").unwrap();
    assert!(matches!(
        db.apply_project_command(
            &command,
            &registration(),
            &authorization(&command),
            None,
            1000
        ),
        Err(Error::Sqlite(_))
    ));
    assert!(matches!(
        db.project_grant(&grant.id, 1000),
        Err(Error::NotFound)
    ));
    assert!(
        db.project_command_receipt(&command, &registration())
            .unwrap()
            .is_none()
    );
    sql.execute_batch("DROP TRIGGER fail_receipt").unwrap();
    assert!(matches!(
        apply(&mut db, &command, None).outcome,
        ProjectOutcome::Applied { .. }
    ));
}
#[test]
fn refusal_is_durable_but_an_expired_authorization_lease_is_retryable() {
    let (_dir, mut db, grant) = setup();
    let reserve = command("reserve", ProjectOperation::ReserveProject { grant });
    let mut auth = authorization(&reserve);
    auth.valid_until_ms = 1000;
    assert!(matches!(
        db.apply_project_command(&reserve, &registration(), &auth, None, 1000),
        Err(Error::OutcomeUnknown)
    ));
    assert!(
        db.project_command_receipt(&reserve, &registration())
            .unwrap()
            .is_none()
    );
    auth = authorization(&reserve);
    auth.allowed = false;
    let refused = db
        .apply_project_command(&reserve, &registration(), &auth, None, 1000)
        .unwrap();
    assert!(matches!(
        refused.outcome,
        ProjectOutcome::Refused {
            code: ProjectRefusal::Authority
        }
    ));
    assert_eq!(apply(&mut db, &reserve, None), refused);
    let mut expired = reserve.clone();
    expired.command_id = "expired".into();
    expired.expires_at_ms = 999;
    assert!(matches!(
        apply(&mut db, &expired, None).outcome,
        ProjectOutcome::Refused {
            code: ProjectRefusal::Expired
        }
    ));
}
#[test]
fn scoped_refusal_leaves_no_debit_or_effect_and_changed_command_conflicts() {
    let (dir, mut db, grant) = setup();
    apply(
        &mut db,
        &command(
            "reserve",
            ProjectOperation::ReserveProject {
                grant: grant.clone(),
            },
        ),
        None,
    );
    let proof = proof_for_agent(&mut db);
    let mut approve = command(
        "approve",
        ProjectOperation::ApproveAgent {
            grant_id: grant.id,
            grant_revision: 1,
            request: proof.request().clone(),
            allocated_tokens: 200,
        },
    );
    approve.actor_mxid = "@unassigned:example.test".into();
    assert!(matches!(
        apply(&mut db, &approve, Some(&proof)).outcome,
        ProjectOutcome::Refused {
            code: ProjectRefusal::Authority
        }
    ));
    assert!(matches!(
        db.get(&proof.request().engagement_id().unwrap()),
        Err(Error::NotFound)
    ));
    let sql = rusqlite::Connection::open(dir.path().join("state/domain.sqlite3")).unwrap();
    assert_eq!(
        sql.query_row("SELECT COUNT(*) FROM effects", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        sql.query_row("SELECT COUNT(*) FROM project_grant_agents", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    approve.actor_mxid = "@admin:example.test".into();
    assert!(matches!(
        db.apply_project_command(
            &approve,
            &registration(),
            &authorization(&approve),
            Some(&proof),
            1000
        ),
        Err(Error::Conflict)
    ));
}
#[test]
fn another_fleet_cannot_top_up_or_revoke_a_known_grant() {
    let (_dir, mut db, grant) = setup();
    apply(
        &mut db,
        &command(
            "reserve",
            ProjectOperation::ReserveProject {
                grant: grant.clone(),
            },
        ),
        None,
    );
    let proof = proof_for_agent(&mut db);
    apply(
        &mut db,
        &command(
            "approve",
            ProjectOperation::ApproveAgent {
                grant_id: grant.id.clone(),
                grant_revision: 1,
                request: proof.request().clone(),
                allocated_tokens: 200,
            },
        ),
        Some(&proof),
    );
    let mut other = registration();
    other.fleet_id = format!("hf_{}", "b".repeat(32));
    other.representative_mxid = format!("@{}_representative:example.test", other.fleet_id);
    db.register(&other).unwrap();
    for operation in [
        ProjectOperation::TopUpAgent {
            grant_id: grant.id.clone(),
            grant_revision: 1,
            engagement_id: proof.request().engagement_id().unwrap(),
            requester_mxid: grant.owner_mxid,
            add_tokens: 10,
        },
        ProjectOperation::RevokeProject {
            grant_id: grant.id,
            expected_revision: 1,
        },
    ] {
        let mut command = command(
            if matches!(&operation, ProjectOperation::TopUpAgent { .. }) {
                "topup"
            } else {
                "revoke"
            },
            operation,
        );
        command.fleet_id = other.fleet_id.clone();
        assert!(matches!(
            db.apply_project_command(&command, &other, &authorization(&command), None, 1000)
                .unwrap()
                .outcome,
            ProjectOutcome::Refused {
                code: ProjectRefusal::Authority
            }
        ));
    }
    assert_eq!(
        u64::from(
            db.get(&proof.request().engagement_id().unwrap())
                .unwrap()
                .allocation()
        ),
        200
    );
}
#[test]
fn closed_wire_refuses_unknown_operations_fields_and_wrong_execution_lease() {
    let (_dir, mut db, grant) = setup();
    let reserve = command("reserve", ProjectOperation::ReserveProject { grant });
    let mut wire = serde_json::to_value(&reserve).unwrap();
    wire["operation"]["url"] = json!("http://arbitrary.invalid");
    assert!(serde_json::from_value::<ProjectCommand>(wire).is_err());
    let mut auth = authorization(&reserve);
    auth.command_digest = "a".repeat(64);
    assert!(
        db.apply_project_command(&reserve, &registration(), &auth, None, 1000)
            .is_err()
    );
    assert!(
        db.pending_project_receipts(&registration())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn status_pages_cover_the_whole_fleet_and_exclude_old_registrations() {
    let (_dir, mut db, _) = setup();
    let pool = resource("commands", "seat", 1000);
    let mut expected = Vec::new();
    for i in 0..126 {
        let request = request(&format!("page_{i}"), &format!("Agent_{i}"), &pool, 1);
        db.admit(&proof(&request), 1000).unwrap();
        expected.push(request.engagement_id().unwrap());
    }
    expected.sort();
    let mut observed = Vec::new();
    let mut after = String::new();
    loop {
        let page = db.palpo_status_page(&registration(), &after, 25).unwrap();
        assert!(page.len() <= 25);
        let Some(last) = page.last() else {
            break;
        };
        after = last.id.clone();
        observed.extend(page.into_iter().map(|e| e.id));
    }
    assert_eq!(observed, expected);
    let mut next = registration();
    next.generation += 1;
    db.register(&next).unwrap();
    assert!(matches!(
        db.palpo_status_page(&registration(), "", 25),
        Err(Error::Generation)
    ));
    assert!(db.palpo_status_page(&next, "", 25).unwrap().is_empty());
}

#[test]
fn agent_admission_and_effect_also_roll_back_if_the_receipt_cannot_commit() {
    let (dir, mut db, grant) = setup();
    apply(
        &mut db,
        &command(
            "reserve",
            ProjectOperation::ReserveProject {
                grant: grant.clone(),
            },
        ),
        None,
    );
    let proof = proof_for_agent(&mut db);
    let approve = command(
        "approve",
        ProjectOperation::ApproveAgent {
            grant_id: grant.id.clone(),
            grant_revision: 1,
            request: proof.request().clone(),
            allocated_tokens: 200,
        },
    );
    let sql = rusqlite::Connection::open(dir.path().join("state/domain.sqlite3")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_receipt BEFORE INSERT ON project_command_receipts BEGIN SELECT RAISE(ABORT,'receipt failure'); END;").unwrap();
    assert!(matches!(
        db.apply_project_command(
            &approve,
            &registration(),
            &authorization(&approve),
            Some(&proof),
            1000
        ),
        Err(Error::Sqlite(_))
    ));
    assert!(matches!(
        db.get(&proof.request().engagement_id().unwrap()),
        Err(Error::NotFound)
    ));
    assert_eq!(
        sql.query_row("SELECT COUNT(*) FROM effects", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        sql.query_row("SELECT COUNT(*) FROM project_grant_agents", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    sql.execute_batch("DROP TRIGGER fail_receipt").unwrap();
    assert!(matches!(
        apply(&mut db, &approve, Some(&proof)).outcome,
        ProjectOutcome::Applied { .. }
    ));
}
