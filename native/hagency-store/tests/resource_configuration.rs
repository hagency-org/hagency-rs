#[path = "common/mod.rs"]
mod common;
use hagency_core::project::{Resource, Seat};
use hagency_store::*;
use serde_json::json;
use std::time::{Duration, Instant};
fn resource() -> Resource {
    serde_json::from_value(json!({"presetId":"private_source_preset","seatId":"private_shared_account","framework":"codex","model":"gpt-5.6-sol","reasoning":"medium","ceiling":{"tokens":5000,"period":"monthly"},"published":false})).unwrap()
}
fn access() -> ResourceConfigurationAccess {
    ResourceConfigurationAccess::new(
        Instant::now() + Duration::from_secs(60),
        ResourcePublicationRetirement::default(),
    )
}
fn command(
    access: &ResourceConfigurationAccess,
    resource: &Resource,
    create: bool,
    ceiling: CeilingChange,
) -> ResourceConfigurationCommand {
    access
        .prepare(
            resource.id(),
            resource_publication_revision(resource).unwrap(),
            create,
            ProfileChange::Preserve {},
            ceiling,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap()
}
#[tokio::test]
async fn native_resource_configuration_create_edit() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let mut db = DomainRepository::open(&state).unwrap();
    let source = resource();
    db.put_resource(&source).unwrap();
    let seat: Seat = serde_json::from_value(
        json!({"id":source.seat_id,"declaration":{"quotaTokens":9000,"period":"monthly"}}),
    )
    .unwrap();
    db.put_seat(&seat).unwrap();
    let a = access();
    let created = db
        .configure_resource(command(&a, &source, true, CeilingChange::Preserve {}))
        .unwrap();
    let new = db.resource_configuration(&created.resource_id).unwrap();
    assert!(new.published);
    assert_ne!(new.preset_id, source.preset_id);
    assert_eq!(new.seat_id, source.seat_id);
    assert_eq!(new.provider, source.provider);
    assert_eq!(new.framework, source.framework);
    assert_eq!(
        serde_json::to_value(&new.ceiling).unwrap(),
        serde_json::to_value(&source.ceiling).unwrap()
    );
    assert_eq!(
        u64::from(db.resource_budget(&new.id()).unwrap().seat.quota.unwrap()),
        9000
    );
    assert_eq!(db.seats("", 100).unwrap().len(), 1);
    assert!(db.engagements("", 100).unwrap().is_empty());
    assert!(db.claim_effect().unwrap().is_none());
    db.register(&common::registration()).unwrap();
    // A selectable profile can create a new pool and edit while no allocation holds it.
    let other = hagency_core::qualification::configuration_choices(&source.profile())
        .unwrap()
        .into_iter()
        .find(|c| {
            c.tier >= hagency_core::qualification::Tier::Medium
                && (c.model != source.model || c.reasoning != source.reasoning)
        })
        .unwrap();
    let select = || ProfileChange::Select {
        model: other.model.clone(),
        reasoning: other.reasoning.clone(),
    };
    let changed = a
        .prepare(
            source.id(),
            resource_publication_revision(&source).unwrap(),
            true,
            select(),
            CeilingChange::Preserve {},
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    let variant = db.configure_resource(changed).unwrap();
    let variant = db.resource_configuration(&variant.resource_id).unwrap();
    assert_eq!(variant.seat_id, source.seat_id);
    assert_eq!(variant.model, other.model);
    assert_eq!(variant.reasoning, other.reasoning);
    let pending = common::proof(&common::request(
        "configuration_pending",
        "PendingConfiguration",
        &variant,
        100,
    ));
    db.admit(&pending, 1000).unwrap();
    let edit = a
        .prepare(
            variant.id(),
            resource_publication_revision(&variant).unwrap(),
            false,
            ProfileChange::Select {
                model: source.model.clone(),
                reasoning: source.reasoning.clone(),
            },
            CeilingChange::Monthly {
                tokens: 0u64.try_into().unwrap(),
            },
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    db.configure_resource(edit).unwrap();
    assert!(matches!(
        db.approve("configuration_pending_capacity", &pending, 1000),
        // The ceiling is declared (as zero), so the refusal names the draw
        // rather than reporting unknown capacity.
        Err(Error::OverCommit { .. })
    ));
    let proof = common::proof(&common::request(
        "configuration_commitment",
        "ConfiguredWorker",
        &new,
        100,
    ));
    db.admit(&proof, 1000).unwrap();
    db.approve("configuration_approval", &proof, 1000).unwrap();
    let alternative = hagency_core::qualification::configuration_choices(&new.profile())
        .unwrap()
        .into_iter()
        .find(|c| c.model != new.model || c.reasoning != new.reasoning)
        .unwrap();
    let profile_change = || ProfileChange::Select {
        model: alternative.model.clone(),
        reasoning: alternative.reasoning.clone(),
    };
    let edit = a
        .prepare(
            new.id(),
            resource_publication_revision(&new).unwrap(),
            false,
            profile_change(),
            CeilingChange::Preserve {},
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    assert!(matches!(db.configure_resource(edit), Err(Error::State)));
    let effect = db.claim_effect().unwrap().unwrap();
    db.observe_effect(
        &effect.id,
        effect.fence,
        &EffectOutcome::Applied {
            receipt: "original local fixture".into(),
        },
    )
    .unwrap();
    let edit = a
        .prepare(
            new.id(),
            resource_publication_revision(&new).unwrap(),
            false,
            profile_change(),
            CeilingChange::Preserve {},
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    assert!(matches!(db.configure_resource(edit), Err(Error::State)));
    db.configure_resource(command(
        &a,
        &new,
        false,
        CeilingChange::Monthly {
            tokens: 50u64.try_into().unwrap(),
        },
    ))
    .unwrap();
    let budget = db.resource_budget(&new.id()).unwrap();
    assert_eq!(u64::from(budget.pool.committed), 100);
    assert_eq!(u64::from(budget.remaining_tokens.unwrap()), 0);
    assert_eq!(
        u64::from(db.resource_budget(&source.id()).unwrap().seat.committed),
        100
    );
    let changed = db
        .configure_resource(command(
            &a,
            &source,
            false,
            CeilingChange::Monthly {
                tokens: 0u64.try_into().unwrap(),
            },
        ))
        .unwrap();
    assert!(!changed.published);
    assert_eq!(changed.resource_id, source.id());
    assert!(matches!(
        db.configure_resource(command(&a, &source, false, CeilingChange::Clear {})),
        Err(Error::Conflict)
    ));
    for ceiling in [
        json!(null),
        json!({"tokens":null}),
        json!({"tokens":null,"period":null}),
        json!({"tokens":42,"period":"weekly"}),
    ] {
        let mut source = source.clone();
        source.ceiling = serde_json::from_value(ceiling.clone()).unwrap();
        db.put_resource(&source).unwrap();
        db.configure_resource(command(&a, &source, false, CeilingChange::Preserve {}))
            .unwrap();
        assert_eq!(
            serde_json::to_value(db.resource_configuration(&source.id()).unwrap().ceiling).unwrap(),
            ceiling
        );
    }
    let mut source = source.clone();
    source.model = "unrecognized-model".into();
    db.put_resource(&source).unwrap();
    db.configure_resource(command(&a, &source, false, CeilingChange::Clear {}))
        .unwrap();
    let source = db.resource_configuration(&source.id()).unwrap();
    assert!(matches!(
        db.configure_resource(command(&a, &source, true, CeilingChange::Preserve {})),
        Err(Error::Invalid(_))
    ));
    assert!(
        serde_json::from_value::<CeilingChange>(
            json!({"kind":"monthly","tokens":9007199254740992u64})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<ProfileChange>(json!({"kind":"preserve","seatId":"browser"}))
            .is_err()
    );
    drop(db);
    let db = DomainRepository::open(&state).unwrap();
    assert_eq!(
        db.resource_configuration(&new.id()).unwrap().seat_id,
        source.seat_id
    );
    drop(db);
    // A live original writer carries the same concrete command through queue custody.
    let store = DomainStore::start(DomainRepository::open(&state).unwrap(), 8).unwrap();
    let current = store.resource_configuration(new.id()).await.unwrap();
    store
        .configure_resource(command(
            &a,
            &current,
            false,
            CeilingChange::Monthly {
                tokens: 7u64.try_into().unwrap(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        u64::from(
            store
                .resource_budget(current.id())
                .await
                .unwrap()
                .pool
                .ceiling
                .unwrap()
        ),
        7
    );
    store.shutdown().await.unwrap();
    let mut db = DomainRepository::open(&state).unwrap();
    let mut inspect = rusqlite::Connection::open(state.join("domain.sqlite3")).unwrap();
    let count: usize = inspect
        .query_row("SELECT COUNT(*) FROM resources", [], |r| r.get(0))
        .unwrap();
    let tx = inspect.transaction().unwrap();
    for index in count..2048 {
        let mut row = new.clone();
        row.preset_id = format!("capacity_{index}");
        tx.execute(
            "INSERT INTO resources(id,preset_id,config) VALUES(?1,?2,?3)",
            rusqlite::params![
                row.id(),
                row.preset_id,
                serde_json::to_string(&row).unwrap()
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let current = db.resource_configuration(&new.id()).unwrap();
    assert!(matches!(
        db.configure_resource(command(&a, &current, true, CeilingChange::Preserve {})),
        Err(Error::Capacity)
    ));
    assert_eq!(
        inspect
            .query_row("SELECT COUNT(*) FROM resources", [], |r| r
                .get::<_, usize>(0))
            .unwrap(),
        2048
    );
}

#[test]
fn engagement_resources_are_atomic_independent_budgets_on_one_shared_account() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let mut db = DomainRepository::open(&state).unwrap();
    let source = resource();
    db.put_resource(&source).unwrap();
    db.put_seat(
        &serde_json::from_value(
            json!({"id":source.seat_id,"declaration":{"quotaTokens":9000,"period":"monthly"}}),
        )
        .unwrap(),
    )
    .unwrap();
    let fleet = common::registration().fleet_id;
    db.register(&common::registration()).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    db.configure_coordinator(&serde_json::from_value(json!({"id":fleet,"server":"example.test","owner":"@provider:example.test","coordinator":"@coordinator:example.test","registrationGeneration":1,"delegationRevision":1,"delegationExpiresAtMs":now+3600000,"state":"verified","allowSelfApproval":false,"coordinatorApprovalV1":true})).unwrap()).unwrap();
    let access = access();
    let create = |tokens: u64, manager: &str| {
        command(&access, &source, true, CeilingChange::Monthly { tokens: tokens.try_into().unwrap() })
        .with_engagement(serde_json::from_value(json!({"serverEngagementId":fleet,"allocationId":null,"expectedRevision":null,"eligibleManagers":[manager]})).unwrap()).unwrap()
    };
    // The source account profile is not a parent pool: a new 6000 budget is
    // valid despite its 5000 template ceiling, bounded by the shared 9000.
    let first = db
        .configure_resource(create(6000u64, "@owner:example.test"))
        .unwrap();
    let second = db
        .configure_resource(create(3000u64, "@owner:example.test"))
        .unwrap();
    assert_ne!(first.resource_id, second.resource_id);
    let rows = db.server_engagement_resources(&fleet, "", 50).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        db.resource_engagements(&first.resource_id).unwrap().len(),
        1
    );
    assert_eq!(
        u64::from(
            db.resource_budget(&first.resource_id)
                .unwrap()
                .seat
                .committed
        ),
        9000
    );
    let inspect = rusqlite::Connection::open(state.join("domain.sqlite3")).unwrap();
    let snapshot = || -> (i64, i64, i64) {
        (
            inspect
                .query_row("SELECT COUNT(*) FROM resources", [], |r| r.get(0))
                .unwrap(),
            inspect
                .query_row("SELECT COUNT(*) FROM coordinator_resources", [], |r| {
                    r.get(0)
                })
                .unwrap(),
            inspect
                .query_row("SELECT COUNT(*) FROM coordinator_publications", [], |r| {
                    r.get(0)
                })
                .unwrap(),
        )
    };
    let before = snapshot();
    assert!(matches!(
        db.configure_resource(create(1u64, "@owner:example.test")),
        Err(Error::InsufficientCapacity) | Err(Error::OverCommit { .. })
    ));
    assert_eq!(
        snapshot(),
        before,
        "no orphan configuration or publication on quota failure"
    );
    assert!(matches!(
        db.configure_resource(create(1u64, "@owner:other.test")),
        Err(Error::LocalAuthority)
    ));
    assert_eq!(
        snapshot(),
        before,
        "cross-server eligibility rolls back the entire save"
    );
    let original = db.resource_configuration(&first.resource_id).unwrap();
    let binding = db
        .resource_engagements(&first.resource_id)
        .unwrap()
        .remove(0);
    let edit = |r: &Resource, tokens: u64, revision| {
        command(&access, r, false, CeilingChange::Monthly { tokens: tokens.try_into().unwrap() })
        .with_engagement(serde_json::from_value(json!({"serverEngagementId":fleet,"allocationId":binding.id,"expectedRevision":revision,"eligibleManagers":["@owner:example.test"]})).unwrap()).unwrap()
    };
    db.configure_resource(edit(&original, 5500, 1)).unwrap();
    let changed = db.resource_configuration(&first.resource_id).unwrap();
    assert_eq!(
        u64::from(changed.ceiling.as_ref().unwrap().tokens.unwrap()),
        5500
    );
    let current = db
        .resource_engagements(&first.resource_id)
        .unwrap()
        .remove(0);
    assert_eq!(
        current.id, binding.id,
        "project and agent references remain valid"
    );
    assert_eq!(u64::from(current.revision), 2);
    assert_eq!(u64::from(current.allocated_tokens), 5500);
    assert_eq!(
        u64::from(
            db.resource_budget(&first.resource_id)
                .unwrap()
                .seat
                .committed
        ),
        8500
    );
    assert!(matches!(
        db.configure_resource(edit(&changed, 5000, 1)),
        Err(Error::Conflict)
    ));
    assert_eq!(
        u64::from(
            db.resource_configuration(&first.resource_id)
                .unwrap()
                .ceiling
                .unwrap()
                .tokens
                .unwrap()
        ),
        5500
    );
    drop(db);
    let mut db = DomainRepository::open(&state).unwrap();
    assert_eq!(
        u64::from(db.resource_engagements(&first.resource_id).unwrap()[0].allocated_tokens),
        5500
    );
    assert_eq!(
        u64::from(
            db.resource_budget(&first.resource_id)
                .unwrap()
                .seat
                .committed
        ),
        8500
    );
    let context = |id| json!({"version":1,"commandId":id,"serverEngagementId":fleet,"registrationGeneration":1,"delegationRevision":1,"actor":"@coordinator:example.test","issuedAtMs":now,"expiresAtMs":now+300000});
    let definition = json!({"name":"Existing project","roomId":"!project:example.test","ownerDmRoomId":"!private:example.test"});
    let project = serde_json::from_value(json!({"context":context("project_config"),"request":{"id":"project_request_config","revision":1,"serverEngagementId":fleet,"projectId":"project_one","owner":"@owner:example.test","requester":"@owner:example.test","definitionDigest":hagency_core::canonical::digest(&definition).unwrap(),"resourceAllocations":[binding.id]}})).unwrap();
    db.approve_coordinator_project(&project, &definition, now)
        .unwrap();
    let request = common::request("agent_config", "ExistingAgent", &changed, 500);
    let mut observation = common::observation(&request);
    observation.observed_at_ms = now;
    let proof = hagency_core::authority::verify_request(
        &common::registration(),
        request.clone(),
        observation,
    )
    .unwrap();
    db.verify_coordinator_project(&proof, now).unwrap();
    db.admit(&proof, now).unwrap();
    let approve = serde_json::from_value(json!({"context":context("approve_config"),"request":{"id":"agent_config","revision":1,"serverEngagementId":fleet,"projectId":"project_one","projectRevision":1,"resourceAllocationId":binding.id,"projectOwner":"@owner:example.test","requester":"@owner:example.test","definitionDigest":hagency_core::canonical::digest(&serde_json::to_value(&request).unwrap()).unwrap(),"requestedTokens":500},"allocatedTokens":500})).unwrap();
    let agent = db.approve_coordinated_agent(&approve, &proof, now).unwrap();
    assert_eq!(
        u64::from(
            db.resource_budget(&first.resource_id)
                .unwrap()
                .seat
                .committed
        ),
        8500,
        "agent allocations do not charge the account twice"
    );
    assert!(matches!(
        db.configure_resource(edit(&changed, 499, 2)),
        Err(Error::InsufficientCapacity)
    ));
    assert_eq!(
        u64::from(
            db.resource_configuration(&first.resource_id)
                .unwrap()
                .ceiling
                .unwrap()
                .tokens
                .unwrap()
        ),
        5500,
        "a rejected shrink restores the configuration too"
    );
    assert_eq!(
        db.get(&agent.id).unwrap().allocated_tokens,
        agent.allocated_tokens
    );
}
