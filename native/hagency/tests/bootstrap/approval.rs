//! PC-C0 selectors (specs/task-rust-private-approval-wiring.spec.md).
//!
//! Both scenarios drive the REAL service composition through the actual
//! `hagency` binary with the fake TLS Matrix peer — no live homeserver — by
//! injecting the approval bot's own second credential section into the
//! development-driver configuration the bootstrap fixture already writes.
use super::*;
use serde_json::Value;
use std::io::{Seek, Write};

/// The product budget `fresh_approval_waiting` writes as `operation_ms`, and
/// the value the delivery drain below is derived from. The drain is a FAILURE
/// bound, so it must outlast everything the product is permitted to take —
/// startup plus one whole operation budget — or a loaded host reports a
/// roundtrip the product would still have completed.
const APPROVAL_OPERATION_MS: u64 = 20_000;

/// Inject the approval bot's own credential set (config.rs `approval`): a
/// SECOND identity/token/device/SDK root and a DM room, never the pooled
/// ordinary `HostConfig`. `anchors` empty models the absent enrollment.
async fn with_approval(f: &Fixture, anchors: bool) {
    let path = f.state_dir.join("development-driver.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["approval"] = json!({
        "origin": f.fake.endpoint,
        "server_name": "example.test",
        "registration_fingerprint": "a".repeat(64),
        "engagement_id": config["matrix"]["engagement_id"],
        "registration_generation": 1,
        "transport_generation": 1,
        "sender_mxid": "@approval:example.test",
        "device_id": "APPROVAL_DEVICE",
        "rooms": [{"id": "!private:example.test", "generation": 1,
                   "privacy": {"kind": "direct", "human_mxid": "@owner:example.test"}}],
        "peer_masters": if anchors {
            json!([{"user_id": "@owner:example.test",
                    "master_key": matrix_sdk_crypto::vodozemac::Ed25519SecretKey::new().public_key().to_base64()}])
        } else {
            json!([])
        }
    });
    // The fixture's own `write_new` created this file already; rewrite it in
    // place (the `scope.rs` shape) instead of re-creating it, and let the
    // write itself fail loudly rather than swallowing `AlreadyExists`.
    let mut file = hagency_store::private::open(&path, false).unwrap();
    file.set_len(0).unwrap();
    file.rewind().unwrap();
    file.write_all(&serde_json::to_vec(&config).unwrap())
        .unwrap();
    drop(file);
    hagency_store::private::write_new(
        &f.state_dir.join("approval.access_token"),
        common::TOKEN.as_bytes(),
    )
    .unwrap();
    hagency_store::private::write_new(&f.state_dir.join("approval.sdk_key"), &[42; 32]).unwrap();
}

