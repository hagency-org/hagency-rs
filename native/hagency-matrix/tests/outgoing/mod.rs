use super::*;
use crate::collector::observation::{Phase as ObservationPhase, Trace, observed};
use crate::{HostConfig, HostIdentity, HostIntakePlan, HostRoom, collector::fixtures as common};
use hagency_core::{
    commands::CommandNoticeRequest, ingress::VerifiedTaskRequest, task_intents::TaskDefinition,
    tasks::*,
};
use serde_json::{Value, json};
use std::{
    sync::atomic::Ordering,
    time::{SystemTime, UNIX_EPOCH},
};
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn config(f: &common::Fixture, endpoint: &str, identity: HostIdentity, direct: bool) -> HostConfig {
    HostConfig::new(
        identity,
        endpoint,
        common::TOKEN,
        f.root.path().join("sdk"),
        [42; 32],
        vec![HostRoom {
            room_id: "!project:example.test".into(),
            generation: 1,
            privacy: if direct {
                RoomPrivacy::Direct {
                    human_mxid: "@owner:example.test".into(),
                }
            } else {
                RoomPrivacy::Group {}
            },
        }],
        common::limits(),
    )
    .unwrap()
    .with_root_pem(include_bytes!("../fixtures/ca.pem"))
    .unwrap()
}
fn room(encrypted: bool) -> Value {
    let mut v = common::state();
    if !encrypted {
        v.as_array_mut()
            .unwrap()
            .retain(|e| e["type"] != "m.room.encryption");
    }
    v
}
async fn scripted<T: std::fmt::Debug, S: std::future::Future>(
    callsite: &'static str,
    variant: Option<&'static str>,
    operation: impl std::future::Future<Output = Result<T, Error>>,
    script: S,
) -> (Result<T, Error>, S::Output) {
    let trace = Trace::new(callsite, variant, None);
    let result = common::scripted(observed(trace.clone(), operation), script).await;
    assert!(trace.has(ObservationPhase::OwnerReturned));
    result
}
async fn ready(encrypted: bool, direct: bool) -> (common::Fixture, common::Fake, Collector) {
    ready_named(encrypted, direct, "outgoing bootstrap", None).await
}
/// Board #61 row 3: like `ready` but the collector carries the representative
/// credential the provisioning custody verified, so the send path can re-
/// invite a kicked agent (bridge-matrix.js:10912-10918).
async fn ready_with_representative(
    encrypted: bool,
    direct: bool,
    callsite: &'static str,
) -> (common::Fixture, common::Fake, Collector) {
    let f = common::Fixture::new();
    let mut fake = common::Fake::start(true).await;
    let mut config = config(&f, &fake.endpoint, f.identity.clone(), direct);
    config.limits = common::limits();
    config = config
        .with_representative("synthetic-representative-token-not-real")
        .unwrap();
    let c = Collector::new(config, f.store.clone()).unwrap();
    let cancel = CancellationToken::new();
    let (r, ()) = scripted(callsite, None, c.collect(&cancel), async {
        fake.next().await.json(200, common::who());
        fake.next().await.json(200, common::sync("boot"));
        fake.next().await.json(200, room(encrypted));
    })
    .await;
    r.unwrap();
    f.store
        .resolve_verified_matrix_session(SessionBinding {
            id: "root".into(),
            engagement_id: f.identity.transport.engagement_id.clone(),
            room_id: "!project:example.test".into(),
            thread_root: None,
        })
        .await
        .unwrap();
    (f, fake, c)
}
async fn ready_named(
    encrypted: bool,
    direct: bool,
    callsite: &'static str,
    variant: Option<&'static str>,
) -> (common::Fixture, common::Fake, Collector) {
    ready_named_limits(encrypted, direct, callsite, variant, common::limits()).await
}
/// #81: the recovery-restore family shares one runtime with the fake peer;
/// the tight tier's headers=400ms was crossed by scheduler starvation, not
/// by the product (the under-load trace: collector Err(Timeout) after
/// 467ms, script progress 0). The load tier stays strictly below
/// `Limits::default()`, so a real transport refusal still fails. The
/// bounds-wire-failures family KEEPS the tight tier via plain
/// `ready_named` — its slow_headers/slow_body variants ARE deliberate
/// deadline tests (700ms headers vs the 400ms bound; a 400ms body gap vs
/// the 200ms body-idle).
async fn ready_named_under_load(
    encrypted: bool,
    direct: bool,
    callsite: &'static str,
    variant: Option<&'static str>,
) -> (common::Fixture, common::Fake, Collector) {
    ready_named_limits(encrypted, direct, callsite, variant, common::load_limits()).await
}
async fn ready_named_limits(
    encrypted: bool,
    direct: bool,
    callsite: &'static str,
    variant: Option<&'static str>,
    limits: crate::Limits,
) -> (common::Fixture, common::Fake, Collector) {
    let f = common::Fixture::new();
    let mut fake = common::Fake::start(true).await;
    let mut config = config(&f, &fake.endpoint, f.identity.clone(), direct);
    config.limits = limits;
    let c = Collector::new(config, f.store.clone()).unwrap();
    let cancel = CancellationToken::new();
    let (r, ()) = scripted(callsite, variant, c.collect(&cancel), async {
        fake.next().await.json(200, common::who());
        fake.next().await.json(200, common::sync("boot"));
        fake.next().await.json(200, room(encrypted));
    })
    .await;
    r.unwrap();
    f.store
        .resolve_verified_matrix_session(SessionBinding {
            id: "root".into(),
            engagement_id: f.identity.transport.engagement_id.clone(),
            room_id: "!project:example.test".into(),
            thread_root: None,
        })
        .await
        .unwrap();
    (f, fake, c)
}
async fn final_claim(f: &common::Fixture) -> ReplyClaim {
    final_claim_named(f, "task").await
}
async fn final_claim_named(f: &common::Fixture, name: &str) -> ReplyClaim {
    f.store
        .create_canonical_task(name.into(), "root".into(), "Finish result".into(), now())
        .await
        .unwrap();
    f.store
        .enqueue_dispatch(DispatchInput {
            id: format!("run_{name}"),
            session_id: "root".into(),
            task_id: Some(name.into()),
            resources: vec![],
            payload: json!({"instruction":"fixture"}),
        })
        .await
        .unwrap();
    let cap = f
        .store
        .claim_dispatch("runner".into(), now(), 60_000, 120_000, 1)
        .await
        .unwrap()
        .unwrap();
    f.store.start_dispatch(cap.clone(), now()).await.unwrap();
    f.store
        .mutate_task(
            cap.clone(),
            name.into(),
            "done".into(),
            TaskMutation::Transition {
                status: TaskState::Done,
                waiting_reason: None,
                waiting_until: None,
            },
            now(),
        )
        .await
        .unwrap();
    f.store
        .runner_command(
            cap.clone(),
            RunnerCommand::SubmitFinalReply(FinalReply {
                call_id: "final".into(),
                body: "Answer **verified** 中文".into(),
                incidental: false,
            }),
        )
        .await
        .unwrap();
    f.store
        .complete_dispatch(cap, json!({"observed":"fixture completed"}), now())
        .await
        .unwrap();
    f.store.claim_final_reply(60_000).await.unwrap().unwrap()
}
async fn preflight(fake: &mut common::Fake, encrypted: bool) {
    let r = fake.next().await;
    assert_eq!(r.method, "GET");
    assert_eq!(r.target, "/_matrix/client/v3/account/whoami");
    r.json(200, common::who());
    let r = fake.next().await;
    assert!(r.target.ends_with("/state"));
    r.json(200, room(encrypted));
}
async fn plain_wire(fake: &mut common::Fake) -> common::Request {
    preflight(fake, false).await;
    preflight(fake, false).await;
    let r = fake.next().await;
    assert_eq!(r.method, "PUT");
    assert!(r.target.contains("/send/m.room.message/"));
    assert_eq!(
        r.headers["authorization"],
        format!("Bearer {}", common::TOKEN)
    );
    r
}

