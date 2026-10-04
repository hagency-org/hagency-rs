mod common;
use common::*;
use hagency_core::{authority::VerifiedRequest, canonical, project::EngagementState};
use hagency_store::{DomainRepository, Error, coordinator::*};
use palpo_hagency_contract as contract;
use serde_json::{Value, json};

fn authority() -> ServerEngagement {
    serde_json::from_value(json!({"id":registration().fleet_id,"server":"example.test","owner":"@provider:example.test","coordinator":"@coordinator:example.test","registrationGeneration":1,"delegationRevision":1,"delegationExpiresAtMs":1000000,"state":"verified","allowSelfApproval":false,"coordinatorApprovalV1":true})).unwrap()
}
fn context(id: &str) -> Value {
    json!({"version":1,"commandId":id,"serverEngagementId":registration().fleet_id,"registrationGeneration":1,"delegationRevision":1,"actor":"@coordinator:example.test","issuedAtMs":1000,"expiresAtMs":100000})
}
fn resource_grant(amount: u64, revision: u64) -> ResourceGrant {
    serde_json::from_value(json!({"id":"grant_one","serverEngagementId":registration().fleet_id,"resourceId":resource("pool","seat",1000).id(),"revision":revision,"allocatedTokens":amount,"eligibleManagers":["@owner:example.test"]})).unwrap()
}
fn setup() -> (tempfile::TempDir, DomainRepository) {
    let dir = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    db.put_resource(&resource("pool", "seat", 1000)).unwrap();
    db.configure_coordinator(&authority()).unwrap();
    db.put_coordinator_resource(&resource_grant(300, 1), 1000)
        .unwrap();
    let definition = json!({"name":"Project","roomId":"!project:example.test","ownerDmRoomId":"!private:example.test"});
    let command: ProjectApproval = serde_json::from_value(json!({"context":context("project_decision"),"request":{"id":"project_request","revision":1,"serverEngagementId":registration().fleet_id,"projectId":"project_one","owner":"@owner:example.test","requester":"@owner:example.test","definitionDigest":canonical::digest(&definition).unwrap(),"resourceAllocations":["grant_one"]}})).unwrap();
    assert_eq!(
        db.approve_coordinator_project(&command, &definition, 1000)
            .unwrap()
            .state,
        contract::ProjectState::Approved
    );
    (dir, db)
}
fn prepared(
    db: &mut DomainRepository,
    id: &str,
    name: &str,
    tokens: u64,
) -> (AgentApproval, VerifiedRequest) {
    let proof = proof(&request(id, name, &resource("pool", "seat", 1000), tokens));
    db.verify_coordinator_project(&proof, 1000).unwrap();
    db.admit(&proof, 1000).unwrap();
    let command = serde_json::from_value(json!({"context":context(&format!("approve_{id}")),"request":{"id":id,"revision":1,"serverEngagementId":registration().fleet_id,"projectId":"project_one","projectRevision":1,"resourceAllocationId":"grant_one","projectOwner":"@owner:example.test","requester":"@owner:example.test","definitionDigest":canonical::digest(&value(proof.request())).unwrap(),"requestedTokens":tokens},"allocatedTokens":tokens})).unwrap();
    (command, proof)
}

#[test]
fn coordinator_approval_reserves_and_provisions_without_a_console_decision() {
    let (dir, mut db) = setup();
    let (command, proof) = prepared(&mut db, "agent_one", "Littlewhite", 200);
    assert!(matches!(
        db.approve("console_bypass", &proof, 1000),
        Err(Error::LocalAuthority)
    ));
    let approved = db
        .approve_coordinated_agent(&command, &proof, 1000)
        .unwrap();
    assert_eq!(approved.state, EngagementState::Reserved);
    assert_eq!(u64::from(approved.allocation()), 200);
    let effect = db.effect(&format!("provision_{}", approved.id)).unwrap();
    assert_eq!(serde_json::to_value(effect.state).unwrap(), "pending");
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert_eq!(
        db.approve_coordinated_agent(&command, &proof, 1001)
            .unwrap()
            .id,
        approved.id
    );
    assert_eq!(db.effect(&effect.id).unwrap().fence, 0);
    let mut changed = command;
    changed.allocated_tokens = 100.try_into().unwrap();
    assert!(
        db.approve_coordinated_agent(&changed, &proof, 1001)
            .is_err()
    );
}

