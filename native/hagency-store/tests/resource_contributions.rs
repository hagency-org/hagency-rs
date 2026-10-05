mod common;
use common::*;
use hagency_core::project_grants::{GrantLimits, ResourceDelegation};
use hagency_store::{
    ContributionMutation, DomainRepository, Error, ResourceContributionAccess,
    ResourcePublicationRetirement, resource_publication_revision,
};
use std::time::{Duration, Instant};

#[test]
fn contribution_publication_is_fleet_scoped_and_excludes_rotated_authority() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    let mut registration = registration();
    db.register(&registration).unwrap();
    let mut foreign = registration.clone();
    foreign.fleet_id = format!("hf_{}", "b".repeat(32));
    foreign.representative_mxid = format!("@{}_representative:example.test", foreign.fleet_id);
    db.register(&foreign).unwrap();
    let resource = resource("contribute", "seat", 1000);
    db.put_resource(&resource).unwrap();
    for (id, target) in [("mine", &registration), ("theirs", &foreign)] {
        db.delegate_resource(
            &ResourceDelegation {
                v: 1,
                id: id.into(),
                revision: 1,
                fleet_id: target.fleet_id.clone(),
                registration_generation: target.generation,
                issuer: target.server_name.clone(),
                resource_id: resource.id(),
                limits: GrantLimits {
                    tokens: 400,
                    max_agents: 4,
                    max_rate_per_day: 100,
                },
                expires_at_ms: 5000,
            },
            1000,
        )
        .unwrap();
    }
    let identity =
        |r: &hagency_core::authority::Registration| hagency_store::outbound::RegistrationIdentity {
            binding: "original_contribution".into(),
            side_id: r.server_name.clone(),
            fleet_id: r.fleet_id.clone(),
            registration_generation: r.generation,
            registration_fingerprint: hagency_store::publication_fingerprint(r).unwrap(),
        };
    let original = identity(&registration);
    let page = db.contribution_page(&original, "", 2000).unwrap();
    assert_eq!(page.contributions.len(), 1);
    assert_eq!(page.contributions[0].grant.id, "mine");
    assert_eq!(
        page.contributions[0].state,
        hagency_store::ContributionState::Active
    );
    assert_eq!(
        db.contribution_page(&original, "", 5000)
            .unwrap()
            .contributions[0]
            .state,
        hagency_store::ContributionState::Expired
    );
    db.revoke_resource_delegation("mine", 1, 5001).unwrap();
    assert_eq!(
        db.contribution_page(&original, "", 5002)
            .unwrap()
            .contributions[0]
            .state,
        hagency_store::ContributionState::Revoked
    );
    registration.generation += 1;
    db.register(&registration).unwrap();
    assert!(matches!(
        db.contribution_page(&original, "", 6000),
        Err(Error::Generation)
    ));
    assert!(
        db.contribution_page(&identity(&registration), "", 6000)
            .unwrap()
            .contributions
            .is_empty()
    );
    assert_eq!(
        db.resource_contributions(&resource.id(), "", 16, 6000)
            .unwrap()
            .len(),
        2,
        "rotation preserves historical reservations"
    );
}

#[test]
fn contribution_is_owned_by_its_original_console_session_and_incarnation() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    let registration = registration();
    db.register(&registration).unwrap();
    let resource = resource("contribute", "seat", 1000);
    db.put_resource(&resource).unwrap();
    let retirement = ResourcePublicationRetirement::default();
    let access = ResourceContributionAccess::new(
        Instant::now() + Duration::from_secs(60),
        retirement.clone(),
    );
    let grant = ResourceDelegation {
        v: 1,
        id: "contribution".into(),
        revision: 1,
        fleet_id: registration.fleet_id,
        registration_generation: 1,
        issuer: registration.server_name,
        resource_id: resource.id(),
        limits: GrantLimits {
            tokens: 800,
            max_agents: 4,
            max_rate_per_day: 1000,
        },
        expires_at_ms: 100000,
    };
    let command = access
        .prepare(
            resource.id(),
            resource_publication_revision(&resource).unwrap(),
            ContributionMutation::Contribute { grant },
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
    assert!(matches!(access.revoke(), Err(Error::Busy)));
    retirement.retire();
    assert!(matches!(
        db.contribute_resource(command, 1000),
        Err(Error::LocalAuthority)
    ));
    assert!(
        db.resource_contributions(&resource.id(), "", 16, 1000)
            .unwrap()
            .is_empty()
    );
    access.revoke().unwrap();
}
#[test]
fn contribution_writer_refuses_a_registration_rotated_after_the_form_was_reviewed() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    let mut registration = registration();
    db.register(&registration).unwrap();
    let resource = resource("contribute", "seat", 1000);
    db.put_resource(&resource).unwrap();
    let access = ResourceContributionAccess::new(
        Instant::now() + Duration::from_secs(60),
        ResourcePublicationRetirement::default(),
    );
    let grant = ResourceDelegation {
        v: 1,
        id: "contribution".into(),
        revision: 1,
        fleet_id: registration.fleet_id.clone(),
        registration_generation: 1,
        issuer: registration.server_name.clone(),
        resource_id: resource.id(),
        limits: GrantLimits {
            tokens: 800,
            max_agents: 4,
            max_rate_per_day: 1000,
        },
        expires_at_ms: 100000,
    };
    let command = access
        .prepare(
            resource.id(),
            resource_publication_revision(&resource).unwrap(),
            ContributionMutation::Contribute { grant },
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
    registration.generation += 1;
    db.register(&registration).unwrap();
    assert!(matches!(
        db.contribute_resource(command, 1000),
        Err(Error::Invalid(_))
    ));
    assert!(
        db.resource_contributions(&resource.id(), "", 16, 1000)
            .unwrap()
            .is_empty()
    );
}
