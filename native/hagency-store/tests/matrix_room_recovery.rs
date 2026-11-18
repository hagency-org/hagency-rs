mod common;
use common::*;
use hagency_core::{replies::*, tasks::*};
use hagency_store::{DomainRepository, EffectOutcome, Error};
use std::collections::BTreeSet;

/// One provisioned agent with an observed transport and room; returns the
/// repository, its engagement and the observed room.
fn agent(
    direct: bool,
) -> (
    tempfile::TempDir,
    DomainRepository,
    String,
    MatrixRoomObservation,
) {
    let temp = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&temp.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    let pool = resource("pool", "seat", 1000);
    db.put_resource(&pool).unwrap();
    let p = proof(&request("one", "Worker", &pool, 100));
    let e = db.admit(&p, 1000).unwrap();
    db.approve("approve", &p, 1000).unwrap();
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
            engagement_id: e.id.clone(),
            registration_generation: 1,
            generation: 1,
            sender_mxid: "@worker:example.test".into(),
            device_id: "DEVICE_1".into(),
        },
        5000,
    )
    .unwrap();
    let room = MatrixRoomObservation {
        engagement_id: e.id.clone(),
        registration_generation: 1,
        transport_generation: 1,
        room_id: if direct {
            "!direct:example.test"
        } else {
            "!project:example.test"
        }
        .into(),
        generation: 1,
        privacy: if direct {
            RoomPrivacy::Direct {
                human_mxid: "@owner:example.test".into(),
            }
        } else {
            RoomPrivacy::Group {}
        },
        joined: BTreeSet::from(["@worker:example.test".into(), "@owner:example.test".into()]),
        invite_only: true,
        encrypted: true,
    };
    db.observe_matrix_room(&room, 5001).unwrap();
    (temp, db, e.id, room)
}
fn bind(engagement: &str, room: &str, id: &str) -> SessionBinding {
    SessionBinding {
        id: id.into(),
        engagement_id: engagement.into(),
        room_id: room.into(),
        thread_root: None,
    }
}
fn state(db: &DomainRepository, e: &str, room: &str) -> MatrixRoomState {
    db.matrix_room_state(e, room).unwrap().unwrap()
}
fn retire(
    db: &mut DomainRepository,
    e: &str,
    room: &MatrixRoomObservation,
    generation: u64,
    reason: &str,
) {
    db.invalidate_matrix_room(
        &MatrixRoomInvalidation {
            engagement_id: e.into(),
            registration_generation: 1,
            transport_generation: 1,
            room_id: room.room_id.clone(),
            generation,
            reason: reason.into(),
        },
        6000,
    )
    .unwrap();
}

/// A DM whose scope advanced past the agent's view is re-observed under the
/// group arms; the new session id resolves and the old retired id stays a
/// Conflict (the store never rebinds a retired id).
#[test]
fn native_direct_room_advanced_scope_resolves_a_new_session() {
    let (_temp, mut db, e, room) = agent(true);
    db.resolve_verified_matrix_session(&bind(&e, &room.room_id, "dm"), 5002)
        .unwrap();
    let mut next = room.clone();
    next.generation = 2;
    db.observe_matrix_room(&next, 5003).unwrap();
    let current = state(&db, &e, &room.room_id);
    assert!(current.available && current.generation == 2);
    // A view still at generation 1 is stale.
    assert!(matches!(
        db.refresh_matrix_direct_room(
            &room,
            Some(&MatrixRoomState {
                generation: 1,
                available: true
            }),
            5004
        ),
        Err(Error::Generation)
    ));
    assert!(matches!(
        db.resolve_verified_matrix_session(&bind(&e, &room.room_id, "dm"), 5005),
        Err(Error::Conflict)
    ));
    let observed = db
        .refresh_matrix_direct_room(&next, Some(&current), 5006)
        .unwrap();
    assert_eq!(observed.generation, 2);
    let bound = db
        .resolve_verified_matrix_session(&bind(&e, &room.room_id, "dm_1_2"), 5007)
        .unwrap();
    assert_eq!(bound.id, "dm_1_2");
    // The old id is not rebound: asking for it yields the current session.
    assert_eq!(
        db.resolve_verified_matrix_session(&bind(&e, &room.room_id, "dm"), 5008)
            .unwrap()
            .id,
        "dm_1_2"
    );
}