/// Task #1 send-side: the activity envelope's exact Matrix content
/// (lib/matrix-activity.js:4-13). First send (no anchor) keeps the thread
/// relation and the plain body; the edit (anchor) stars the body, moves
/// the plain content into `m.new_content`, and swaps the relation to
/// `m.replace` of the anchor — the "same event edited in place" wire shape.
#[test]
fn native_matrix_activity_envelope_first_send_and_edit() {
    // First send: no anchor.
    let first = apply_activity_envelope(
        json!({"msgtype":"m.notice","body":"⏳ 已开始处理，等待运行器的下一步事件\n已运行 0 秒 · 工具调用 0 次，已返回 0 次",
               "m.relates_to":{"rel_type":"m.thread","event_id":"$root","is_falling_back":true,"m.in_reply_to":{"event_id":"$root"}}}),
        "run_1",
        None,
    );
    assert_eq!(first["msgtype"], "m.notice");
    assert_eq!(first["io.hagency.activity"]["dispatch_id"], "run_1");
    assert_eq!(first["m.relates_to"]["rel_type"], "m.thread");
    assert!(first.get("m.new_content").is_none());
    assert!(first.get("m.replace").is_none());
    assert!(!first["body"].as_str().unwrap().starts_with("* "));

    // The edit: an anchor swaps the relation and stars the body.
    let plain = json!({"msgtype":"m.notice","body":"⏳ 正在运行命令\n已运行 4 秒 · 工具调用 1 次，已返回 0 次",
                       "m.relates_to":{"rel_type":"m.thread","event_id":"$root","is_falling_back":true,"m.in_reply_to":{"event_id":"$root"}}});
    let edit = apply_activity_envelope(plain.clone(), "run_1", Some("$activity_first"));
    assert_eq!(
        edit["body"],
        "* ⏳ 正在运行命令\n已运行 4 秒 · 工具调用 1 次，已返回 0 次"
    );
    // The replacement is the plain content: unstarred body, thread
    // relation intact, activity key intact — no replace relation (TS
    // matrix-activity.js builds `next` before adding the replace link).
    assert_eq!(edit["m.new_content"]["body"], plain["body"]);
    assert_eq!(
        edit["m.new_content"]["m.relates_to"]["rel_type"],
        "m.thread"
    );
    assert_eq!(
        edit["m.new_content"]["io.hagency.activity"]["dispatch_id"],
        "run_1"
    );
    assert!(edit["m.new_content"].get("m.replace").is_none());
    assert_eq!(edit["m.relates_to"]["rel_type"], "m.replace");
    assert_eq!(edit["m.relates_to"]["event_id"], "$activity_first");
    assert_eq!(edit["io.hagency.activity"]["dispatch_id"], "run_1");
}

