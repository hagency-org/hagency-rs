//! O2 (audit): the console's real-agent proof. The roster shows an agent whose
//! engagement was minted by the production Matrix ingress and made effective
//! and routable by the production verdict path — never a `seed()`ed row. The
//! fake peer delivers the provisioning request into the reception room, the
//! intake admits it, the representative's verdict makes it effective and binds
//! the session route, and only then does the real GET /console/api/agents
//! roster show it, keyed by its minted engagement id.
use super::*;

#[path = "../../../hagency-matrix/tests/common/mod.rs"]
pub mod matrix_common;

use hagency_core::replies::RoomPrivacy;
use hagency_matrix::{CancellationToken, Collector, HostConfig, HostIntakePlan, HostRoom};
use serde_json::json;
use sha2::Digest;
use std::net::SocketAddr;

const SESSION_ROOM: &str = "!project:example.test";
const PROJECT: &str = "!project_provision:example.test";
const RECEPTION: &str = "!reception:example.test";
const PRIVATE: &str = "!private:example.test";
const OWNER: &str = "@owner:example.test";
const APPROVAL_BOT: &str = "@approval:example.test";

fn fleet_id() -> String {
    format!("hf_{}", "a".repeat(32))
}
fn representative() -> String {
    format!("@{}_representative:example.test", fleet_id())
}
fn agent_mxid() -> String {
    format!(
        "@en_{}:example.test",
        &format!(
            "{:x}",
            sha2::Sha256::digest(
                serde_json::to_vec(&[&fleet_id(), &"request_one".to_string()]).unwrap()
            )
        )[..32]
    )
}

fn member(mxid: &str) -> Value {
    json!({"type": "m.room.member", "state_key": mxid, "content": {"membership": "join"}})
}
fn power_levels(users: Value) -> Value {
    json!({
        "type": "m.room.power_levels",
        "state_key": "",
        "content": {"users": users, "users_default": 0, "invite": 0}
    })
}
fn session_state() -> Value {
    json!([
        member("@worker:example.test"),
        member(OWNER),
        {"type": "m.room.join_rules", "state_key": "", "content": {"join_rule": "invite"}},
        power_levels(json!({}))
    ])
}
fn reception_state() -> Value {
    json!([
        member("@worker:example.test"),
        member(OWNER),
        member(&representative()),
        {"type": "m.room.join_rules", "state_key": "", "content": {"join_rule": "invite"}},
        power_levels(json!({}))
    ])
}
fn project_state() -> Value {
    json!([
        member("@worker:example.test"),
        member(OWNER),
        member(&representative()),
        member(&agent_mxid()),
        {"type": "m.room.join_rules", "state_key": "", "content": {"join_rule": "invite"}},
        power_levels(json!({OWNER: 100, representative(): 50})),
        {"type": "m.room.name", "state_key": "", "content": {"name": "实际项目名称"}},
        {
            "type": "com.hagency.project.binding.v1",
            "state_key": "",
            "content": {
                "v": 1,
                "fleetId": fleet_id(),
                "purpose": "project",
                "projectId": "project_provision",
                "ownerMxid": OWNER,
                "authVersion": 1
            }
        }
    ])
}

fn request_body(request_id: &str, tokens: u64) -> String {
    serde_json::json!({
        "requestId": request_id,
        "requester": OWNER,
        "project": "project_provision",
        "projectRoomId": PROJECT,
        "role": "coding",
        "requestedTokens": tokens,
        "ratePerDay": null,
        "agent": "Provisioned",
        "context": {"agentDefinition": {"resourceId": "resource_27cac5503836765cd10751d2"}}
    })
    .to_string()
}
fn request_event(event_id: &str, body: String) -> Value {
    json!({
        "event_id": event_id,
        "sender": OWNER,
        "type": "m.room.message",
        "origin_server_ts": now(),
        "content": {"msgtype": "com.hagency.engagement.request.v1", "body": body}
    })
}
fn approval_event(event_id: &str, request_id: &str) -> Value {
    json!({
        "event_id": event_id,
        "sender": representative(),
        "type": "m.room.message",
        "origin_server_ts": now(),
        "content": {
            "msgtype": "com.hagency.engagement.approval.v1",
            "body": json!({"requestId": request_id, "decision": "approve"}).to_string()
        }
    })
}
fn provisioning_sync(token: &str, events: Vec<Value>) -> Value {
    json!({
        "next_batch": token,
        "rooms": {
            "join": {
                SESSION_ROOM: {"timeline": {"events": [], "limited": false}, "state": {"events": []}},
                RECEPTION: {"timeline": {"events": events, "limited": false}, "state": {"events": []}}
            }
        },
        "to_device": {"events": []}
    })
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// The collector config: the session-route room publishes; the reception room
/// is observed but never routed.
fn provisioning_config(f: &matrix_common::Fixture, endpoint: &str) -> HostConfig {
    let mut config = HostConfig::new(
        f.identity.clone(),
        endpoint,
        matrix_common::TOKEN,
        f.root.path().join("sdk"),
        [42; 32],
        vec![HostRoom {
            room_id: SESSION_ROOM.into(),
            generation: 1,
            privacy: RoomPrivacy::Group {},
        }],
        matrix_common::limits(),
    )
    .unwrap()
    .with_root_pem(include_bytes!("../../../hagency-matrix/tests/fixtures/ca.pem"))
    .unwrap();
    config
        .with_reception_room(HostRoom {
            room_id: RECEPTION.into(),
            generation: 1,
            privacy: RoomPrivacy::Group {},
        })
        .unwrap();
    config
}

/// Seed the enrolled owner-DM room the provisioning ingress reads fail-closed
/// before admit.
fn enroll_owner_room(f: &matrix_common::Fixture) {
    rusqlite::Connection::open(f.root.path().join("domain/domain.sqlite3"))
        .unwrap()
        .execute(
            "INSERT INTO approval_rooms(server_name,room_id,generation,fleet_id,project_id,registration_generation,owner_mxid,bot_mxid,device_id,available,digest,config) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10,?11)",
            rusqlite::params![
                "example.test",
                PRIVATE,
                1,
                fleet_id(),
                "project_one",
                1,
                OWNER,
                APPROVAL_BOT,
                "APPROVAL_DEVICE",
                "e".repeat(64),
                serde_json::json!({"joined":[OWNER,APPROVAL_BOT],"invite_only":true,"encrypted":true,"available":true}).to_string()
            ],
        )
        .unwrap();
}

/// Assemble the shared store: the provisioning fixture's domain (registration,
/// resource, host engagement) on one root.
async fn ready() -> (matrix_common::Fixture, matrix_common::Fake, Collector) {
    let f = matrix_common::Fixture::new();
    enroll_owner_room(&f);
    let fake = matrix_common::Fake::start(true).await;
    let c = Collector::new(provisioning_config(&f, &fake.endpoint), f.store.clone()).unwrap();
    (f, fake, c)
}

fn placeholder() {}