/// A DM retired only because a state read failed recovers at generation+1.
#[test]
fn native_direct_room_observation_failure_recovers() {
    let (_temp, mut db, e, room) = agent(true);
    db.resolve_verified_matrix_session(&bind(&e, &room.room_id, "dm"), 5002)
        .unwrap();
    retire(&mut db, &e, &room, 2, OBSERVATION_FAILED);
    let retired = state(&db, &e, &room.room_id);
    assert!(!retired.available && retired.generation == 2);
    assert!(matches!(
        db.resolve_verified_matrix_session(&bind(&e, &room.room_id, "dm"), 5900),
        Err(Error::Conflict)
    ));
    let mut input = room.clone();
    input.generation = retired.generation;
    let observed = db
        .refresh_matrix_direct_room(&input, Some(&retired), 6001)
        .unwrap();
    assert_eq!(observed.generation, 3);
    let now = state(&db, &e, &room.room_id);
    assert!(now.available && now.generation == 3);
    assert_eq!(
        db.resolve_verified_matrix_session(&bind(&e, &room.room_id, "dm_1_3"), 6002)
            .unwrap()
            .id,
        "dm_1_3"
    );
}

/// Authority and safety retirements are not recovered, for DMs and groups.
#[test]
fn native_direct_and_group_safety_retirements_stay_retired() {
    for reason in [
        "owner left the room",
        "Matrix current recipient proof changed",
    ] {
        for direct in [true, false] {
            let (_temp, mut db, e, room) = agent(direct);
            retire(&mut db, &e, &room, 2, reason);
            let retired = state(&db, &e, &room.room_id);
            let mut input = room.clone();
            input.generation = retired.generation;
            let result = if direct {
                db.refresh_matrix_direct_room(&input, Some(&retired), 6001)
            } else {
                db.refresh_matrix_group_room(&input, Some(&retired), 6001)
            };
            assert!(
                matches!(result, Err(Error::Generation)),
                "{reason} {direct}"
            );
            assert!(!state(&db, &e, &room.room_id).available);
        }
    }
    // An unsafe DM snapshot retires the room and it stays retired.
    let (_temp, mut db, e, room) = agent(true);
    let mut unsafe_room = room.clone();
    unsafe_room.generation = 2;
    unsafe_room.joined.insert("@intruder:example.test".into());
    db.observe_matrix_room(&unsafe_room, 6000).unwrap();
    let retired = state(&db, &e, &room.room_id);
    assert!(!retired.available);
    let mut input = room.clone();
    input.generation = retired.generation;
    assert!(matches!(
        db.refresh_matrix_direct_room(&input, Some(&retired), 6001),
        Err(Error::Generation)
    ));
    assert!(!state(&db, &e, &room.room_id).available);
}

/// A stale expected state and a group observation sent to the DM entry point
/// are refused.
#[test]
fn native_direct_room_refresh_refuses_stale_and_group() {
    let (_temp, mut db, e, room) = agent(true);
    retire(&mut db, &e, &room, 2, OBSERVATION_FAILED);
    let mut input = room.clone();
    input.generation = 2;
    let stale = MatrixRoomState {
        generation: 1,
        available: true,
    };
    assert!(matches!(
        db.refresh_matrix_direct_room(&input, Some(&stale), 6001),
        Err(Error::Generation)
    ));
    assert!(matches!(
        db.refresh_matrix_direct_room(&input, None, 6001),
        Err(Error::Generation)
    ));
    let (_temp, mut db, _e, group) = agent(false);
    assert!(matches!(
        db.refresh_matrix_direct_room(
            &group,
            Some(&MatrixRoomState {
                generation: 1,
                available: true
            }),
            6001
        ),
        Err(Error::RunnerAuthority)
    ));
}
