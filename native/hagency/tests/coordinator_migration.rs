#[path = "../../hagency-store/tests/common/mod.rs"]
mod common;
use common::*;
use hagency_store::{DomainRepository, Repository, coordinator::ServerEngagement, private};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn cli(state: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_hagency"))
        .args(["coordinator-migration", "--state-dir"])
        .arg(state)
        .args(args)
        .output()
        .unwrap()
}
fn backup(source: &Path, target: &Path) {
    // Native stores deliberately retain committed WAL frames at close. Copy
    // through SQLite so the backup includes them without checkpointing source.
    private::write_new(target, b"").unwrap();
    let db =
        rusqlite::Connection::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    db.execute("VACUUM INTO ?1", [target.to_str().unwrap()])
        .unwrap();
}
#[test]
fn offline_cli_adopts_copied_legacy_allocations_and_replays_without_source_writes() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("original");
    let copy = dir.path().join("copy");
    private::create_directory_new(&original).unwrap();
    private::write_new(
        &original.join("operator.token"),
        b"isolated-migration-owner-secret",
    )
    .unwrap();
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let mut db = DomainRepository::open(&original).unwrap();
    db.register(&registration()).unwrap();
    let parent = resource("pool", "seat", 1000);
    db.put_resource(&parent).unwrap();
    let legacy = proof(&request("legacy_agent", "OriginalAgent", &parent, 1000));
    let allocated = db.approve("original_approval", &legacy, 1000);
    assert!(allocated.is_err(), "Approval must follow actual admission");
    db.admit(&legacy, 1000).unwrap();
    let allocated = db.approve("original_approval", &legacy, 1000).unwrap();
    let authority:ServerEngagement=serde_json::from_value(json!({"id":registration().fleet_id,"server":"example.test","owner":"@provider:example.test","coordinator":"@coordinator:example.test","registrationGeneration":1,"delegationRevision":1,"delegationExpiresAtMs":now+3600000,"state":"verified","allowSelfApproval":false,"coordinatorApprovalV1":true})).unwrap();
    db.configure_coordinator(&authority).unwrap();
    let original_inventory = db.coordinator_migration_inventory().unwrap();
    drop(db);
    private::create_directory_new(&copy).unwrap();
    backup(
        &original.join("domain.sqlite3"),
        &copy.join("domain.sqlite3"),
    );
    private::write_new(&copy.join("operator.token"), b"isolated-copy-owner-secret").unwrap();
    let output = cli(&copy, &["inventory"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let inventory: Value = serde_json::from_slice(&output.stdout).unwrap();
    for (left, right) in inventory["tables"]
        .as_array()
        .unwrap()
        .iter()
        .zip(original_inventory["tables"].as_array().unwrap())
    {
        assert_eq!(left, right, "Copied table {} changed", left["table"]);
    }
    assert_eq!(
        inventory["sourceDigest"],
        original_inventory["sourceDigest"]
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&cli(&copy, &["inventory"]).stdout).unwrap(),
        inventory
    );
    let plan = json!({"version":1,"id":"copy_adoption","sourceDigest":inventory["sourceDigest"],"serverEngagementId":authority.id,"registrationGeneration":1,"delegationRevision":1,"resourceOwner":authority.owner,
        "resources":[{"id":"grant_one","serverEngagementId":authority.id,"resourceId":parent.id(),"revision":1,"allocatedTokens":1000,"eligibleManagers":["@owner:example.test"]}],
        "projects":[{"grant":{"projectId":"project_one","serverEngagementId":authority.id,"revision":1,"owner":"@owner:example.test","resourceAllocations":["grant_one"],"state":"ready"},"definition":{"name":"Original project","roomId":"!project:example.test","ownerDmRoomId":"!private:example.test"}}],"agents":{allocated.id.clone():"grant_one"}});
    let file = dir.path().join("adoption.json");
    private::write_new(&file, &serde_json::to_vec(&plan).unwrap()).unwrap();
    // Either active owner lock prevents inventory and adoption, even though
    // SQLite readers could otherwise open the database concurrently.
    let custody = Repository::open(&copy).unwrap();
    assert!(!cli(&copy, &["inventory"]).status.success());
    drop(custody);
    let domain = DomainRepository::open_for_migration(&copy).unwrap();
    assert!(
        !cli(&copy, &["adopt", "--file", file.to_str().unwrap()])
            .status
            .success()
    );
    drop(domain);
    let output = cli(&copy, &["adopt", "--file", file.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["agents"][0]["agentAllocationId"], allocated.id);
    assert_eq!(
        receipt["agents"][0]["originalDecisions"][0]["id"],
        "original_approval"
    );
    assert!(receipt["agents"][0]["consumedTokens"].is_null());
    assert_eq!(receipt["resources"][0]["allocatedTokens"], 1000);
    assert!(receipt["resources"][0]["periodKey"].is_string());
    let again = cli(&copy, &["adopt", "--file", file.to_str().unwrap()]);
    assert!(again.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&again.stdout).unwrap(),
        receipt
    );
    assert_eq!(
        DomainRepository::open_for_migration(&original)
            .unwrap()
            .coordinator_migration_inventory()
            .unwrap(),
        original_inventory
    );
    // Restore the migrated copy with every later receipt, not a stale snapshot.
    let restored = dir.path().join("restored");
    private::create_directory_new(&restored).unwrap();
    backup(
        &copy.join("domain.sqlite3"),
        &restored.join("domain.sqlite3"),
    );
    private::write_new(
        &restored.join("operator.token"),
        b"isolated-restored-owner-secret",
    )
    .unwrap();
    let restored_receipt = cli(&restored, &["adopt", "--file", file.to_str().unwrap()]);
    assert!(restored_receipt.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&restored_receipt.stdout).unwrap(),
        receipt
    );
    if let Some(output) = std::env::var_os("HAGENCY_MIGRATION_FIXTURE_OUTPUT") {
        // Explicit test artifact only: all identities and receipts above are
        // generated in this isolated fixture, never from a user's state.
        fs::write(
            output,
            serde_json::to_vec_pretty(&json!({"authority":authority,"nativeReceipt":receipt}))
                .unwrap(),
        )
        .unwrap();
    }
}