fn fresh_approval(f: &Fixture, anchor: String) {
    fresh_approval_waiting(f, anchor, 10_000);
}
/// `owner_wait_ms` explicit: the answered roundtrips need a wait long enough
/// for the owner's encrypted reply to cross it; the expiry selector needs one
/// short enough to elapse inside the startup watchdog.
fn fresh_approval_waiting(f: &Fixture, anchor: String, owner_wait_ms: u64) {
    use sha2::{Digest, Sha256};
    let probe = std::path::PathBuf::from(env!("CARGO_BIN_EXE_hagency-approval-mcp-probe"))
        .canonicalize()
        .unwrap();
    let digest: String = Sha256::digest(std::fs::read(&probe).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let path = f.state_dir.join("development-driver.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["executable"] = json!(probe);
    config["executable_sha256"] = json!(digest);
    // Without this the owner wait is the 1 s default, and the owner's verdict
    // has to cross the approval bot's encrypted send and sync round trip inside
    // it. On a small hosted runner the card send alone took about 1.4 s: the
    // approval had expired before it landed, the command approval stayed pending
    // and the transport timed out. These fixtures test the round trip, not wait
    // expiry. The wait plus its 5 s response reserve must fit in the operation
    // budget that remains at turn start, so both are raised together, as the
    // configured-fleet approval fixtures already do.
    config["operation_ms"] = json!(APPROVAL_OPERATION_MS);
    config["approval_owner_wait_ms"] = json!(owner_wait_ms);
    // The runner is a REAL child whose initialize handshake answers through a
    // loaded host. The template's 1500 ms RPC bound fired under load (see
    // .peer/report-96.md: spawn_done -> stop_requested measured 1505 ms, the
    // failure record named stage "initialize" / transport timeout) and the
    // one-attempt profile then ended the turn before any card existed
    // (polls=0, cards=0). 2000 is the product's own ceiling
    // (hagency-execution Limits::validate) and the configured_fleet sibling
    // fixture's value; the owner-wait reserve stays max(response_ms, 5000)
    // either way, so no expiry-path timing moves.
    config["response_ms"] = json!(2_000);
    config["approval"] = json!({
        "origin":f.fake.endpoint,"server_name":"example.test","registration_fingerprint":"a".repeat(64),
        "engagement_id":config["matrix"]["engagement_id"],"registration_generation":1,"transport_generation":1,
        "sender_mxid":support::BOT,"device_id":support::DEVICE,
        "rooms":[{"id":support::ROOM,"generation":1,"privacy":{"kind":"direct","human_mxid":support::crypto::HUMAN}}],
        "peer_masters":[{"user_id":support::crypto::HUMAN,"master_key":anchor}]
    });
    let mut file = hagency_store::private::open(&path, false).unwrap();
    file.set_len(0).unwrap();
    file.rewind().unwrap();
    file.write_all(&serde_json::to_vec(&config).unwrap())
        .unwrap();
    hagency_store::private::write_new(
        &f.state_dir.join("approval.access_token"),
        support::APPROVAL_TOKEN.as_bytes(),
    )
    .unwrap();
    hagency_store::private::write_new(&f.state_dir.join("approval.sdk_key"), &[42; 32]).unwrap();
    hagency_store::private::write_new(
        &f.state_dir.join("approval.ca.pem"),
        include_bytes!("../../../hagency-matrix/tests/fixtures/ca.pem"),
    )
    .unwrap();
    hagency_store::private::write_new(&f.work.join("approval-mcp.roundtrip"), b"fixture only")
        .unwrap();
    assert!(!f.state_dir.join("approval-sdk").exists());
    let sql = rusqlite::Connection::open(f.state_dir.join("domain.sqlite3")).unwrap();
    assert_eq!(
        sql.query_row("SELECT COUNT(*) FROM current_approval_bindings", [], |r| {
            r.get::<_, u64>(0)
        })
        .unwrap(),
        0
    );
}

/// The public "waiting for owner" status notice (bridge-matrix.js:2578-2598,
/// sent at :9377). It arrives on the ORDINARY leg — the AGENT's own transport,
/// never the approval bot's — which is itself the (a) assertion: `is_approval`
/// is false for it and true for the card's leg.
const NOTICE_PREFIX: &str = "/send/m.room.message/approval_status_";

fn is_notice(request: &common::Request) -> bool {
    request.method == "PUT" && request.target.contains(NOTICE_PREFIX)
}

/// Answer the notice 200 and hand back the observed `(target, content, auth)`
/// so the scenario asserts the exact TS packet rather than tolerating it.
fn accept_notice(request: common::Request) -> (String, Value, String) {
    let target = request.target.clone();
    let auth = request
        .headers
        .get("authorization")
        .cloned()
        .unwrap_or_default();
    let content: Value = serde_json::from_slice(&request.body).unwrap();
    request.json(200, json!({"event_id": "$public_status"}));
    (target, content, auth)
}

/// Derive the expected packet from the delivered CARD (never guess it): TS
/// reuses `approval.agent`/`approval.project` verbatim (bridge-matrix.js:9377,
/// :2583-2596), so the notice must carry the card's own identity pair, the
/// task thread root, and `m.room.message` as the PUT event type.
#[allow(clippy::too_many_arguments)]
fn assert_public_notice(
    card: &Value,
    target: &str,
    content: &Value,
    auth: &str,
    thread_root: &str,
) {
    let detail = &card["content"]["com.agentchat.approval"];
    let agent = detail["agent"].as_str().unwrap();
    let project = detail["project"].as_str().unwrap();
    let expected = json!({
        "msgtype": "com.agentchat.approval.status.v1",
        "body": format!("Agent {agent} is waiting for approval from its owner."),
        "com.agentchat.approval": {
            "version": 1, "kind": "status", "agent": agent, "project": project,
            "state": "waiting_for_owner",
        },
        "m.relates_to": {
            "rel_type": "m.thread", "event_id": thread_root,
            "is_falling_back": true, "m.in_reply_to": {"event_id": thread_root},
        },
    });
    assert_eq!(
        content, &expected,
        "the exact TS packet (bridge-matrix.js:2583-2596)"
    );
    let path =
        "/_matrix/client/v3/rooms/!project:example.test/send/m.room.message/approval_status_";
    assert!(
        target.starts_with(path),
        "the exact PUT path (bridge-matrix.js:10823): {target}"
    );
    // Sender = the AGENT, not the approval bot: the notice rides the ordinary
    // agent token, never the approval bot's own.
    assert_eq!(
        auth,
        format!("Bearer {}", common::TOKEN),
        "the notice is spoken by the agent's own transport (bridge-matrix.js:9377)"
    );
}

async fn ordinary_leg(request: common::Request, notice: &mut Option<(String, Value, String)>) {
    if is_notice(&request) {
        *notice = Some(accept_notice(request));
        return;
    }
    if request.target.ends_with("/whoami") {
        request.json(200, common::who());
    } else if request.target.contains("/sync?") {
        request.json(200, common::sync("bootstrap"));
    } else if request.target.ends_with("/state") {
        request.json(200, common::state());
    } else {
        panic!("unexpected ordinary-leg request: {}", request.target);
    }
}

fn ordinary(request: common::Request) -> Option<(String, Value, String)> {
    if is_notice(&request) {
        return Some(accept_notice(request));
    }
    if request.target.ends_with("/whoami") {
        request.json(200, common::who());
    } else if request.target.contains("/sync?") {
        request.json(200, common::sync("bootstrap"));
    } else if request.target.ends_with("/state") {
        request.json(200, common::state());
    } else {
        panic!("unexpected ordinary request: {}", request.target);
    }
    None
}

/// `action: None` means the OWNER NEVER ANSWERS: the card is delivered and
/// nothing comes back, so the owner wait runs out and the host must answer the
/// runtime itself. Everything else about the composition is identical, so the
/// two paths differ only in what the owner does.
async fn roundtrip(plaintext_first: bool, action: Option<&str>) {
    let mut f = Fixture::new(false).await;
    let mut peer = support::crypto::Peer::for_sender(support::BOT, support::DEVICE).await;
    // The expiry selector has to outlive the whole owner wait inside the 15 s
    // startup watchdog, so its wait is shorter than the answered roundtrips'.
    // It is not shorter than the card send, though: this fixture's own note
    // above records a 1.4 s send on a small hosted runner, and a card that
    // lands after the cutoff is refused by the card gate and never delivered,
    // which would fail the "the owner really was asked" assertion below. 5 s
    // clears that with room and still leaves the watchdog the startup, the send
    // and the decline sequence.
    let owner_wait_ms: u64 = if action.is_some() { 10_000 } else { 5_000 };
    fresh_approval_waiting(&f, peer.anchor(), owner_wait_ms);
    let child = f.launch(true);
    // The drain must outlast the product's OWN declared budget, not a fixed
    // constant. `fresh_approval_waiting` sets `operation_ms` to
    // `APPROVAL_OPERATION_MS`, the whole budget the service may take for this
    // operation; a bound shorter than startup plus that budget fires before a
    // loaded host can finish, and the assert below then reports a roundtrip the
    // product would still have completed (`cards=0` — the card had not even been
    // issued yet). The drain stays a FAILURE bound: it is only reached if the
    // product never answers at all.
    let until = tokio::time::Instant::now()
        + STARTUP_WATCHDOG
        + std::time::Duration::from_millis(APPROVAL_OPERATION_MS);
    let mut plaintext_sent = false;
    let mut encrypted_sent = false;
    let mut observed_startup = false;
    let mut polls = 0;
    // The public status notice the AGENT sends once the card is accepted
    // (bridge-matrix.js:9377). It rides the ordinary leg, so `ordinary`
    // captures it; both exit paths assert the exact TS packet below.
    let mut notice: Option<(String, Value, String)> = None;
    let response_path = f.work.join("approval-mcp.response");
    while !response_path.exists() && tokio::time::Instant::now() < until {
        let request = tokio::select! {
            request = f.fake.next() => request,
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => continue,
        };
        if !support::is_approval(&request) {
            if let Some(seen) = ordinary(request) {
                notice = Some(seen);
            }
        } else if request.target.contains("/sync?") && !peer.events.is_empty() {
            polls += 1;
            let Some(action) = action else {
                // The owner never answers. Keep serving empty batches so the
                // service stays live while its owner wait runs out; the host's
                // own decline is what ends this loop.
                request.json(
                    200,
                    json!({"next_batch":format!("owner-poll-{polls}"),"to_device":{"events":[]},
                        "rooms":{"join":{support::ROOM:{"timeline":{"events":[],"limited":false},"state":{"events":[]}}}}}),
                );
                continue;
            };
            let detail = &peer.events[0]["content"]["com.agentchat.approval"];
            let verdict = json!({"msgtype":"com.agentchat.approval.verdict.v1","body":"Owner button action",
                "com.agentchat.approval":{"version":1,"kind":"verdict","agent":detail["agent"],"project":detail["project"],
                    "project_room_id":detail["project_room_id"],"request_id":detail["request_id"],"input_digest":detail["input_digest"],"action":action}});
            let mut sync = json!({"next_batch":format!("owner-poll-{polls}"),"to_device":{"events":[]},
                "rooms":{"join":{support::ROOM:{"timeline":{"events":[],"limited":false},"state":{"events":[]}}}}});
            if plaintext_first && !plaintext_sent {
                sync["rooms"]["join"][support::ROOM]["timeline"]["events"] = json!([{
                    "type":"m.room.message","sender":support::crypto::HUMAN,"event_id":"$plaintext_verdict","origin_server_ts":1,"content":verdict
                }]);
                plaintext_sent = true;
            } else if !encrypted_sent {
                if plaintext_first {
                    // The next sync cannot start until the previous original
                    // SDK batch is durably settled. Read-only DB observation.
                    let sql =
                        rusqlite::Connection::open(f.state_dir.join("domain.sqlite3")).unwrap();
                    let (state, choice): (String, Option<String>) = sql
                        .query_row("SELECT state,choice FROM owner_approvals", [], |r| {
                            Ok((r.get(0)?, r.get(1)?))
                        })
                        .unwrap();
                    assert_eq!(state, "pending");
                    assert_eq!(choice, None);
                    assert!(!response_path.exists());
                }
                let room = support::ROOM.try_into().unwrap();
                sync["to_device"] = peer.inbound_room_key(room).await["to_device"].clone();
                sync["rooms"]["join"][support::ROOM]["timeline"]["events"] =
                    json!([peer.owner_event(room, verdict).await]);
                encrypted_sent = true;
            }
            request.json(200, sync);
        } else {
            if request.target.ends_with("/keys/query")
                && peer.writes.is_empty()
                && !observed_startup
            {
                // Hold the actual enrollment response while calling the real
                // service endpoint: startup must not starve its HTTP server.
                assert_eq!(
                    f.capabilities().await["development_execution"]["state"],
                    "enrolling"
                );
                assert_eq!(f.attempts(), 0);
                observed_startup = true;
            }
            support::respond(request, &mut peer).await;
        }
    }
    assert!(
        response_path.exists(),
        "native roundtrip missing: polls={polls}, encrypted={encrypted_sent}, cards={}, enrollment_writes={}\n{}",
        peer.events.len(),
        peer.writes.len(),
        String::from_utf8_lossy(
            &std::fs::read(f.root.path().join("native.stderr")).unwrap_or_default()
        )
    );
    let response: Value = serde_json::from_slice(&std::fs::read(response_path).unwrap()).unwrap();
    assert_eq!(
        response,
        json!({"id":7,"result":{"decision":if action == Some("approve_once") {"accept"} else {"decline"}}}),
        "an unanswered owner wait must yield the family's own decline"
    );
    // The card really was delivered; the owner simply never acted on it.
    assert_eq!(peer.events.len(), 1);
    if action.is_none() {
        assert!(!encrypted_sent && !plaintext_sent, "the owner sent nothing");
        let sql = rusqlite::Connection::open(f.state_dir.join("domain.sqlite3")).unwrap();
        let (choice, reason): (String, Option<String>) = sql
            .query_row(
                "SELECT a.choice,(SELECT r.denial_reason FROM approval_verdict_receipts r WHERE r.request_id=a.id) FROM owner_approvals a",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(serde_json::from_str::<String>(&choice).unwrap(), "deny");
        assert_eq!(
            reason.as_deref(),
            Some("owner wait expired without an answer"),
            "the expiry is durably named, never inferred"
        );
        drop(sql);
        // The agent's public status notice went with the card (bridge-matrix.js:9377).
        let (target, content, auth) = notice
            .as_ref()
            .expect("the public status notice was sent with the card");
        assert_public_notice(&peer.events[0], target, content, auth, "$task_thread");
        // Fixture process cleanup is not native shutdown or sandbox qualification.
        drop(child);
        f.fake.close().await;
        return;
    }
    assert!(encrypted_sent);
    assert!(observed_startup);
    assert_eq!(plaintext_sent, plaintext_first);
    assert_eq!(peer.events.len(), 1);
    assert_eq!(peer.shares, 1);
    assert_eq!(peer.writes.len(), 5);
    assert_eq!(peer.claims, 1);
    assert_eq!(f.attempts(), 1);
    let sql = rusqlite::Connection::open(f.state_dir.join("domain.sqlite3")).unwrap();
    let (choice, receipts): (String, u64) = sql
        .query_row(
            "SELECT choice,(SELECT COUNT(*) FROM approval_verdict_receipts) FROM owner_approvals",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<String>(&choice).unwrap(),
        if action == Some("deny") {
            "deny"
        } else {
            "once"
        }
    );
    assert_eq!(receipts, 1);
    drop(sql);
    let (target, content, auth) = notice
        .as_ref()
        .expect("the public status notice was sent with the card");
    assert_public_notice(&peer.events[0], target, content, auth, "$task_thread");
    // Fixture process cleanup is not native shutdown or sandbox qualification.
    drop(child);
    f.fake.close().await;
}

#[tokio::test]
async fn native_private_approval_roundtrip_encrypted_owner() {
    roundtrip(false, Some("approve_once")).await;
    roundtrip(false, Some("deny")).await;
}

/// The live 2026-09-19 shape at fleet scale: the whole real composition, the
/// approval bot's own credential set, a card delivered to the owner's DM — and
/// an owner who never answers. The agent must survive that and the decline must
/// be the host's, recorded durably before it reached the wire.
#[tokio::test]
async fn native_fleet_approval_owner_wait_expiry() {
    roundtrip(false, None).await;
}

#[tokio::test]
async fn native_private_approval_roundtrip_plaintext_refused() {
    roundtrip(true, Some("approve_once")).await;
}

#[tokio::test]
async fn native_private_approval_startup_wrong_anchor() {
    let mut f = Fixture::new(false).await;
    let mut peer = support::crypto::Peer::for_sender(support::BOT, support::DEVICE).await;
    let different_owner = support::crypto::Peer::for_sender(support::BOT, support::DEVICE).await;
    assert_ne!(peer.anchor(), different_owner.anchor());
    fresh_approval(&f, different_owner.anchor());
    let mut child = f.launch(true);
    // ADR-183 decision 0: a wrong anchor is a refusal the bridge never turns
    // into an exit. The service keeps serving, names the refusal in its
    // status and readiness, and re-checks with backoff — the anchor is the
    // operator's to correct; no agent is admitted meanwhile.
    let until = tokio::time::Instant::now() + STARTUP_WATCHDOG;
    let refused = loop {
        tokio::select! {
            request = f.fake.next() => {
                assert!(support::is_approval(&request), "driver must not start before enrollment");
                support::respond(request, &mut peer).await;
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {
                let status = f.capabilities().await;
                if status["development_execution"]["state"] == "approval_refused" {
                    break status["development_execution"].clone();
                }
                assert!(
                    tokio::time::Instant::now() < until,
                    "the refusal never reached the status: {status}"
                );
            }
        }
    };
    assert_eq!(refused["matrix_error"], "recipients", "{refused}");
    assert!(child.still_owned(), "the service keeps serving");
    assert_eq!(
        f.ready().await,
        503,
        "readiness names the refusing component"
    );
    assert!(
        String::from_utf8_lossy(
            &std::fs::read(f.root.path().join("native.stderr")).unwrap_or_default()
        )
        .contains("approval startup refused"),
        "the refusal is named in the log"
    );
    assert_eq!(f.attempts(), 0);
    assert!(peer.events.is_empty());
    assert!(!f.work.join("approval-mcp.requests").exists());
    #[cfg(unix)]
    {
        child.request_shutdown();
        child.exited().await;
    }
    // Windows verifies the refusal/readiness above; dropping the fixture is
    // cleanup, not evidence of Unix graceful shutdown.
    drop(child);
    f.fake.close().await;
}

/// Scenario: A service-composed run delivers a request end to end.
///
/// PC-C0b: the delivery leg is observable through the test-only fixture.
/// The callback-capable probe — pinned by this test's OWN executable path
/// and sha, never a production switch — raises one real
/// `item/commandExecution/requestApproval` during the turn. The
/// composition's pump (PC-C0's wiring, untouched) takes the
/// single-consumer receiver, re-reads the card from the admitted domain
/// request and drives `send_private_approval_card` against the shared fake
/// peer. The scripted second-identity enrollment (test-side, pre-launch)
/// leaves the bot's SDK root enrolled so startup validates its original
/// Complete record. The fresh roundtrip tests above do not use this helper.
#[tokio::test]
async fn native_private_approval_delivery_is_wired() {
    use sha2::{Digest, Sha256};
    let mut f = Fixture::new(false).await;
    // The composition's engagement (the driver config's matrix section).
    let driver: Value = serde_json::from_slice(
        &std::fs::read(f.state_dir.join("development-driver.json")).unwrap(),
    )
    .unwrap();
    let engagement = driver["matrix"]["engagement_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The scripted host act: enroll the approval bot's second identity
    // against the shared fake peer, into the very state the composition
    // will run on. The anchor it returns is the fresh-account enrollment
    // key the driver config must carry.
    let (anchor, mut peer) = support::enroll(&f.state_dir, &mut f.fake, &engagement).await;
    // The approval section plus the probe pin: the executable and its sha
    // are the test's own choice — the composition's configured executable
    // is whatever the pinned path says (config.rs never changes).
    let probe = std::path::PathBuf::from(env!("CARGO_BIN_EXE_hagency-approval-mcp-probe"))
        .canonicalize()
        .unwrap();
    let probe_sha256: String = Sha256::digest(std::fs::read(&probe).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let path = f.state_dir.join("development-driver.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["executable"] = json!(probe);
    config["executable_sha256"] = json!(probe_sha256);
    // Same budgets, for the same reason, as the round-trip fixture above: on
    // the 1 s default owner wait a hosted runner's card send outlives the wait,
    // and this fixture tests that delivery is wired, not wait expiry.
    config["operation_ms"] = json!(20_000);
    config["approval_owner_wait_ms"] = json!(10_000);
    config["approval"] = json!({
        "origin": f.fake.endpoint,
        "server_name": "example.test",
        "registration_fingerprint": "a".repeat(64),
        "engagement_id": engagement,
        "registration_generation": 1,
        "transport_generation": 1,
        "sender_mxid": support::BOT,
        "device_id": support::DEVICE,
        "rooms": [{"id": support::ROOM, "generation": 2,
                   "privacy": {"kind": "direct", "human_mxid": "@owner:example.test"}}],
        "peer_masters": [{"user_id": "@owner:example.test", "master_key": anchor}]
    });
    {
        let mut file = hagency_store::private::open(&path, false).unwrap();
        file.set_len(0).unwrap();
        file.rewind().unwrap();
        file.write_all(&serde_json::to_vec(&config).unwrap())
            .unwrap();
    }
    hagency_store::private::write_new(
        &f.state_dir.join("approval.access_token"),
        support::APPROVAL_TOKEN.as_bytes(),
    )
    .unwrap();
    hagency_store::private::write_new(&f.state_dir.join("approval.sdk_key"), &[42; 32]).unwrap();

    let _child = f.launch(true);
    // Route the two interleaved legs of the shared fake peer until the run
    // reports its result: the ordinary worker leg (who/sync/state on the
    // worker token) and the approval leg (room refresh, key protocol,
    // secret share and the encrypted card itself on the bot's own token).
    // The two borrows (fake mutable, capabilities immutable) alternate
    // rather than share one select — requests buffer in the peer's channel
    // during the brief status polls.
    let mut notice: Option<(String, Value, String)> = None;
    let status = loop {
        let request = tokio::select! {
            request = f.fake.next() => Some(request),
            _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => None,
        };
        if let Some(request) = request {
            if support::is_approval(&request) {
                support::respond(request, &mut peer).await;
            } else {
                ordinary_leg(request, &mut notice).await;
            }
            continue;
        }
        let status = f.capabilities().await["development_execution"].clone();
        if matches!(
            status["state"].as_str(),
            Some("completed" | "unavailable" | "outcome_unknown" | "no_work")
        ) {
            break status;
        }
    };
    // The pump's send is still in flight when the run settles — the probe
    // holds the turn open with no terminal event, so the transport timeout
    // settles outcome_unknown BEFORE the card's HTTP round trip completes.
    // Keep servicing both legs until the event lands, bounded by the
    // fixture's own STARTUP_WATCHDOG (the same bound capabilities() polls
    // under — a fixed 10 s here raced a loaded host's round trip and the
    // card arrived after the window, leaving peer.events empty).
    let drain = tokio::time::Instant::now() + STARTUP_WATCHDOG;
    while peer.events.is_empty() && tokio::time::Instant::now() < drain {
        let request = tokio::select! {
            request = f.fake.next() => Some(request),
            _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => None,
        };
        if let Some(request) = request {
            if support::is_approval(&request) {
                support::respond(request, &mut peer).await;
            } else {
                ordinary_leg(request, &mut notice).await;
            }
        }
    }
    // Named failure: the card must be observed before the fixture watchdog
    // expires. Dump the child's stderr so a timeout names the pump's own
    // send outcome (a denied-at-deadline card is a different root cause
    // than a merely slow one).
    assert!(
        !peer.events.is_empty(),
        "approval card not delivered within the fixture watchdog\n--- native.stderr ---\n{}",
        String::from_utf8_lossy(
            &std::fs::read(f.root.path().join("native.stderr")).unwrap_or_default()
        )
    );
    // The scenario observes the DELIVERY, never the verdict: the probe
    // raises the requestApproval and then holds the turn open with no
    // terminal event (the owned probe's own discipline — "Done
    // intentionally has no turn terminal event"), so the designed end is
    // the transport timeout settling outcome_unknown, never protocol
    // completion. What must hold: one attempt, the card delivered, and the
    // pump gone when the worker dropped the sender at run end.
    assert_eq!(
        status["state"],
        "outcome_unknown",
        "the wired run: {status}\n--- native.stderr ---\n{}\n--- probe requests (exists={}) ---\n{}\n--- store ---\n{}",
        String::from_utf8_lossy(
            &std::fs::read(f.root.path().join("native.stderr")).unwrap_or_default()
        ),
        f.work.join("approval-mcp.requests").exists(),
        String::from_utf8_lossy(
            &std::fs::read(f.work.join("approval-mcp.requests")).unwrap_or_default()
        ),
        {
            let sql = rusqlite::Connection::open(f.state_dir.join("domain.sqlite3")).unwrap();
            let dispatch: String = sql.query_row("SELECT state||' lease='||COALESCE(lease_until,0)||' cap='||COALESCE(capability_until,0)||' fence='||fence FROM runner_dispatches WHERE id='dispatch'", [], |r| r.get(0)).unwrap();
            let task: String = sql
                .query_row(
                    "SELECT config FROM canonical_tasks WHERE id='task'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let approvals: i64 = sql
                .query_row("SELECT COUNT(*) FROM owner_approvals", [], |r| r.get(0))
                .unwrap();
            let contexts: i64 = sql
                .query_row("SELECT COUNT(*) FROM approval_contexts", [], |r| r.get(0))
                .unwrap();
            format!(
                "dispatch: {dispatch}\ntask: {task}\nowner_approvals: {approvals}\napproval_contexts: {contexts}"
            )
        }
    );
    assert_eq!(f.attempts(), 1);
    // The delivery observation: the approval leg decrypted exactly one
    // room event — the card built from the admitted domain request — and
    // the send completed under the cancellation token (an errored or
    // cancelled send would have left no decrypted event and the pump's
    // warning would name it).
    assert_eq!(peer.events.len(), 1, "the card was delivered");
    assert!(peer.events[0]["content"].is_object());
    assert_eq!(peer.shares, 1, "the room key was shared exactly once");
    assert_eq!(peer.writes.len(), 5, "startup kept the original enrollment");
    assert_eq!(
        peer.claims, 1,
        "startup did not regenerate the original session"
    );
    // The callback-capable probe is what ran (its own request log), and it
    // was driven by the ordinary dispatch — one attempt, no retries.
    assert!(f.work.join("approval-mcp.requests").exists());
    // Process cleanup only: `Running`'s Drop kills and reaps the child so
    // the fixture never leaks a zombie. It deliberately proves nothing
    // about the pump's self-termination — the pump task ends its
    // `while let` loop silently (no terminal log line) and its handle is
    // aborted, not joined, at shutdown, so the receiver's `None` is not
    // observable from outside the child; the delivery evidence above is
    // what this slice observes.
    drop(_child);
}

/// Scenario: The pump refuses without a fresh approval enrollment.
///
/// The same fixture with the enrollment anchors absent: the composition
/// refuses at startup with the named configuration failure — the pump is
/// never constructed, no collector is built from a config the ordinary
/// `Collector::new` would refuse, and no card is sent (the peer stays idle
/// because the process exits before any Matrix work).
#[tokio::test]
async fn native_private_approval_delivery_wiring_refuses_without_enrollment() {
    let mut f = Fixture::new(false).await;
    with_approval(&f, false).await;
    // The fixture's command() nulls stderr; re-capture it for the assertion.
    let mut command = f.command(true);
    command.stderr(std::process::Stdio::piped());
    let output = command.output().unwrap();
    assert!(
        !output.status.success(),
        "the composition accepted an approval section without enrollment anchors"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The named refusal (r1 item 4), not the shared `invalid or unavailable`
    // config label every `Failure::Config` emits: the enrollment leg of the
    // approval construction is what this scenario refuses.
    assert!(
        stderr.contains("approval enrollment refused"),
        "refusal was not the named enrollment failure: {stderr}"
    );
    f.fake.no_request().await;
}

/// ADR-183 decision 0: a component refusal does not exit the process. The
/// approval bot's startup is refused by the homeserver twice; the service
/// keeps serving with the refusal in its status and readiness, retries with
/// backoff, and once the homeserver answers the roundtrip completes as if
/// nothing had happened.
#[tokio::test]
async fn native_component_refusal_does_not_exit_the_process() {
    let mut f = Fixture::new(false).await;
    let mut peer = support::crypto::Peer::for_sender(support::BOT, support::DEVICE).await;
    fresh_approval_waiting(&f, peer.anchor(), 10_000);
    let mut child = f.launch(true);
    let until = tokio::time::Instant::now() + STARTUP_WATCHDOG + std::time::Duration::from_secs(10);
    let response_path = f.work.join("approval-mcp.response");
    let mut refused = 0u32;
    let mut refusal_seen: Option<(Value, u16)> = None;
    let mut encrypted_sent = false;
    let mut polls = 0;
    let mut notice: Option<(String, Value, String)> = None;
    while !response_path.exists() && tokio::time::Instant::now() < until {
        let request = tokio::select! {
            request = f.fake.next() => request,
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                if refused > 0 && refusal_seen.is_none() {
                    let status = f.capabilities().await;
                    if status["development_execution"]["state"] == "approval_refused" {
                        assert!(child.still_owned(), "the process is still serving");
                        refusal_seen = Some((status["development_execution"].clone(), f.ready().await));
                    }
                }
                continue;
            }
        };
        if support::is_approval(&request) && request.target.ends_with("/whoami") && refused < 2 {
            // The homeserver refuses the approval bot's startup, twice.
            refused += 1;
            request.json(503, json!({"errcode":"M_UNKNOWN","error":"fixture fault"}));
            continue;
        }
        if !support::is_approval(&request) {
            ordinary_leg(request, &mut notice).await;
        } else if request.target.contains("/sync?") && !peer.events.is_empty() {
            polls += 1;
            let detail = &peer.events[0]["content"]["com.agentchat.approval"];
            let verdict = json!({"msgtype":"com.agentchat.approval.verdict.v1","body":"Owner button action",
                "com.agentchat.approval":{"version":1,"kind":"verdict","agent":detail["agent"],"project":detail["project"],
                    "project_room_id":detail["project_room_id"],"request_id":detail["request_id"],"input_digest":detail["input_digest"],"action":"approve_once"}});
            let mut sync = json!({"next_batch":format!("owner-poll-{polls}"),"to_device":{"events":[]},
                "rooms":{"join":{support::ROOM:{"timeline":{"events":[],"limited":false},"state":{"events":[]}}}}});
            if !encrypted_sent {
                let room = support::ROOM.try_into().unwrap();
                sync["to_device"] = peer.inbound_room_key(room).await["to_device"].clone();
                sync["rooms"]["join"][support::ROOM]["timeline"]["events"] =
                    json!([peer.owner_event(room, verdict).await]);
                encrypted_sent = true;
            }
            request.json(200, sync);
        } else {
            support::respond(request, &mut peer).await;
        }
    }
    assert_eq!(refused, 2, "both refusals were served");
    let (status, ready) =
        refusal_seen.expect("the refusal was visible in the status while it lasted");
    assert_eq!(status["matrix_error"], "remote", "{status}");
    assert!(
        status["refresh_failures"].as_u64().is_some_and(|n| n >= 1),
        "{status}"
    );
    assert_eq!(ready, 503, "readiness named the refusing component");
    assert!(
        response_path.exists(),
        "the roundtrip completed after the refusals cleared: cards={}\n{}",
        peer.events.len(),
        String::from_utf8_lossy(
            &std::fs::read(f.root.path().join("native.stderr")).unwrap_or_default()
        )
    );
    let response: Value = serde_json::from_slice(&std::fs::read(response_path).unwrap()).unwrap();
    assert_eq!(response, json!({"id":7,"result":{"decision":"accept"}}));
    // The AGENT's public status notice went with the card, once the refusals
    // cleared (bridge-matrix.js:9377).
    let (target, content, auth) = notice
        .as_ref()
        .expect("the public status notice was sent with the card");
    assert_public_notice(&peer.events[0], target, content, auth, "$task_thread");
    assert!(child.still_owned());
    drop(child);
    f.fake.close().await;
}