/// The kind shape `activity:<dispatch>:<revision>[:<anchor>]` (task #1):
/// dispatch ids are colon-free, the revision is numeric, the anchor is the
/// remainder (a Matrix event id may itself carry colons).
#[test]
fn native_matrix_activity_kind_parse() {
    assert_eq!(
        parse_activity_notice("activity:run_1:1"),
        Some(("run_1".to_owned(), None))
    );
    assert_eq!(
        parse_activity_notice("activity:run_1:2:$activity_first"),
        Some(("run_1".to_owned(), Some("$activity_first".to_owned())))
    );
    assert_eq!(
        parse_activity_notice("activity:run_1:3:$a:b:c"),
        Some(("run_1".to_owned(), Some("$a:b:c".to_owned())))
    );
    assert_eq!(parse_activity_notice("ack"), None);
    assert_eq!(parse_activity_notice("activity:run_1"), None);
}
fn state(f: &common::Fixture, id: &str) -> String {
    rusqlite::Connection::open(f.root.path().join("domain/domain.sqlite3"))
        .unwrap()
        .query_row("SELECT state FROM final_replies WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
}
/// The journaled transaction id of a final reply, read straight from the
/// domain row. Task #9 asserts a resumed send reuses this exact value.
fn transaction_id(f: &common::Fixture, id: &str) -> String {
    rusqlite::Connection::open(f.root.path().join("domain/domain.sqlite3"))
        .unwrap()
        .query_row(
            "SELECT transaction_id FROM final_replies WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}
async fn stop_sdk(c: Collector) {
    c.inner
        .owner
        .lock()
        .await
        .take()
        .unwrap()
        .close()
        .await
        .unwrap();
    drop(c);
}
#[tokio::test]
async fn native_matrix_outgoing_kicked_agent_rejoins_and_the_message_is_delivered() {
    // Board #11 acceptance: a send that fails on membership re-invites the
    // path back — the agent rejoins and the message is delivered
    // (bridge-matrix.js:10888-10950). The fixture state drives the exact
    // moment: the draft preflight sees both joined, the loop preflight sees
    // the agent kicked, the rejoin restores membership, the write proceeds.
    let (f, mut fake, c) = ready(false, false).await;
    let claim = final_claim(&f).await;
    let cancel = CancellationToken::new();
    let (result, ()) = common::scripted(c.send_final(claim.clone(), &cancel), async {
        // Draft and loop preflights: membership intact on every read — the
        // kick is invisible to the observations and surfaces at the write.
        preflight(&mut fake, false).await;
        preflight(&mut fake, false).await;
        // The send: the server refuses the write on membership (the native
        // form of the retained `e.message.includes('membership')` catch).
        let refused = fake.next().await;
        assert_eq!(refused.method, "PUT");
        assert!(refused.target.contains("/send/m.room.message/"));
        let transaction = refused.target.rsplit('/').next().unwrap().to_owned();
        refused.json(403, json!({"errcode":"M_FORBIDDEN","error":"not in room"}));
        // The rejoin (bridge-matrix.js:10936-10943, the join half of the
        // retained invite-then-join pair): POST /join as the agent itself.
        let r = fake.next().await;
        assert_eq!(r.method, "POST");
        assert!(
            r.target
                .contains("/_matrix/client/v3/join/!project:example.test")
        );
        assert_eq!(
            r.headers["authorization"],
            format!("Bearer {}", common::TOKEN)
        );
        r.json(200, json!({"room_id": "!project:example.test"}));
        // The resend: the very write the kick interrupted, SAME transaction
        // id — a server that somehow accepted before refusing dedupes.
        let resent = fake.next().await;
        assert_eq!(resent.method, "PUT");
        assert!(
            resent
                .target
                .ends_with(&format!("/send/m.room.message/{transaction}"))
        );
        let body: Value = serde_json::from_slice(&resent.body).unwrap();
        resent.json(200, json!({"event_id": "$rejoined"}));
        assert_eq!(body["body"], "Answer **verified** 中文");
    })
    .await;
    assert_eq!(result.unwrap().state, OutgoingState::Delivered);
    assert_eq!(state(&f, &claim.id), "delivered");
    fake.quiesced(fake.requests(), &common::limits()).await;
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}

/// Board #61 row 3 (TS bridge-matrix.js:10912-10918): the INVITE half of the
/// rejoin-on-kick pair. With the representative credential the provisioning
/// custody verified riding the collector, a membership-refused send is
/// preceded by a representative invite of the kicked agent — then the agent
/// joins and the same transaction is resent. The invite is observable on the
/// wire: its own bearer, the agent's MXID in the body.
#[tokio::test]
async fn native_matrix_outgoing_kicked_agent_is_reinvited_by_the_representative_then_rejoins() {
    let (f, mut fake, c) = ready_with_representative(false, false, "reinvite bootstrap").await;
    let claim = final_claim(&f).await;
    let cancel = CancellationToken::new();
    let (result, ()) = common::scripted(c.send_final(claim.clone(), &cancel), async {
        preflight(&mut fake, false).await;
        preflight(&mut fake, false).await;
        // The kick surfaces at the write itself, as in the join-only test.
        let refused = fake.next().await;
        assert_eq!(refused.method, "PUT");
        assert!(refused.target.contains("/send/m.room.message/"));
        let transaction = refused.target.rsplit('/').next().unwrap().to_owned();
        refused.json(403, json!({"errcode":"M_FORBIDDEN","error":"not in room"}));
        // The invite (bridge-matrix.js:10912-10918, the appservice half):
        // POST /rooms/{id}/invite as the REPRESENTATIVE, body naming the
        // agent, exactly as provisioning's rooms custody invites at creation.
        let invite = fake.next().await;
        assert_eq!(invite.method, "POST");
        assert!(
            invite
                .target
                .contains("/_matrix/client/v3/rooms/!project:example.test/invite")
        );
        assert_eq!(
            invite.headers["authorization"],
            "Bearer synthetic-representative-token-not-real"
        );
        let body: Value = serde_json::from_slice(&invite.body).unwrap();
        assert_eq!(body["user_id"], "@worker:example.test");
        invite.json(200, json!({}));
        // The rejoin (bridge-matrix.js:10936-10943): POST /join as the agent.
        let join = fake.next().await;
        assert_eq!(join.method, "POST");
        assert!(
            join.target
                .contains("/_matrix/client/v3/join/!project:example.test")
        );
        assert_eq!(
            join.headers["authorization"],
            format!("Bearer {}", common::TOKEN)
        );
        join.json(200, json!({"room_id": "!project:example.test"}));
        // The resend: the same write, same transaction id.
        let resent = fake.next().await;
        assert_eq!(resent.method, "PUT");
        assert!(
            resent
                .target
                .ends_with(&format!("/send/m.room.message/{transaction}"))
        );
        resent.json(200, json!({"event_id": "$reinvited"}));
    })
    .await;
    assert_eq!(result.unwrap().state, OutgoingState::Delivered);
    assert_eq!(state(&f, &claim.id), "delivered");
    fake.quiesced(fake.requests(), &common::limits()).await;
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}

/// Board #61 row 6 (TS bridge-matrix.js:3374-3385): an INCIDENTAL answer to a
/// group question with no source thread STARTS a thread rooted at that
/// question — root == reply target, falling back to a plain reply for clients
/// without thread rendering. Goes through the real send path (submit → claim
/// → send_final → sdk → the wire), with the question admitted as the
/// dispatch's addressed input so the reply names it.
#[tokio::test]
async fn native_matrix_outgoing_incidental_group_answer_starts_a_thread_at_the_question() {
    let (f, mut fake, c) = ready(false, false).await;
    // Admit the question exactly as the notice intake does: a top-level group
    // message addressing the agent, so its route has NO thread root.
    let cancel = CancellationToken::new();
    let event = json!({"event_id":"$question","sender":"@owner:example.test","type":"m.room.message","origin_server_ts":now(),"content":{"msgtype":"m.text","body":"Please implement","m.mentions":{"user_ids":["@worker:example.test"]}}});
    let sync = json!({"next_batch":"question","rooms":{"join":{"!project:example.test":{"timeline":{"limited":false,"events":[event]},"state":{"events":[]}}}},"to_device":{"events":[]}});
    let (r, ()) = scripted(
        "incidental intake",
        None,
        c.intake(HostIntakePlan::new(vec!["root".into()]).unwrap(), &cancel),
        async {
            fake.next().await.json(200, common::who());
            fake.next().await.json(200, sync);
            fake.next().await.json(200, room(false));
        },
    )
    .await;
    r.unwrap();
    let inbox = f.store.inbox("root".into(), 0, 10, None).await.unwrap();
    let sequence = inbox[0].message.sequence;
    f.store
        .create_canonical_task("task".into(), "root".into(), "Finish result".into(), now())
        .await
        .unwrap();
    f.store
        .enqueue_inbox_dispatch(
            DispatchInput {
                id: "run_task".into(),
                session_id: "root".into(),
                task_id: Some("task".into()),
                resources: vec![],
                payload: json!({"instruction":"fixture"}),
            },
            vec![sequence],
        )
        .await
        .unwrap();
    let cap = f
        .store
        .claim_dispatch("runner".into(), now(), 60_000, 120_000, 1)
        .await
        .unwrap()
        .unwrap();
    f.store.start_dispatch(cap.clone(), now()).await.unwrap();
    f.store
        .mutate_task(
            cap.clone(),
            "task".into(),
            "done".into(),
            TaskMutation::Transition {
                status: TaskState::Done,
                waiting_reason: None,
                waiting_until: None,
            },
            now(),
        )
        .await
        .unwrap();
    f.store
        .runner_command(
            cap.clone(),
            RunnerCommand::SubmitFinalReply(FinalReply {
                call_id: "final".into(),
                body: "Progress: still working".into(),
                incidental: true,
            }),
        )
        .await
        .unwrap();
    f.store
        .complete_dispatch(cap, json!({"observed":"fixture completed"}), now())
        .await
        .unwrap();
    let claim = f.store.claim_final_reply(60_000).await.unwrap().unwrap();
    let (result, body) = scripted(
        "incidental send",
        None,
        c.send_final(claim.clone(), &cancel),
        async {
            let req = plain_wire(&mut fake).await;
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            req.json(200, json!({"event_id":"$threaded"}));
            body
        },
    )
    .await;
    assert_eq!(result.unwrap().state, OutgoingState::Delivered);
    assert_eq!(state(&f, &claim.id), "delivered");
    // The incidental shape: a NEW thread rooted at the question itself,
    // falling back to a plain reply for thread-less clients.
    assert_eq!(body["m.relates_to"]["rel_type"], "m.thread");
    assert_eq!(body["m.relates_to"]["event_id"], "$question");
    assert_eq!(body["m.relates_to"]["is_falling_back"], true);
    assert_eq!(
        body["m.relates_to"]["m.in_reply_to"]["event_id"],
        "$question"
    );
    fake.quiesced(fake.requests(), &common::limits()).await;
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}

#[tokio::test]
async fn native_matrix_outgoing_plain_final_actual_https_formatted_and_idempotent_receipt() {
    let (f, mut fake, c) = ready(false, false).await;
    let claim = final_claim(&f).await;
    let cancel = CancellationToken::new();
    let (result, body) = common::scripted(c.send_final(claim.clone(), &cancel), async {
        let req = plain_wire(&mut fake).await;
        assert_eq!(state(&f, &claim.id), "sending");
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        req.json(200, json!({"event_id":"$accepted"}));
        body
    })
    .await;
    assert_eq!(result.unwrap().state, OutgoingState::Delivered);
    assert_eq!(state(&f, &claim.id), "delivered");
    assert_eq!(body["body"], "Answer **verified** 中文");
    assert!(
        body["formatted_body"]
            .as_str()
            .unwrap()
            .contains("<strong>verified</strong>")
    );
    assert!(body.get("m.relates_to").is_none());
    assert!(c.send_final(claim, &cancel).await.unwrap().replayed);
    fake.quiesced(fake.requests(), &common::limits()).await;
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}
#[tokio::test]
async fn native_matrix_outgoing_recovery_accepted_response_survives_busy_lost_domain_reply_and_restart()
 {
    for fault in [1, 2] {
        let variant_label = Some(if fault == 1 {
            "busy"
        } else {
            "lost domain reply"
        });
        let (f, mut fake, c) =
            ready_named(false, false, "accepted recovery bootstrap", variant_label).await;
        let claim = final_claim(&f).await;
        let cancel = CancellationToken::new();
        c.inner.outgoing_fault.store(fault, Ordering::SeqCst);
        let (result, ()) = scripted(
            "accepted recovery send",
            variant_label,
            c.send_final(claim.clone(), &cancel),
            async {
                plain_wire(&mut fake)
                    .await
                    .json(200, json!({"event_id":"$journaled"}));
            },
        )
        .await;
        assert_eq!(
            result,
            Err(if fault == 1 {
                Error::Busy
            } else {
                Error::OutcomeUnknown
            })
        );
        assert!(f.available().await);
        assert_eq!(
            state(&f, &claim.id),
            if fault == 1 { "sending" } else { "delivered" }
        );
        stop_sdk(c).await;
        let c = Collector::new(
            config(&f, &fake.endpoint, f.identity.clone(), false),
            f.store.clone(),
        )
        .unwrap();
        let r = observed(
            Trace::new("accepted recovery resume", variant_label, None),
            c.resume_outgoing_custody(&cancel),
        )
        .await
        .unwrap();
        assert_eq!(r.state, OutgoingState::Delivered);
        assert!(r.replayed);
        fake.quiesced(fake.requests(), &common::limits()).await;
        assert_eq!(state(&f, &claim.id), "delivered");
        observed(
            Trace::new("accepted recovery close", variant_label, None),
            c.close(),
        )
        .await
        .unwrap();
        f.store.shutdown().await.unwrap();
        fake.close().await;
    }
}
#[tokio::test]
async fn native_matrix_outgoing_recovery_lost_http_and_begin_do_not_replay() {
    for lost_begin in [false, true] {
        let variant_label = Some(if lost_begin {
            "lost begin"
        } else {
            "lost HTTP"
        });
        let (f, mut fake, c) =
            ready_named(false, false, "lost write recovery bootstrap", variant_label).await;
        let claim = final_claim(&f).await;
        let cancel = CancellationToken::new();
        if lost_begin {
            c.inner.outgoing_fault.store(4, Ordering::SeqCst);
        }
        let (result, ()) = scripted(
            "lost write recovery send",
            variant_label,
            c.send_final(claim.clone(), &cancel),
            async {
                if lost_begin {
                    preflight(&mut fake, false).await;
                } else {
                    drop(plain_wire(&mut fake).await);
                }
            },
        )
        .await;
        assert!(result.is_err());
        assert_eq!(state(&f, &claim.id), "sending");
        stop_sdk(c).await;
        let c = Collector::new(
            config(&f, &fake.endpoint, f.identity.clone(), false),
            f.store.clone(),
        )
        .unwrap();
        let transaction = transaction_id(&f, &claim.id);
        if lost_begin {
            // ADR-059: a lost begin result is inspect-only. No write was ever
            // authorized — the journal is still BeforeBegin — so a resume parks
            // it for a human and starts no HTTP write. #9 does not change this:
            // there is no failed *send* to re-send.
            assert_eq!(
                observed(
                    Trace::new("lost write recovery resume", variant_label, None),
                    c.resume_outgoing_custody(&cancel)
                )
                .await
                .unwrap()
                .state,
                OutgoingState::Uncertain
            );
            fake.quiesced(fake.requests(), &common::limits()).await;
            assert_eq!(
                c.send_final(claim, &cancel).await,
                Err(Error::OutcomeUnknown)
            );
            fake.quiesced(fake.requests(), &common::limits()).await;
        } else {
            // Task #9 (TS `pollOneRouterOutbox`, bridge-matrix.js:6036-6055): a
            // lost write leaves the row `sending` (journal WritePossible), which
            // is non-permanent. The resume re-sends it with the SAME journaled
            // transaction id — Matrix dedups the replay — and settles it
            // delivered, exactly as TS retries the still-claimed command.
            let (summary, ()) = scripted(
                "lost write recovery resume",
                variant_label,
                c.resume_outgoing_custody(&cancel),
                async {
                    let request = fake.next().await;
                    assert_eq!(request.method, "PUT");
                    assert!(
                        request.target.ends_with(&format!("/{transaction}")),
                        "the resumed write kept its journaled transaction id"
                    );
                    request.json(200, json!({"event_id": "$resent"}));
                },
            )
            .await;
            assert_eq!(summary.unwrap().state, OutgoingState::Delivered);
            assert_eq!(state(&f, &claim.id), "delivered");
            fake.quiesced(fake.requests(), &common::limits()).await;
            assert!(c.send_final(claim, &cancel).await.unwrap().replayed);
            fake.quiesced(fake.requests(), &common::limits()).await;
        }
        observed(
            Trace::new("lost write recovery close", variant_label, None),
            c.close(),
        )
        .await
        .unwrap();
        f.store.shutdown().await.unwrap();
        fake.close().await;
    }
}
#[tokio::test]
async fn native_matrix_outgoing_domain_accepted_after_cancellation_records_history() {
    let (f, mut fake, c) = ready(false, false).await;
    let claim = final_claim(&f).await;
    let cancel = CancellationToken::new();
    c.inner.outgoing_fault.store(3, Ordering::SeqCst);
    let (result, ()) = common::scripted(c.send_final(claim.clone(), &cancel), async {
        plain_wire(&mut fake)
            .await
            .json(200, json!({"event_id":"$late"}));
        c.inner.outgoing_reached.notified().await;
        f.store.cancel_final_reply(claim.id.clone()).await.unwrap();
        cancel.cancel();
        c.inner.outgoing_continue.notify_one();
    })
    .await;
    assert_eq!(result.unwrap().state, OutgoingState::Delivered);
    assert_eq!(state(&f, &claim.id), "delivered");
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}
async fn notice_claim(
    f: &common::Fixture,
    fake: &mut common::Fake,
    c: &Collector,
) -> (VerifiedNoticeClaim, String) {
    let cancel = CancellationToken::new();
    let event = json!({"event_id":"$question","sender":"@owner:example.test","type":"m.room.message","origin_server_ts":now(),"content":{"msgtype":"m.text","body":"Please implement","m.mentions":{"user_ids":["@worker:example.test"]}}});
    let sync = json!({"next_batch":"question","rooms":{"join":{"!project:example.test":{"timeline":{"limited":false,"events":[event]},"state":{"events":[]}}}},"to_device":{"events":[]}});
    let (r, ()) = scripted(
        "outgoing notice intake",
        None,
        c.intake(HostIntakePlan::new(vec!["root".into()]).unwrap(), &cancel),
        async {
            fake.next().await.json(200, common::who());
            fake.next().await.json(200, sync);
            fake.next().await.json(200, room(false));
        },
    )
    .await;
    r.unwrap();
    let scope = f.store.matrix_ingress_scope("root".into()).await.unwrap();
    let inbox = f.store.inbox("root".into(), 0, 10, None).await.unwrap();
    let intent = f
        .store
        .create_verified_task_intent(VerifiedTaskRequest {
            scope,
            request_key: "request".into(),
            source_sequence: inbox[0].message.sequence,
            definition: TaskDefinition {
                title: "Implement".into(),
                ..TaskDefinition::default()
            },
        })
        .await
        .unwrap();
    assert_eq!(intent.activation, "pending");
    let claim = f
        .store
        .claim_verified_task_notice(60_000)
        .await
        .unwrap()
        .unwrap();
    (claim, intent.task_id)
}
#[tokio::test]
async fn native_matrix_outgoing_plain_notice_activates_only_after_real_acceptance() {
    let (f, mut fake, c) = ready_named(false, false, "notice bootstrap", None).await;
    let cancel = CancellationToken::new();
    let (claim, task_id) = notice_claim(&f, &mut fake, &c).await;
    let (r, body) = scripted(
        "notice send",
        None,
        c.send_notice(claim.clone(), &cancel),
        async {
            let request = plain_wire(&mut fake).await;
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            request.json(200, json!({"event_id":"$notice"}));
            body
        },
    )
    .await;
    assert_eq!(r.unwrap().state, OutgoingState::Delivered);
    assert_eq!(body["msgtype"], "m.notice");
    assert_eq!(body["m.relates_to"]["event_id"], "$question");
    let row: (String, String) =
        rusqlite::Connection::open(f.root.path().join("domain/domain.sqlite3"))
            .unwrap()
            .query_row(
                "SELECT state,anchor_event_id FROM task_intents WHERE task_id=?1",
                [task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
    assert_eq!(row, ("active".into(), "$notice".into()));
    observed(Trace::new("notice close", None, None), c.close())
        .await
        .unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}

#[tokio::test]
async fn native_matrix_outgoing_crypto_real_verified_dm_and_group_ciphertext() {
    for direct in [true, false] {
        let (f, mut fake, c) = ready(true, direct).await;
        let claim = final_claim(&f).await;
        let peer = c
            .inner
            .owner
            .lock()
            .await
            .as_ref()
            .unwrap()
            .outgoing_fixture(true)
            .await;
        let cancel = CancellationToken::new();
        let (r, plain) = common::scripted(c.send_final(claim, &cancel), async {
            preflight(&mut fake, true).await;
            let query = fake.next().await;
            assert_eq!(query.method, "POST");
            assert_eq!(query.target, "/_matrix/client/v3/keys/query");
            query.json(200, peer.query.clone());
            preflight(&mut fake, true).await;
            fake.next().await.json(200, peer.query.clone());
            let share = fake.next().await;
            assert_eq!(share.method, "PUT");
            assert!(share.target.contains("/sendToDevice/m.room.encrypted/"));
            peer.share(serde_json::from_slice(&share.body).unwrap())
                .await;
            share.json(200, json!({}));
            preflight(&mut fake, true).await;
            fake.next().await.json(200, peer.query.clone());
            let message = fake.next().await;
            assert_eq!(message.method, "PUT");
            assert!(message.target.contains("/send/m.room.encrypted/"));
            let value: Value = serde_json::from_slice(&message.body).unwrap();
            assert!(value.get("body").is_none());
            let plain = peer.decrypt(value).await;
            message.json(200, json!({"event_id":"$encrypted"}));
            plain
        })
        .await;
        assert_eq!(r.unwrap().state, OutgoingState::Delivered);
        assert_eq!(plain["content"]["body"], "Answer **verified** 中文");
        assert!(
            plain["content"]["formatted_body"]
                .as_str()
                .unwrap()
                .contains("<strong>verified</strong>")
        );
        c.close().await.unwrap();
        f.store.shutdown().await.unwrap();
        fake.close().await;
    }
}
/// ADR-185: an agent's own reply is shared with its owner's device even when
/// the owner's identity never signed it (Rinx, 2026-10-01: the replies were
/// withheld and unreadable). The owner decrypts the actual ciphertext.
#[tokio::test]
async fn native_matrix_outgoing_crypto_reaches_an_unverified_owner_device() {
    let (f, mut fake, c) = ready(true, true).await;
    let claim = final_claim(&f).await;
    let peer = c
        .inner
        .owner
        .lock()
        .await
        .as_ref()
        .unwrap()
        .outgoing_fixture(true)
        .await;
    // The owner's identity stays anchored; only this device is not signed by
    // it: keep the device's own signature, drop the identity's.
    let mut keys = peer.query.clone();
    keys["device_keys"]["@owner:example.test"]["HUMAN"]["signatures"]["@owner:example.test"]
        .as_object_mut()
        .unwrap()
        .retain(|id, _| id == "ed25519:HUMAN");
    let cancel = CancellationToken::new();
    let (r, plain) = common::scripted(c.send_final(claim, &cancel), async {
        preflight(&mut fake, true).await;
        fake.next().await.json(200, keys.clone());
        preflight(&mut fake, true).await;
        fake.next().await.json(200, keys.clone());
        let share = fake.next().await;
        assert!(
            share.target.contains("/sendToDevice/m.room.encrypted/"),
            "the room key is shared, not withheld: {}",
            share.target
        );
        peer.share(serde_json::from_slice(&share.body).unwrap())
            .await;
        share.json(200, json!({}));
        preflight(&mut fake, true).await;
        fake.next().await.json(200, keys.clone());
        let message = fake.next().await;
        assert!(message.target.contains("/send/m.room.encrypted/"));
        let value: Value = serde_json::from_slice(&message.body).unwrap();
        let plain = peer.decrypt(value).await;
        message.json(200, json!({"event_id":"$encrypted"}));
        plain
    })
    .await;
    assert_eq!(r.unwrap().state, OutgoingState::Delivered);
    assert_eq!(plain["content"]["body"], "Answer **verified** 中文");
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}
#[tokio::test]
async fn native_matrix_outgoing_crypto_unverified_missing_or_changed_keys_never_fall_back() {
    // "unverified" left this list (ADR-185): an agent's message reaches its
    // owner's unsigned device; asserted positively in
    // `native_matrix_outgoing_crypto_reaches_an_unverified_owner_device`.
    for variant in [
        "own_missing",
        "changed_before_share",
        "malformed_master",
        "malformed_signing",
        "bad_device_signature",
    ] {
        let (f, mut fake, c) =
            ready_named(true, true, "crypto refusal bootstrap", Some(variant)).await;
        let claim = final_claim(&f).await;
        let peer = c
            .inner
            .owner
            .lock()
            .await
            .as_ref()
            .unwrap()
            .outgoing_fixture(variant != "unverified")
            .await;
        let cancel = CancellationToken::new();
        let (r, ()) = scripted(
            "crypto refusal send",
            Some(variant),
            c.send_final(claim, &cancel),
            async {
                preflight(&mut fake, true).await;
                let mut keys = peer.query.clone();
                if variant == "own_missing" {
                    keys["device_keys"]["@worker:example.test"]
                        .as_object_mut()
                        .unwrap()
                        .clear();
                }
                if variant == "malformed_master" {
                    keys["master_keys"]["@owner:example.test"] = json!({});
                }
                if variant == "malformed_signing" {
                    keys["self_signing_keys"]["@owner:example.test"] = json!({});
                }
                if variant == "bad_device_signature" {
                    keys["device_keys"]["@owner:example.test"]["HUMAN"]["signatures"] = json!({});
                }
                fake.next().await.json(200, keys);
                if variant == "changed_before_share" {
                    preflight(&mut fake, true).await;
                    let mut changed = peer.query.clone();
                    changed["device_keys"]["@owner:example.test"]
                        .as_object_mut()
                        .unwrap()
                        .clear();
                    fake.next().await.json(200, changed);
                }
            },
        )
        .await;
        assert!(matches!(r, Err(Error::Recipients | Error::Identity)));
        fake.quiesced(fake.requests(), &common::limits()).await;
        assert_eq!(
            observed(
                Trace::new("crypto refusal resume", Some(variant), None),
                c.resume_outgoing_custody(&cancel)
            )
            .await
            .unwrap()
            .state,
            OutgoingState::Uncertain
        );
        observed(
            Trace::new("crypto refusal close", Some(variant), None),
            c.close(),
        )
        .await
        .unwrap();
        common::shutdown_domain(&f.store, "outgoing crypto refusal cleanup").await;
        fake.close().await;
    }
}

#[tokio::test]
async fn native_matrix_outgoing_scope_unsafe_private_snapshot_stops_before_begin() {
    let (f, mut fake, c) = ready(true, true).await;
    let claim = final_claim(&f).await;
    let cancel = CancellationToken::new();
    let (r,())=common::scripted(c.send_final(claim.clone(),&cancel),async {
        fake.next().await.json(200,common::who());
        let mut unsafe_room=room(true);unsafe_room.as_array_mut().unwrap().push(json!({"type":"m.room.member","state_key":"@other:example.test","content":{"membership":"join"}}));
        fake.next().await.json(200,unsafe_room);
    }).await;
    assert!(r.is_err());
    fake.quiesced(fake.requests(), &common::limits()).await;
    assert!(
        !f.store
            .matrix_room_state(
                f.identity.transport.engagement_id.clone(),
                "!project:example.test".into()
            )
            .await
            .unwrap()
            .unwrap()
            .available
    );
    assert_ne!(state(&f, &claim.id), "sending");
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}
#[tokio::test]
async fn native_matrix_outgoing_scope_wrong_claim_secret_cannot_send() {
    let (f, mut fake, c) = ready(false, false).await;
    let mut claim = final_claim(&f).await;
    let cancel = CancellationToken::new();
    claim.secret = "wrong".into();
    assert!(c.send_final(claim, &cancel).await.is_err());
    fake.quiesced(fake.requests(), &common::limits()).await;
    assert!(f.available().await);
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}
#[tokio::test]
async fn native_matrix_outgoing_bounds_wire_failures_retain_possible_writes() {
    for kind in [
        "duplicate",
        "coalesced",
        "oversize",
        "slow_headers",
        "slow_body",
        "redirect",
        "forbidden",
        "server_error",
    ] {
        let (f, mut fake, c) =
            ready_named(false, false, "wire refusal bootstrap", Some(kind)).await;
        let claim = final_claim(&f).await;
        let cancel = CancellationToken::new();
        let (r,())=scripted("wire refusal send", Some(kind),c.send_final(claim.clone(),&cancel),async {
            let req=plain_wire(&mut fake).await;
            match kind {
                "duplicate"=>req.raw(common::response(200,b"{\"event_id\":\"$one\",\"event_id\":\"$two\"}")),
                "coalesced"=>req.raw(common::response(200,b"{\"event_id\":\"$one\"} {}")),
                "oversize"=>req.raw(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1048577\r\nConnection: close\r\n\r\n".to_vec()),
                "slow_headers"=>req.chunks(vec![(Duration::from_millis(700),common::response(200,b"{\"event_id\":\"$one\"}"))]),
                "slow_body"=>req.chunks(vec![(Duration::ZERO,b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 19\r\nConnection: close\r\n\r\n".to_vec()),(Duration::from_millis(400),b"{\"event_id\":\"$one\"}".to_vec())]),
                "redirect"=>req.raw(b"HTTP/1.1 307 Temporary Redirect\r\nLocation: https://invalid.example/secret\r\nContent-Length: 0\r\n\r\n".to_vec()),
                "forbidden"=>{req.json(403,json!({"errcode":"M_FORBIDDEN"}));
                    // Board #11: a send refused on membership now re-joins once
                    // before giving up — the refusal environment persists here
                    // (the join itself is forbidden), so the same terminal
                    // Unauthorized stands and the write stays retained.
                    let join=fake.next().await;
                    assert_eq!(join.method,"POST");
                    assert!(join.target.contains("/_matrix/client/v3/join/"));
                    join.json(403,json!({"errcode":"M_FORBIDDEN"}));
                },
                _=>req.json(500,json!({"errcode":"M_UNKNOWN"})),
            }
        }).await;
        assert!(r.is_err(), "{kind}");
        assert_eq!(state(&f, &claim.id), "sending");
        let transaction = transaction_id(&f, &claim.id);
        // Task #9 (TS `isPermanentRouterMatrixFailure`, bridge-matrix.js:6029-6033):
        // only an HTTP 4xx other than 429 (here the M_FORBIDDEN 403) is permanent.
        // Every other failure here — a body the client could not accept, a timeout,
        // a redirect or a 5xx — is non-permanent.
        let permanent = kind == "forbidden";
        if permanent {
            // TS posts a permanent rejection to `../failed` and stops retrying:
            // the verdict surfaced to the caller above, and the journal carries
            // the mark. Every later resume reads it and parks the send for a
            // human — no HTTP write at all, so the refused send is never
            // replayed, and the replay is refused again and again.
            for _ in 0..2 {
                assert_eq!(
                    observed(
                        Trace::new("wire refusal parked resume", Some(kind), None),
                        c.resume_outgoing_custody(&cancel),
                    )
                    .await
                    .unwrap()
                    .state,
                    OutgoingState::Uncertain
                );
            }
            assert_eq!(state(&f, &claim.id), "sending");
            fake.quiesced(fake.requests(), &common::limits()).await;
        } else {
            // TS `pollOneRouterOutbox` re-sends the still-claimed command on a
            // later poll with the SAME transaction id; Matrix dedups the replay.
            let (summary, ()) = scripted(
                "wire refusal resume",
                Some(kind),
                c.resume_outgoing_custody(&cancel),
                async {
                    let request = fake.next().await;
                    assert_eq!(request.method, "PUT");
                    assert!(
                        request.target.ends_with(&format!("/{transaction}")),
                        "{kind}: the resend kept its journaled transaction id"
                    );
                    request.json(200, json!({"event_id":"$resent"}));
                },
            )
            .await;
            assert_eq!(summary.unwrap().state, OutgoingState::Delivered);
            assert_eq!(state(&f, &claim.id), "delivered");
            fake.quiesced(fake.requests(), &common::limits()).await;
        }
        observed(
            Trace::new("wire refusal close", Some(kind), None),
            c.close(),
        )
        .await
        .unwrap();
        common::shutdown_domain(&f.store, "outgoing wire refusal cleanup").await;
        fake.close().await;
    }
}
#[tokio::test]
async fn native_matrix_outgoing_recovery_actual_journal_rollback_does_not_publish_memory_only_acceptance()
 {
    let (f, mut fake, c) = ready(false, false).await;
    let claim = final_claim(&f).await;
    let cancel = CancellationToken::new();
    let sql =
        rusqlite::Connection::open(f.root.path().join("sdk/matrix-sdk-state.sqlite3")).unwrap();
    let (r,())=common::scripted(c.send_final(claim.clone(),&cancel),async {
        let req=plain_wire(&mut fake).await;
        sql.execute_batch("CREATE TRIGGER reject_outgoing_journal BEFORE INSERT ON kv_blob BEGIN SELECT RAISE(ABORT,'fixture rollback'); END;").unwrap();
        req.json(200,json!({"event_id":"$accepted_but_storage_failed"}));
    }).await;
    assert_eq!(r, Err(Error::OutcomeUnknown));
    assert_eq!(state(&f, &claim.id), "sending");
    assert_eq!(
        c.resume_outgoing_custody(&cancel).await,
        Err(Error::OutcomeUnknown)
    );
    sql.execute_batch("DROP TRIGGER reject_outgoing_journal")
        .unwrap();
    drop(sql);
    stop_sdk(c).await;
    let c = Collector::new(
        config(&f, &fake.endpoint, f.identity.clone(), false),
        f.store.clone(),
    )
    .unwrap();
    // The 200 acceptance existed only in memory: its journal insert rolled back,
    // so nothing durable claims delivery. Task #9: the durable phase is
    // WritePossible and the row is still `sending`, so this is a non-permanent
    // lost write — the resume re-sends it with the SAME journaled transaction id
    // and settles only on a real accepted response. It never publishes the
    // memory-only acceptance, which is what this test guards.
    let transaction = transaction_id(&f, &claim.id);
    let (summary, ()) = scripted(
        "rollback recovery resume",
        None,
        c.resume_outgoing_custody(&cancel),
        async {
            let request = fake.next().await;
            assert_eq!(request.method, "PUT");
            assert!(
                request.target.ends_with(&format!("/{transaction}")),
                "the resumed write kept its journaled transaction id"
            );
            request.json(200, json!({"event_id": "$resent"}));
        },
    )
    .await;
    assert_eq!(summary.unwrap().state, OutgoingState::Delivered);
    assert_eq!(state(&f, &claim.id), "delivered");
    fake.quiesced(fake.requests(), &common::limits()).await;
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}

#[tokio::test]
async fn native_matrix_outgoing_scope_retirement_after_possible_persist_prevents_http() {
    for retire in [false, true] {
        let (f, mut fake, c) = ready(false, false).await;
        let claim = final_claim(&f).await;
        let cancel = CancellationToken::new();
        c.inner.outgoing_fault.store(5, Ordering::SeqCst);
        let (r, ()) = common::scripted(c.send_final(claim.clone(), &cancel), async {
            preflight(&mut fake, false).await;
            preflight(&mut fake, false).await;
            c.inner.outgoing_reached.notified().await;
            assert_eq!(c.resume_outgoing_custody(&cancel).await, Err(Error::Busy));
            if retire {
                f.store
                    .invalidate_matrix_transport(MatrixTransportInvalidation {
                        expected: f.identity.transport.clone(),
                        reason: "controlled retirement".into(),
                    })
                    .await
                    .unwrap();
            } else {
                f.store.cancel_final_reply(claim.id.clone()).await.unwrap();
            }
            c.inner.outgoing_continue.notify_one();
        })
        .await;
        assert!(r.is_err());
        fake.quiesced(fake.requests(), &common::limits()).await;
        let resumed = c.resume_outgoing_custody(&cancel).await.unwrap();
        if retire {
            // A retired transport is not a host cancellation of this reply:
            // the send stays parked for a human.
            assert_eq!(resumed.state, OutgoingState::Uncertain);
        } else {
            // The host cancelled the reply itself: the journal slot is
            // released without any HTTP write and without recording delivery,
            // so the agent's next send is not blocked behind it forever.
            assert_eq!(resumed.state, OutgoingState::Idle);
            assert_ne!(state(&f, &claim.id), "delivered");
            assert_eq!(
                c.resume_outgoing_custody(&cancel).await.unwrap().state,
                OutgoingState::Idle,
                "the released send is never resumed again"
            );
        }
        fake.quiesced(fake.requests(), &common::limits()).await;
        c.close().await.unwrap();
        f.store.shutdown().await.unwrap();
        fake.close().await;
    }
}
#[tokio::test]
async fn native_matrix_outgoing_scope_failed_whoami_retires_cached_route() {
    let (f, mut fake, c) = ready(false, false).await;
    let claim = final_claim(&f).await;
    let cancel = CancellationToken::new();
    let (r, ()) = common::scripted(c.send_final(claim, &cancel), async {
        fake.next().await.json(
            200,
            json!({"user_id":"@other:example.test","device_id":"OTHER","is_guest":false}),
        );
    })
    .await;
    assert_eq!(r, Err(Error::Identity));
    assert!(!f.available().await);
    fake.quiesced(fake.requests(), &common::limits()).await;
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}

#[tokio::test]
async fn native_matrix_outgoing_history_rollover() {
    let (f, mut fake, c) = ready(false, false).await;
    let cancel = CancellationToken::new();
    let mut first = None;
    for i in 0..150 {
        let claim = final_claim_named(&f, &format!("task{i}")).await;
        if first.is_none() {
            first = Some(claim.clone());
        }
        let (r, ()) = common::scripted(c.send_final(claim, &cancel), async {
            plain_wire(&mut fake)
                .await
                .json(200, json!({"event_id":format!("$accepted{i}")}));
        })
        .await;
        assert_eq!(r.unwrap().state, OutgoingState::Delivered);
        let guard = c.inner.owner.lock().await;
        let view = guard
            .as_ref()
            .unwrap()
            .outgoing(Command::Read)
            .await
            .unwrap();
        assert_eq!(view.receipts.len(), (i + 1).min(state::MAX_RECEIPTS));
        assert!(view.attempt.is_none());
    }
    let first = first.unwrap();
    fake.quiesced(fake.requests(), &common::limits()).await;
    assert!(c.send_final(first.clone(), &cancel).await.unwrap().replayed);
    stop_sdk(c).await;
    let c = Collector::new(
        config(&f, &fake.endpoint, f.identity.clone(), false),
        f.store.clone(),
    )
    .unwrap();
    assert_eq!(
        c.resume_outgoing_custody(&cancel).await.unwrap().state,
        OutgoingState::Idle
    );
    assert!(c.send_final(first.clone(), &cancel).await.unwrap().replayed);
    fake.quiesced(fake.requests(), &common::limits()).await;
    let next = final_claim_named(&f, "after_history_restart").await;
    let (result, ()) = common::scripted(c.send_final(next, &cancel), async {
        plain_wire(&mut fake)
            .await
            .json(200, json!({"event_id":"$after_history_restart"}));
    })
    .await;
    assert_eq!(result.unwrap().state, OutgoingState::Delivered);
    let guard = c.inner.owner.lock().await;
    let owner = guard.as_ref().unwrap();
    let view = owner.outgoing(Command::Read).await.unwrap();
    assert_eq!(view.receipts.len(), state::MAX_RECEIPTS);
    assert!(!view.receipts.iter().any(|receipt| receipt.id == first.id));
    let exact = owner
        .outgoing(Command::Lookup {
            id: first.id.clone(),
            fence: first.fence,
        })
        .await
        .unwrap();
    assert_eq!(exact.receipts.len(), 1);
    assert!(matches!(exact.receipts[0].kind, Kind::Final));
    drop(guard);
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}

#[tokio::test]
async fn native_matrix_outgoing_history_negative() {
    use matrix_sdk_store_encryption::StoreCipher;
    let (f, mut fake, c) = ready(false, false).await;
    let cancel = CancellationToken::new();
    let mut first = None;
    for index in 0..state::MAX_RECEIPTS {
        let claim = final_claim_named(&f, &format!("history_negative_{index}")).await;
        if first.is_none() {
            first = Some(claim.clone());
        }
        let (result, ()) = common::scripted(c.send_final(claim, &cancel), async {
            plain_wire(&mut fake).await.json(
                200,
                json!({"event_id":format!("$negative_accepted_{index}")}),
            );
        })
        .await;
        result.unwrap();
    }
    let first = first.unwrap();
    let sql =
        rusqlite::Connection::open(f.root.path().join("sdk/matrix-sdk-state.sqlite3")).unwrap();
    let exported: Vec<u8> = sql
        .query_row("SELECT value FROM kv WHERE key='cipher'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let sdk_cipher = StoreCipher::import_with_key(&[42; 32], &exported).unwrap();
    let journal_key = sdk_cipher.hash_key("kv_blob", b"custom:hagency.observer.sync.v1");
    let encoded = journal_key
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    // Refuse only the root journal commit: immutable nodes may reach storage
    // first, but cannot evict live proof or authorize a new domain begin.
    sql.execute_batch(&format!("CREATE TRIGGER outgoing_history_root_abort BEFORE INSERT ON kv_blob WHEN NEW.key=X'{encoded}' BEGIN SELECT RAISE(ABORT,'fixture outgoing root rollback'); END;")).unwrap();
    let next = final_claim_named(&f, "history_root_refused").await;
    let (result, ()) = common::scripted(c.send_final(next.clone(), &cancel), async {
        preflight(&mut fake, false).await;
    })
    .await;
    assert_eq!(result, Err(Error::OutcomeUnknown));
    assert_eq!(state(&f, &next.id), "claimed");
    assert!(matches!(
        c.inner
            .owner
            .lock()
            .await
            .as_ref()
            .unwrap()
            .outgoing(Command::Read)
            .await,
        Err(Error::OutcomeUnknown)
    ));
    let cipher = StoreCipher::import_with_key(
        &[42; 32],
        &std::fs::read(f.root.path().join("sdk/journal.key")).unwrap(),
    )
    .unwrap();
    let bytes: Vec<u8> = sql
        .query_row(
            "SELECT value FROM kv_blob WHERE key=?1",
            [journal_key.as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    let journal: Value = cipher.decrypt_value(&bytes).unwrap();
    assert_eq!(
        journal["outgoing_receipts"].as_array().unwrap().len(),
        state::MAX_RECEIPTS
    );
    assert_eq!(journal["outgoing_receipts"][0]["id"], first.id);
    assert!(journal["sync_history"].is_null());
    assert!(journal["outgoing"].is_null());
    sql.execute_batch("DROP TRIGGER outgoing_history_root_abort")
        .unwrap();
    fake.quiesced(fake.requests(), &common::limits()).await;
    stop_sdk(c).await;
    let c = Collector::new(
        config(&f, &fake.endpoint, f.identity.clone(), false),
        f.store.clone(),
    )
    .unwrap();
    assert_eq!(
        c.resume_outgoing_custody(&cancel).await.unwrap().state,
        OutgoingState::Idle
    );
    assert!(c.send_final(first.clone(), &cancel).await.unwrap().replayed);
    // New independent send; never automatically retries the refused claim.
    let fresh = final_claim_named(&f, "history_independent_send").await;
    let (result, ()) = common::scripted(c.send_final(fresh, &cancel), async {
        plain_wire(&mut fake)
            .await
            .json(200, json!({"event_id":"$independent_history"}));
    })
    .await;
    result.unwrap();
    assert_eq!(state(&f, &next.id), "claimed");
    let bytes: Vec<u8> = sql
        .query_row(
            "SELECT value FROM kv_blob WHERE key=?1",
            [journal_key.as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    let journal: Value = cipher.decrypt_value(&bytes).unwrap();
    let root = journal["sync_history"].as_str().unwrap();
    let node_key = sdk_cipher.hash_key(
        "kv_blob",
        format!("custom:hagency.sync.history.v1.{root}").as_bytes(),
    );
    let node: Vec<u8> = sql
        .query_row(
            "SELECT value FROM kv_blob WHERE key=?1",
            [node_key.as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        sql.execute("DELETE FROM kv_blob WHERE key=?1", [node_key.as_slice()])
            .unwrap(),
        1
    );
    let bad = final_claim_named(&f, "history_missing_proof").await;
    assert_eq!(
        c.send_final(bad.clone(), &cancel).await,
        Err(Error::Storage)
    );
    assert_eq!(state(&f, &bad.id), "claimed");
    assert_eq!(
        c.send_final(first.clone(), &cancel).await,
        Err(Error::Storage)
    );
    fake.quiesced(fake.requests(), &common::limits()).await;
    sql.execute(
        "INSERT INTO kv_blob(key,value) VALUES(?1,?2)",
        rusqlite::params![node_key.as_slice(), node],
    )
    .unwrap();
    assert!(
        !f.store
            .final_reply_history_conflicts(first.id.clone(), first.fence)
            .await
            .unwrap()
    );
    assert!(
        f.store
            .final_reply_history_conflicts(first.id.clone(), first.fence + 1)
            .await
            .unwrap()
    );
    let mut domain =
        rusqlite::Connection::open(f.root.path().join("domain/domain.sqlite3")).unwrap();
    // Removed canonical data does not erase actual SDK acceptance. Rename
    // this fixture row and its references temporarily, preserving FK integrity.
    // This is a retention counterfactual, not a runtime pruning/cascade proof.
    let mut rename = |from: &str, to: &str| {
        let tx = domain.transaction().unwrap();
        tx.execute_batch("PRAGMA defer_foreign_keys=ON").unwrap();
        assert_eq!(
            tx.execute(
                "UPDATE final_replies SET id=?2 WHERE id=?1",
                rusqlite::params![from, to]
            )
            .unwrap(),
            1
        );
        for table in [
            "final_reply_calls",
            "final_reply_inspections",
            "owned_task_completions",
        ] {
            tx.execute(
                &format!("UPDATE {table} SET reply_id=?2 WHERE reply_id=?1"),
                rusqlite::params![from, to],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    };
    rename(&first.id, "fixture_retained_elsewhere");
    assert!(c.send_final(first.clone(), &cancel).await.unwrap().replayed);
    rename("fixture_retained_elsewhere", &first.id);
    assert!(
        domain
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    // Test-only re-admitted current-row counterfactual. SDK key equality is
    // not evidence that this newly claimed row has already been delivered.
    domain.execute("UPDATE final_replies SET state='pending',fence=0,claim_hash=NULL,claim_until=NULL WHERE id=?1", [&first.id]).unwrap();
    let new_claim = f.store.claim_final_reply(60_000).await.unwrap().unwrap();
    assert_eq!(new_claim.id, first.id);
    assert_eq!(new_claim.fence, first.fence);
    assert!(new_claim.secret != first.secret);
    assert_eq!(
        c.send_final(new_claim.clone(), &cancel).await,
        Err(Error::Conflict)
    );
    assert_eq!(state(&f, &new_claim.id), "claimed");
    fake.quiesced(fake.requests(), &common::limits()).await;
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}

async fn encrypted_wire(fake: &mut common::Fake, query: &Value) -> common::Request {
    preflight(fake, true).await;
    fake.next().await.json(200, query.clone());
    preflight(fake, true).await;
    fake.next().await.json(200, query.clone());
    let share = fake.next().await;
    assert!(share.target.contains("/sendToDevice/m.room.encrypted/"));
    share.json(200, json!({}));
    preflight(fake, true).await;
    fake.next().await.json(200, query.clone());
    let message = fake.next().await;
    assert!(message.target.contains("/send/m.room.encrypted/"));
    message
}
#[tokio::test]
async fn native_matrix_outgoing_recovery_restore_rejects_inconsistent_protected_history() {
    for variant in 0..9 {
        let variant_label = Some(
            [
                "history 0",
                "history 1",
                "history 2",
                "history 3",
                "history 4",
                "history 5",
                "history 6",
                "history 7",
                "history 8",
            ][usize::from(variant)],
        );
        let (f, mut fake, c) =
            ready_named_under_load(true, false, "protected history bootstrap", variant_label).await;
        let claim = final_claim(&f).await;
        let peer = c
            .inner
            .owner
            .lock()
            .await
            .as_ref()
            .unwrap()
            .outgoing_fixture(true)
            .await;
        c.inner.outgoing_fault.store(1, Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let (r, ()) = scripted(
            "protected history send",
            variant_label,
            c.send_final(claim.clone(), &cancel),
            async {
                encrypted_wire(&mut fake, &peer.query)
                    .await
                    .json(200, json!({"event_id":"$accepted"}));
            },
        )
        .await;
        assert_eq!(r, Err(Error::Busy));
        c.inner
            .owner
            .lock()
            .await
            .as_ref()
            .unwrap()
            .corrupt_outgoing_fixture(variant)
            .await;
        stop_sdk(c).await;
        let c = Collector::new(
            config(&f, &fake.endpoint, f.identity.clone(), false),
            f.store.clone(),
        )
        .unwrap();
        assert_eq!(
            observed(
                Trace::new("protected history resume", variant_label, None),
                c.resume_outgoing_custody(&cancel)
            )
            .await,
            Err(Error::Storage),
            "variant {variant}"
        );
        assert_eq!(state(&f, &claim.id), "sending");
        fake.quiesced(fake.requests(), &common::limits()).await;
        observed(
            Trace::new("protected history close", variant_label, None),
            c.close(),
        )
        .await
        .unwrap();
        f.store.shutdown().await.unwrap();
        fake.close().await;
    }
}
#[tokio::test]
async fn native_matrix_outgoing_domain_late_notice_acceptance_never_activates_retired_task() {
    let (f, mut fake, c) = ready(false, false).await;
    let (claim, task_id) = notice_claim(&f, &mut fake, &c).await;
    let cancel = CancellationToken::new();
    c.inner.outgoing_fault.store(3, Ordering::SeqCst);
    let (r, ()) = common::scripted(c.send_notice(claim.clone(), &cancel), async {
        plain_wire(&mut fake)
            .await
            .json(200, json!({"event_id":"$late_notice"}));
        c.inner.outgoing_reached.notified().await;
        f.store
            .invalidate_matrix_transport(MatrixTransportInvalidation {
                expected: f.identity.transport.clone(),
                reason: "controlled retirement".into(),
            })
            .await
            .unwrap();
        cancel.cancel();
        c.inner.outgoing_continue.notify_one();
    })
    .await;
    assert_eq!(r.unwrap().state, OutgoingState::Delivered);
    let row: (String, Option<String>) =
        rusqlite::Connection::open(f.root.path().join("domain/domain.sqlite3"))
            .unwrap()
            .query_row(
                "SELECT state,anchor_event_id FROM task_intents WHERE task_id=?1",
                [task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
    assert_ne!(row.0, "active");
    assert!(row.1.is_none());
    assert!(!f.available().await);
    assert_eq!(
        f.store
            .verified_notice_receipt(claim.claim.notice.id)
            .await
            .unwrap()
            .state,
        "delivered"
    );
    fake.quiesced(fake.requests(), &common::limits()).await;
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}
#[tokio::test]
async fn native_matrix_outgoing_crypto_lost_key_share_response_never_sends_room_ciphertext() {
    let (f, mut fake, c) = ready(true, true).await;
    let claim = final_claim(&f).await;
    let peer = c
        .inner
        .owner
        .lock()
        .await
        .as_ref()
        .unwrap()
        .outgoing_fixture(true)
        .await;
    let cancel = CancellationToken::new();
    let (r, ()) = common::scripted(c.send_final(claim.clone(), &cancel), async {
        preflight(&mut fake, true).await;
        fake.next().await.json(200, peer.query.clone());
        preflight(&mut fake, true).await;
        fake.next().await.json(200, peer.query.clone());
        let share = fake.next().await;
        assert!(share.target.contains("/sendToDevice/m.room.encrypted/"));
        peer.share(serde_json::from_slice(&share.body).unwrap())
            .await;
        drop(share);
    })
    .await;
    assert!(r.is_err());
    assert_eq!(state(&f, &claim.id), "sending");
    stop_sdk(c).await;
    let c = Collector::new(
        config(&f, &fake.endpoint, f.identity.clone(), true),
        f.store.clone(),
    )
    .unwrap();
    assert_eq!(
        c.resume_outgoing_custody(&cancel).await.unwrap().state,
        OutgoingState::Uncertain
    );
    fake.quiesced(fake.requests(), &common::limits()).await;
    assert_eq!(
        c.send_final(claim, &cancel).await,
        Err(Error::OutcomeUnknown)
    );
    fake.quiesced(fake.requests(), &common::limits()).await;
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}

#[tokio::test]
async fn native_matrix_outgoing_recovery_sdk_accept_response_lost_still_settles_without_send() {
    let (f, mut fake, c) = ready(false, false).await;
    let claim = final_claim(&f).await;
    c.inner
        .owner
        .lock()
        .await
        .as_ref()
        .unwrap()
        .outgoing_reply_fault()
        .await;
    let cancel = CancellationToken::new();
    let (r, ()) = common::scripted(c.send_final(claim.clone(), &cancel), async {
        plain_wire(&mut fake)
            .await
            .json(200, json!({"event_id":"$accepted"}));
    })
    .await;
    assert_eq!(r, Err(Error::OutcomeUnknown));
    assert_eq!(state(&f, &claim.id), "sending");
    stop_sdk(c).await;
    let c = Collector::new(
        config(&f, &fake.endpoint, f.identity.clone(), false),
        f.store.clone(),
    )
    .unwrap();
    assert_eq!(
        c.resume_outgoing_custody(&cancel).await.unwrap().state,
        OutgoingState::Delivered
    );
    assert_eq!(state(&f, &claim.id), "delivered");
    fake.quiesced(fake.requests(), &common::limits()).await;
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}

#[tokio::test]
async fn native_matrix_outgoing_domain_changed_receipt_or_fence_cannot_settle() {
    let (f, mut fake, c) = ready(false, false).await;
    let claim = final_claim(&f).await;
    let cancel = CancellationToken::new();
    c.inner.outgoing_fault.store(1, Ordering::SeqCst);
    let (r, ()) = common::scripted(c.send_final(claim.clone(), &cancel), async {
        plain_wire(&mut fake)
            .await
            .json(200, json!({"event_id":"$accepted"}));
    })
    .await;
    assert_eq!(r, Err(Error::Busy));
    let actual = c
        .inner
        .owner
        .lock()
        .await
        .as_ref()
        .unwrap()
        .outgoing(Command::Read)
        .await
        .unwrap()
        .attempt
        .unwrap()
        .observation()
        .unwrap();
    assert!(
        f.store
            .reconcile_final_reply(
                claim.id.clone(),
                claim.fence + 1,
                ReplyReconciliation::Delivered(actual.clone())
            )
            .await
            .is_err()
    );
    assert_eq!(state(&f, &claim.id), "sending");
    let mut changed_route = actual.clone();
    changed_route.room_id = "!other:example.test".into();
    assert!(
        f.store
            .reconcile_final_reply(
                claim.id.clone(),
                claim.fence,
                ReplyReconciliation::Delivered(changed_route)
            )
            .await
            .is_err()
    );
    assert_eq!(state(&f, &claim.id), "sending");
    assert_eq!(
        c.resume_outgoing_custody(&cancel).await.unwrap().state,
        OutgoingState::Delivered
    );
    let mut changed_receipt = actual;
    changed_receipt.event_id = "$substitute".into();
    assert!(
        f.store
            .reconcile_final_reply(
                claim.id.clone(),
                claim.fence,
                ReplyReconciliation::Delivered(changed_receipt)
            )
            .await
            .is_err()
    );
    assert_eq!(state(&f, &claim.id), "delivered");
    assert!(f.available().await);
    fake.quiesced(fake.requests(), &common::limits()).await;
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}
/// Review r1 finding 3/8: a FIRST unsafe room snapshot must surface at the
/// matrix boundary as its own word carrying the digest-bound reason —
/// `Error::UnsafeSnapshot("unsafe snapshot <digest>")` — never the wire-format
/// word and never the bare domain catch-all. A fresh collector (no prior room
/// observation) collects a room whose Direct snapshot carries a third member;
/// the store's safety refusal crosses the `From` boundary with the reason
/// intact, and no scope row is created, so the room stays observable later.
#[tokio::test]
async fn native_matrix_outgoing_first_unsafe_snapshot_surfaces_the_safety_reason() {
    let f = common::Fixture::new();
    let mut fake = common::Fake::start(true).await;
    let c = Collector::new(
        config(&f, &fake.endpoint, f.identity.clone(), true),
        f.store.clone(),
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let (r, ()) = scripted(
        "outgoing first unsafe snapshot",
        None,
        c.collect(&cancel),
        async {
            fake.next().await.json(200, common::who());
            fake.next().await.json(200, common::sync("boot"));
            let mut unsafe_room = room(true);
            unsafe_room.as_array_mut().unwrap().push(json!({
                "type": "m.room.member",
                "state_key": "@other:example.test",
                "content": {"membership": "join"}
            }));
            fake.next().await.json(200, unsafe_room);
        },
    )
    .await;
    match r {
        Err(Error::UnsafeSnapshot(reason)) => assert!(
            reason.starts_with("unsafe snapshot "),
            "the refusal carries the digest-bound safety reason, got {reason:?}"
        ),
        other => panic!("the first unsafe snapshot must surface UnsafeSnapshot, got {other:?}"),
    }
    fake.quiesced(fake.requests(), &common::limits()).await;
    // The refused first observation created no scope row: the room remains
    // unobserved, free to be admitted safely at generation 1 later.
    assert!(
        f.store
            .matrix_room_state(
                f.identity.transport.engagement_id.clone(),
                "!project:example.test".into()
            )
            .await
            .unwrap()
            .is_none()
    );
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}

/// Task #61 row 1. A `!help` answer must actually reach the room. Review #54
/// found the builder (`outgoing.rs:292`) drafts `m.text` — exactly TS
/// (`lib/bot-commands.js:397-404`) — while the validator demanded `m.notice`,
/// so `sdk/outgoing.rs:49` refused EVERY command answer and nothing was ever
/// sent live. This drives the real send path end to end (submit -> claim ->
/// send_command_notice -> sdk start/validate -> the wire), not a wire shape.
#[tokio::test]
async fn native_matrix_outgoing_command_answer_is_delivered_live() {
    let (f, mut fake, c) = ready(false, false).await;
    let cancel = CancellationToken::new();
    f.store
        .submit_command_notice(CommandNoticeRequest {
            session_id: "root".into(),
            body: "=== Agent Bridge Bot Commands ===".into(),
            html: Some("<h3>Agent Bridge Bot Commands</h3>".into()),
            source_event_id: "$command".into(),
        })
        .await
        .unwrap();
    let claimed = f
        .store
        .claim_command_notice_for_session("root".into(), 60_000)
        .await
        .unwrap()
        .unwrap();
    let (result, body) = common::scripted(c.send_command_notice(claimed.clone(), &cancel), async {
        let req = plain_wire(&mut fake).await;
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        req.json(200, json!({"event_id":"$answered"}));
        body
    })
    .await;
    // The whole point: it is DELIVERED, not refused by the validator.
    assert_eq!(result.unwrap().state, OutgoingState::Delivered);
    // TS lib/bot-commands.js:398 — the answer is TEXT, and carries the
    // handler's own html verbatim (format + formatted_body, :399-402).
    assert_eq!(body["msgtype"], "m.text");
    assert_eq!(body["body"], "=== Agent Bridge Bot Commands ===");
    assert_eq!(body["format"], "org.matrix.custom.html");
    assert!(
        body["formatted_body"]
            .as_str()
            .unwrap()
            .contains("Agent Bridge Bot Commands")
    );
    fake.quiesced(fake.requests(), &common::limits()).await;
    c.close().await.unwrap();
    f.store.shutdown().await.unwrap();
    fake.close().await;
}

#[path = "two_agents.rs"]
mod two_agents;