#[test]
fn contribution_and_agent_reservations_cannot_exceed_parent_or_child() {
    let (_dir, mut db) = setup();
    let (one, p1) = prepared(&mut db, "one", "One", 200);
    db.approve_coordinated_agent(&one, &p1, 1000).unwrap();
    let (two, p2) = prepared(&mut db, "two", "Two", 101);
    assert!(matches!(
        db.approve_coordinated_agent(&two, &p2, 1000),
        Err(Error::InsufficientCapacity)
    ));
    // Top up the engagement within its owned resource, then retry the original
    // still-unexecuted decision. No new human agent approval is required.
    db.put_coordinator_resource(&resource_grant(400, 2), 1000)
        .unwrap();
    db.approve_coordinated_agent(&two, &p2, 1000).unwrap();
    assert!(
        db.put_coordinator_resource(&resource_grant(1001, 3), 1000)
            .is_err()
    );
    assert!(
        db.put_coordinator_resource(&resource_grant(300, 3), 1000)
            .is_err()
    );
    // Parent resource reserves 400, not 400 + both child allocations.
    assert_eq!(
        db.resource_headroom(&resource("pool", "seat", 1000).id(), 1000)
            .unwrap()
            .0
            .pool
            .remaining
            .map(u64::from),
        Some(600)
    );
    db.revoke("retire_one", &p1.request().engagement_id().unwrap())
        .unwrap();
    assert!(
        db.put_coordinator_resource(&resource_grant(101, 3), 1000)
            .is_err(),
        "retiring cannot refund unknown consumption"
    );
}

#[test]
fn console_contribution_permission_is_owned_revocable_and_fenced_at_commit() {
    use hagency_store::{ResourceConfigurationAccess, ResourcePublicationRetirement};
    use std::time::{Duration, Instant};
    let (_dir, mut db) = setup();
    let until = Instant::now() + Duration::from_secs(30);
    let retirement = ResourcePublicationRetirement::default();
    let access = ResourceConfigurationAccess::new(until, retirement.clone());
    let command = access
        .prepare_contribution(resource_grant(400, 2), until)
        .unwrap();
    assert!(
        matches!(access.revoke(), Err(Error::Busy)),
        "an admitted command retains custody"
    );
    retirement.retire();
    assert!(matches!(
        db.contribute_resource(command, 1000),
        Err(Error::LocalAuthority)
    ));
    assert_eq!(
        db.server_engagement_resources(&registration().fleet_id, "", 50)
            .unwrap()[0]["allocatedTokens"],
        300
    );
    access.revoke().unwrap();
    assert!(matches!(
        access.prepare_contribution(resource_grant(400, 2), until),
        Err(Error::LocalAuthority)
    ));

    let live = ResourceConfigurationAccess::new(until, Default::default());
    let command = live
        .prepare_contribution(resource_grant(400, 2), until)
        .unwrap();
    db.contribute_resource(command, 1000).unwrap();
    live.revoke().unwrap();
    assert_eq!(
        db.server_engagement_resources(&registration().fleet_id, "", 50)
            .unwrap()[0]["remainingTokens"],
        400
    );
}

