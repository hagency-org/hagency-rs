//! ADR-199: only a live engagement holds an agent name. Retiring one frees the
//! name for a new request that gets its own engagement id (and so its own
//! Matrix identity); a live holder is refused with a reason that names it.
mod common;
use common::*;
use hagency_store::{DomainRepository, Error};

fn setup() -> (
    tempfile::TempDir,
    DomainRepository,
    hagency_core::project::Resource,
) {
    let dir = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    let pool = resource("default_pool", "default_seat", 1000);
    db.put_resource(&pool).unwrap();
    (dir, db, pool)
}

#[test]
fn revoked_name_can_be_requested_again_with_new_identity() {
    let (_dir, mut db, pool) = setup();
    let first = db
        .admit(
            &proof(&request("request_one", "qore-orchestrator", &pool, 100)),
            1000,
        )
        .unwrap();
    db.revoke("retire_first", &first.id).unwrap();
    let second = db
        .admit(
            &proof(&request("request_two", "qore-orchestrator", &pool, 100)),
            1001,
        )
        .unwrap();
    assert_ne!(first.id, second.id);
}

#[test]
fn name_refusal_names_the_live_holder_and_the_fix() {
    let (_dir, mut db, pool) = setup();
    let first = db
        .admit(
            &proof(&request("request_one", "qore-orchestrator", &pool, 100)),
            1000,
        )
        .unwrap();
    let error = db
        .admit(
            &proof(&request("request_two", "qore-orchestrator", &pool, 100)),
            1001,
        )
        .unwrap_err();
    assert!(matches!(error, Error::NameInUse { .. }), "{error:?}");
    let text = error.to_string();
    assert!(
        text.contains(&first.id) && text.contains("retire"),
        "{text}"
    );
    assert_eq!(
        hagency_store::coordinator::coordinator_refusal_reason(&error),
        Some("agent_name_in_use")
    );
}