#[test]
fn fleet_status_pages_reach_agents_after_the_first_hundred() {
    let (_dir, mut db) = setup();
    for i in 0..101 {
        let proof = proof(&request(
            &format!("request_{i}"),
            &format!("Worker{i}"),
            &resource("pool", "seat", 1000),
            1,
        ));
        db.admit(&proof, 1000).unwrap();
    }
    let mut after = String::new();
    let mut ids = std::collections::BTreeSet::new();
    loop {
        let page = db
            .fleet_engagements(&registration().fleet_id, &after, 20)
            .unwrap();
        let Some(last) = page.last() else {
            break;
        };
        after = last.id.clone();
        for row in page {
            assert!(ids.insert(row.id));
        }
    }
    assert_eq!(ids.len(), 101);
    assert!(
        db.fleet_engagements("other_engagement", "", 100)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn revoked_coordinator_and_wrong_project_binding_are_refused() {
    let (_dir, mut db) = setup();
    let (command, proof) = prepared(&mut db, "one", "One", 200);
    let mut authority = authority();
    authority.coordinator = "@replacement:example.test".to_owned().try_into().unwrap();
    assert!(db.configure_coordinator(&authority).is_err());
    authority.delegation_revision = 2.try_into().unwrap();
    db.configure_coordinator(&authority).unwrap();
    assert!(
        db.approve_coordinated_agent(&command, &proof, 1000)
            .is_err()
    );
    assert!(matches!(
        db.effect(&format!(
            "provision_{}",
            proof.request().engagement_id().unwrap()
        )),
        Err(Error::NotFound)
    ));
    let mut other = request("other", "Other", &resource("pool", "seat", 1000), 100);
    other.target_room_id = "!different:example.test".into();
    assert!(
        db.verify_coordinator_project(&common::proof(&other), 1000)
            .is_err()
    );
}

fn publication_identity() -> hagency_store::outbound::RegistrationIdentity {
    hagency_store::outbound::RegistrationIdentity {
        binding: "fixture".into(),
        registration_generation: 1,
        side_id: "example.test".into(),
        fleet_id: registration().fleet_id,
        registration_fingerprint: hagency_store::publication_fingerprint(&registration()).unwrap(),
    }
}

#[test]
fn project_readiness_requires_fresh_matrix_proof_and_old_publication_ack_keeps_new_state() {
    let (dir, mut db) = setup();
    let identity = publication_identity();
    let initial = db.coordinator_updates(&identity).unwrap();
    assert_eq!(initial.len(), 3);
    let observation = observation(&request(
        "unused",
        "Unused",
        &resource("pool", "seat", 1000),
        100,
    ));
    let mut readiness = ProjectReadiness {
        registration: registration(),
        project_id: "project_one".into(),
        observed_at_ms: 1000,
        project: observation.project,
        owner_room: observation.owner_room,
    };
    assert!(db.coordinator_project_ready(&readiness, 31001).is_err());
    readiness
        .owner_room
        .joined
        .insert("@intruder:example.test".into());
    assert!(db.coordinator_project_ready(&readiness, 1001).is_err());
    readiness.owner_room.joined.remove("@intruder:example.test");
    assert_eq!(
        db.coordinator_project_ready(&readiness, 1001)
            .unwrap()
            .state,
        contract::ProjectState::Ready
    );
    db.put_coordinator_resource(&resource_grant(400, 2), 1001)
        .unwrap();
    // Network acceptance of an older frozen body cannot erase the later ready
    // state or the top-up. Restart retains precisely those new projections.
    db.acknowledge_coordinator_updates(&identity, &initial)
        .unwrap();
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    let current = db.coordinator_updates(&identity).unwrap();
    assert_eq!(current.len(), 2);
    assert!(
        current
            .iter()
            .any(|u| u["payload"]["project"]["state"] == "ready")
    );
    assert!(
        current
            .iter()
            .any(|u| u["payload"]["resource"]["allocatedTokens"] == 400)
    );
    db.acknowledge_coordinator_updates(&identity, &current)
        .unwrap();
    assert!(db.coordinator_updates(&identity).unwrap().is_empty());
}

fn top_up(agent: &str, id: &str, expected: u64, added: u64) -> TokenTopUpApproval {
    let definition = json!({"agentAllocationId":agent,"expectedAllocatedTokens":expected,"requestedAdditionalTokens":added});
    serde_json::from_value(json!({"context":context(id),"request":{
        "id":format!("request_{id}"),"revision":1,"serverEngagementId":registration().fleet_id,"projectId":"project_one","projectRevision":1,
        "resourceAllocationId":"grant_one","agentAllocationId":agent,"projectOwner":"@owner:example.test","requester":"@owner:example.test",
        "definitionDigest":canonical::digest(&definition).unwrap(),"expectedAllocatedTokens":expected,"requestedAdditionalTokens":added},
        "additionalTokens":added})).unwrap()
}

#[test]
fn coordinator_top_up_is_atomic_replayable_and_cannot_grow_the_engagement_pool() {
    let (dir, mut db) = setup();
    let (decision, proof) = prepared(&mut db, "agent_one", "Littlewhite", 200);
    let agent = db
        .approve_coordinated_agent(&decision, &proof, 1000)
        .unwrap();
    let command = top_up(&agent.id, "tokens_one", 200, 80);
    assert!(matches!(
        db.raise_allocation("console_topup", &agent.id, 80, 1000),
        Err(Error::LocalAuthority)
    ));
    assert_eq!(
        u64::from(
            db.approve_coordinator_top_up(&command, &proof, 1001)
                .unwrap()
                .allocation()
        ),
        280
    );
    // A competing request froze the previous amount. It cannot apply twice.
    assert!(
        db.approve_coordinator_top_up(
            &top_up(&agent.id, "tokens_competing", 200, 20),
            &proof,
            1001
        )
        .is_err()
    );
    assert!(matches!(
        db.approve_coordinator_top_up(&top_up(&agent.id, "tokens_over", 280, 21), &proof, 1001),
        Err(Error::InsufficientCapacity)
    ));
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert_eq!(
        u64::from(
            db.approve_coordinator_top_up(&command, &proof, 1002)
                .unwrap()
                .allocation()
        ),
        280
    );
    assert_eq!(u64::from(db.get(&agent.id).unwrap().allocation()), 280);
    assert_eq!(
        db.resource_headroom(&resource("pool", "seat", 1000).id(), 1002)
            .unwrap()
            .0
            .pool
            .remaining
            .map(u64::from),
        Some(700)
    );
    db.put_coordinator_resource(&resource_grant(400, 2), 1002)
        .unwrap();
    assert_eq!(
        u64::from(
            db.approve_coordinator_top_up(
                &top_up(&agent.id, "tokens_later", 280, 80),
                &proof,
                1002
            )
            .unwrap()
            .allocation()
        ),
        360
    );
    assert_eq!(
        db.resource_headroom(&resource("pool", "seat", 1000).id(), 1002)
            .unwrap()
            .0
            .pool
            .remaining
            .map(u64::from),
        Some(600)
    );
    assert!(
        db.coordinator_updates(&publication_identity())
            .unwrap()
            .iter()
            .any(|u| u["payload"]["commandId"] == "tokens_later")
    );
}

#[test]
fn profile_import_is_atomic_and_native_probe_is_required_before_contribution() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    let mut reg = registration();
    reg.reception_room_id.clear();
    let mut policy = authority();
    policy.state = contract::EngagementState::Configuring;
    db.import_coordinator_registration(&reg, Some(&policy))
        .unwrap();
    db.put_resource(&resource("pool", "seat", 1000)).unwrap();
    assert!(
        db.put_coordinator_resource(&resource_grant(100, 1), 1000)
            .is_err()
    );
    db.bind_reception(&reg.fleet_id, reg.generation, "!reception:example.test")
        .unwrap();
    db.put_coordinator_resource(&resource_grant(100, 1), 1000)
        .unwrap();
    // A rejected delegate revision must not rotate the registration underneath
    // the still-valid profile.
    reg.generation = 2;
    policy.registration_generation = 2.try_into().unwrap();
    policy.coordinator = "@replacement:example.test".to_owned().try_into().unwrap();
    assert!(
        db.import_coordinator_registration(&reg, Some(&policy))
            .is_err()
    );
    assert_eq!(
        db.provisioning_registration(&reg.fleet_id)
            .unwrap()
            .generation,
        1
    );
    db.put_coordinator_resource(&resource_grant(200, 2), 1000)
        .unwrap();
}
