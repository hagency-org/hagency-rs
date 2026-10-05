#[path = "verified_ingress/attachments.rs"]
mod attachments;
mod common;
#[path = "verified_ingress/notice_custody.rs"]
mod notice_custody;
use common::*;
use hagency_core::{
    commands::CommandNoticeRequest, ingress::*, messages::*, replies::*, task_intents::*, tasks::*,
};
use hagency_store::{DomainRepository, EffectOutcome, Error, OutcomeAction, OutcomeResolution};
use serde_json::{Value, json};
use std::collections::BTreeSet;

struct Fixture {
    root: tempfile::TempDir,
    db: DomainRepository,
    agents: Vec<String>,
    room: MatrixRoomObservation,
}
impl Fixture {
    fn new(direct: bool) -> Self {
        Self::with_agents(if direct { &["a"] } else { &["a", "b"] }, direct)
    }
    /// The same fixture with an explicit agent roster. Board #97 needs THREE
    /// agents joined to one room: a shared room where several agents are
    /// present is exactly the shape that answered one `!help` once per agent.
    fn with_agents(names: &[&str], direct: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
        db.register(&registration()).unwrap();
        let pool = resource("pool", "seat", 1000);
        db.put_resource(&pool).unwrap();
        let mut agents = vec![];
        for name in names.iter().copied() {
            let proof = proof(&request(name, name, &pool, 100));
            let agent = db.admit(&proof, 1000).unwrap();
            db.approve(&format!("approve_{name}"), &proof, 1000)
                .unwrap();
            let effect = db.claim_effect().unwrap().unwrap();
            db.observe_effect(
                &effect.id,
                effect.fence,
                &EffectOutcome::Applied {
                    receipt: "fixture account".into(),
                },
            )
            .unwrap();
            db.observe_matrix_transport(
                &MatrixTransportObservation {
                    engagement_id: agent.id.clone(),
                    registration_generation: 1,
                    generation: 1,
                    sender_mxid: format!("@{name}:example.test"),
                    device_id: format!("DEVICE_{name}"),
                },
                1001,
            )
            .unwrap();
            agents.push(agent.id);
        }
        let mut joined: BTreeSet<String> = names
            .iter()
            .map(|name| format!("@{name}:example.test"))
            .collect();
        joined.insert("@owner:example.test".into());
        if !direct {
            joined.extend([
                "@other:example.test".into(),
                registration().representative_mxid,
                registration().approval_bot_mxid,
            ]);
        }
        let room = MatrixRoomObservation {
            engagement_id: agents[0].clone(),
            registration_generation: 1,
            transport_generation: 1,
            room_id: if direct {
                "!dm:example.test"
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
            joined,
            invite_only: true,
            encrypted: direct,
        };
        for (index, agent) in agents.iter().enumerate() {
            let mut observation = room.clone();
            observation.engagement_id = agent.clone();
            db.observe_matrix_room(&observation, 1002).unwrap();
            db.resolve_verified_matrix_session(
                &SessionBinding {
                    id: names[index].into(),
                    engagement_id: agent.clone(),
                    room_id: room.room_id.clone(),
                    thread_root: None,
                },
                1003,
            )
            .unwrap();
        }
        Self {
            root,
            db,
            agents,
            room,
        }
    }
    fn event(
        &self,
        session: &str,
        id: &str,
        thread: Option<&str>,
        mentions: &[&str],
        at: u64,
    ) -> MatrixEventObservation {
        MatrixEventObservation {
            scope: self.db.matrix_ingress_scope(session).unwrap(),
            event: InboundMessage {
                server_name: "example.test".into(),
                room_id: self.room.room_id.clone(),
                event_id: format!("${id}"),
                sender_mxid: "@owner:example.test".into(),
                thread_root: thread.map(str::to_owned),
                body: format!("Message {id}"),
                kind: "m.text".into(),
                origin_ts: at,
            },
            mentions: mentions.iter().map(|s| (*s).into()).collect(),
            encrypted: self.room.encrypted,
        }
    }
    fn intent(&self, session: &str, key: &str, sequence: u64) -> VerifiedTaskRequest {
        VerifiedTaskRequest {
            scope: self.db.matrix_ingress_scope(session).unwrap(),
            request_key: key.into(),
            source_sequence: sequence,
            definition: TaskDefinition {
                title: "Verify canonical work".into(),
                ..Default::default()
            },
        }
    }
    fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.root.path().join("state/domain.sqlite3")).unwrap()
    }
    fn activate(&mut self, at: u64) -> (VerifiedNoticeClaim, IntentResult) {
        let claim = self
            .db
            .claim_verified_task_notice(at, 1000)
            .unwrap()
            .unwrap();
        self.db
            .begin_verified_task_notice_send(&claim.claim.notice.id, &claim.claim.token, at)
            .unwrap();
        let task = self
            .db
            .deliver_verified_task_notice(
                &claim.claim.notice.id,
                &claim.claim.token,
                &notice_delivery(&claim),
                at + 1,
            )
            .unwrap();
        (claim, task)
    }
    fn start(
        &mut self,
        id: &str,
        task: &IntentResult,
        sequences: &[u64],
        at: u64,
    ) -> RunnerCapability {
        self.db
            .enqueue_inbox_dispatch(&dispatch(id, task), sequences)
            .unwrap();
        let cap = self
            .db
            .claim_dispatch("fixture_runner", at, 60000, 120000, 8)
            .unwrap()
            .unwrap();
        assert_eq!(cap.dispatch_id, id);
        self.db.start_dispatch(&cap, at + 1).unwrap();
        cap
    }
    fn done(&mut self, cap: &RunnerCapability, task: &IntentResult, at: u64) {
        self.db
            .mutate_task(
                cap,
                &task.task_id,
                "done",
                &TaskMutation::Transition {
                    status: TaskState::Done,
                    waiting_reason: None,
                    waiting_until: None,
                },
                at,
            )
            .unwrap();
    }
}
fn dispatch(id: &str, task: &IntentResult) -> DispatchInput {
    DispatchInput {
        id: id.into(),
        session_id: task.session_id.clone(),
        task_id: Some(task.task_id.clone()),
        resources: vec![],
        payload: json!({"instruction":"Handle the admitted task input"}),
    }
}
fn notice_delivery(claim: &VerifiedNoticeClaim) -> ReplyDeliveryObservation {
    ReplyDeliveryObservation {
        transaction_id: claim.claim.notice.transaction_id.clone(),
        digest: claim.digest.clone(),
        server_name: claim.route.server_name.clone(),
        room_id: claim.route.room_id.clone(),
        sender_mxid: claim.route.sender_mxid.clone(),
        device_id: claim.route.device_id.clone(),
        event_id: format!("$ack_{}", claim.claim.notice.id),
        encrypted: claim.route.encrypted,
    }
}
fn count(sql: &rusqlite::Connection, table: &str) -> u64 {
    sql.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
fn setup_task(f: &mut Fixture) -> (MatrixEventObservation, u64, IntentResult) {
    let event = f.event("a", "root", None, &["@a:example.test"], 1010);
    let source = f.db.admit_matrix_event(&event, 1011).unwrap();
    let input = f.intent("a", "request", source.sequence);
    let task = f.db.create_verified_task_intent(&input, 1012).unwrap();
    (event, source.sequence, task)
}

#[test]
fn native_verified_ingress_policy() {
    trait Ambiguous<A> {
        fn check() {}
    }
    impl<T: ?Sized> Ambiguous<()> for T {}
    impl<T: serde::de::DeserializeOwned> Ambiguous<u8> for T {}
    let _ = <MatrixEventObservation as Ambiguous<_>>::check;
    let _ = <VerifiedTaskRequest as Ambiguous<_>>::check;
    let _ = <MatrixIngressScope as Ambiguous<_>>::check;
    let mut f = Fixture::new(false);
    for (id, mentions, expected) in [
        ("background", vec![], false),
        ("text_mention", vec![], false),
        ("wrong_server", vec!["@a:other.test"], false),
        ("actual", vec!["@a:example.test"], true),
    ] {
        let mut event = f.event("a", id, None, &mentions, 1010);
        event.event.body = "@a:example.test arbitrary body text".into();
        assert_eq!(
            f.db.admit_matrix_event(&event, 1011).unwrap().wake,
            expected
        );
    }
    for (index, sender) in [
        "@a:example.test".to_owned(),
        "@b:example.test".into(),
        registration().representative_mxid,
        registration().approval_bot_mxid,
    ]
    .into_iter()
    .enumerate()
    {
        let mut event = f.event(
            "a",
            &format!("service{index}"),
            None,
            &["@a:example.test"],
            1012,
        );
        event.event.sender_mxid = sender;
        assert!(!f.db.admit_matrix_event(&event, 1013).unwrap().wake);
    }
    for field in [
        "session",
        "incarnation",
        "registration",
        "room_generation",
        "device",
        "sender",
        "room",
        "server",
        "future",
        "old",
    ] {
        let mut event = f.event("a", "bad", None, &["@a:example.test"], 1010);
        match field {
            "session" => event.scope.session_id = "unknown".into(),
            "incarnation" => event.scope.session_generation += 1,
            "registration" => event.scope.registration_generation += 1,
            "room_generation" => event.scope.room_generation += 1,
            "device" => event.scope.transport_generation += 1,
            "sender" => event.event.sender_mxid = "@absent:example.test".into(),
            "room" => event.event.room_id = "!other:example.test".into(),
            "server" => event.event.server_name = "other.test".into(),
            "future" => event.event.origin_ts = 9999,
            _ => event.event.origin_ts = 1,
        };
        assert!(f.db.admit_matrix_event(&event, 1011).is_err(), "{field}");
    }
    // Board #112: a THREAD ROOT that binds no thread session is NOT a refusal.
    // TS routes a thread follow-up to the task bound to its root, and when no
    // binding applies it is an ordinary, woken new message to the mentioned
    // agent (`backend-v2.js:2352-2366`) — the retained bridge never had a
    // "thread root must equal the session's" rule. Native refused it, so the
    // live follow-up vanished; it is now admitted through the room session,
    // carrying its own thread root so the answer lands IN the thread.
    let mut threaded = f.event("a", "threaded", None, &["@a:example.test"], 1010);
    threaded.event.thread_root = Some("$unknown".into());
    let receipt = f.db.admit_matrix_event(&threaded, 1011).unwrap();
    assert!(receipt.wake, "the thread follow-up addresses the agent");
    assert_eq!(
        f.db.inbox("a", 0, 10, None)
            .unwrap()
            .last()
            .map(|item| item.message.thread_root.clone()),
        Some(Some("$unknown".into())),
        "the admitted follow-up keeps the thread root it arrived with"
    );
    let source = f.event("a", "legacy", None, &[], 1010);
    assert!(
        f.db.ingest_message(
            &source.event,
            &[MessageTarget {
                session_id: "a".into(),
                wake: true
            }],
            1011
        )
        .is_err()
    );
    let mut dm = Fixture::new(true);
    let event = dm.event("a", "dm", None, &[], 1010);
    assert!(dm.db.admit_matrix_event(&event, 1011).unwrap().wake);
    let mut bad = dm.event("a", "unencrypted", None, &[], 1012);
    bad.encrypted = false;
    assert!(dm.db.admit_matrix_event(&bad, 1013).is_err());
}

/// TS:bridge-matrix.js:3310 admits `m.notice` in the same breath as `m.text`, so
/// a human notice is TEXT for every purpose — including waking the agent it
/// addresses. The port admitted it but never let it wake. A notice that
/// addresses nobody still does not wake, exactly as a text that addresses
/// nobody does: the notice's msgtype changes nothing about the mention rule.
#[test]
fn native_verified_ingress_human_notice_wakes_like_text() {
    let mut f = Fixture::new(false);
    for (id, kind, mentions, expected) in [
        (
            "notice_addressed",
            "m.notice",
            vec!["@a:example.test"],
            true,
        ),
        ("text_addressed", "m.text", vec!["@a:example.test"], true),
        ("notice_unaddressed", "m.notice", vec![], false),
        ("text_unaddressed", "m.text", vec![], false),
    ] {
        let mut event = f.event("a", id, None, &mentions, 1010);
        event.event.kind = kind.into();
        assert_eq!(
            f.db.admit_matrix_event(&event, 1011).unwrap().wake,
            expected,
            "{id}"
        );
    }
}

/// TS:bridge-matrix.js:3318-3393, the store's half of the top-level case. A
/// group session with no thread root answers at the room's top level; the reply
/// must still name the message it answers. That question is the dispatch's own
/// addressed input — read back from the frozen window, not guessed.
#[test]
fn native_verified_ingress_top_level_group_answer_names_the_question() {
    let mut f = Fixture::new(false);
    // Session "a" is the Group room observed with NO thread root, so its route is
    // top-level: whatever the answer carries, it cannot be a thread relation.
    let event = f.event("a", "root", None, &["@a:example.test"], 1010);
    let source = f.db.admit_matrix_event(&event, 1011).unwrap();
    f.db.create_canonical_task("t", "a", "Answer the question", 1012)
        .unwrap();
    f.db.enqueue_inbox_dispatch(
        &DispatchInput {
            id: "d".into(),
            session_id: "a".into(),
            task_id: Some("t".into()),
            resources: vec![],
            payload: json!({"instruction":"Answer it"}),
        },
        &[source.sequence],
    )
    .unwrap();
    let cap =
        f.db.claim_dispatch("runner", 1013, 60_000, 120_000, 8)
            .unwrap()
            .unwrap();
    f.db.start_dispatch(&cap, 1014).unwrap();
    f.db.mutate_task(
        &cap,
        "t",
        "done",
        &TaskMutation::Transition {
            status: TaskState::Done,
            waiting_reason: None,
            waiting_until: None,
        },
        1015,
    )
    .unwrap();
    f.db.submit_final_reply(
        &cap,
        &FinalReply {
            call_id: "final".into(),
            body: "The answer".into(),
            incidental: false,
        },
        1016,
    )
    .unwrap();
    let send = f.db.claim_final_reply(1017, 1000).unwrap().unwrap();
    let output = f.db.begin_final_reply_send(&send, 1018).unwrap();
    assert_eq!(
        output.route.thread_root, None,
        "a group answer with no source thread stays at the room's top level"
    );
    assert_eq!(
        output.reply_to.as_deref(),
        Some("$root"),
        "it still names the question it answers"
    );
}

/// Board #112, the store's half of the exact live sequence: the question was
/// answered through the ROOM session (no task thread session is ever bound to
/// it), the task completed, and the owner then replies IN THE THREAD of that
/// question. TS routes a thread follow-up to the task bound to its root, and
/// when no binding applies it is an ordinary new message to the mentioned
/// agent, answered IN the thread (`backend-v2.js:2352-2366`); the relation is
/// built from the SOURCE message's `threadRootEventId`
/// (`bridge-matrix.js:3318-3393`), never from the session. So the answer must
/// carry the thread root even though the session is room-scoped.
#[test]
fn native_verified_ingress_threaded_followup_answer_stays_in_thread() {
    let mut f = Fixture::new(false);
    let mut follow = f.event("a", "followup", None, &["@a:example.test"], 1010);
    follow.event.thread_root = Some("$question".into());
    let source = f.db.admit_matrix_event(&follow, 1011).unwrap();
    assert!(source.wake, "the thread follow-up addresses the agent");
    f.db.create_canonical_task("t", "a", "Add 11", 1012)
        .unwrap();
    f.db.enqueue_inbox_dispatch(
        &DispatchInput {
            id: "d".into(),
            session_id: "a".into(),
            task_id: Some("t".into()),
            resources: vec![],
            payload: json!({"instruction":"Add 11"}),
        },
        &[source.sequence],
    )
    .unwrap();
    let cap =
        f.db.claim_dispatch("runner", 1013, 60_000, 120_000, 8)
            .unwrap()
            .unwrap();
    f.db.start_dispatch(&cap, 1014).unwrap();
    f.db.mutate_task(
        &cap,
        "t",
        "done",
        &TaskMutation::Transition {
            status: TaskState::Done,
            waiting_reason: None,
            waiting_until: None,
        },
        1015,
    )
    .unwrap();
    f.db.submit_final_reply(
        &cap,
        &FinalReply {
            call_id: "final".into(),
            body: "300".into(),
            incidental: false,
        },
        1016,
    )
    .unwrap();
    let send = f.db.claim_final_reply(1017, 1000).unwrap().unwrap();
    let output = f.db.begin_final_reply_send(&send, 1018).unwrap();
    assert_eq!(
        output.route.thread_root.as_deref(),
        Some("$question"),
        "the answer lands IN the thread the follow-up arrived in"
    );
    assert_eq!(
        output.reply_to.as_deref(),
        Some("$followup"),
        "it names the follow-up it answers"
    );
}

/// A `!` line is a bot command, never agent input: the retained bridge checked
/// `cmdBody.startsWith('!')` before routing (`bridge-matrix.js:7111-7125`), so a
/// command was dispatched and never became a prompt. The event stays admitted
/// (it is a fact in the room), and it wakes nobody — in a DM, where a bare line
/// used to wake the agent, and in a group, where a mention used to be enough.
#[test]
fn native_bot_command_lines_never_wake() {
    for direct in [false, true] {
        let mut f = Fixture::new(direct);
        let mentions: Vec<&str> = if direct {
            vec![]
        } else {
            vec!["@a:example.test"]
        };
        // The same shape that DOES wake, so the difference is the `!` alone.
        let ordinary = f.event("a", "ordinary", None, &mentions, 1010);
        assert!(f.db.admit_matrix_event(&ordinary, 1011).unwrap().wake);
        let mut command = f.event("a", "command", None, &mentions, 1012);
        command.event.body = "!help\n".into();
        let receipt = f.db.admit_matrix_event(&command, 1013).unwrap();
        // Admitted, recorded — and silent.
        assert!(receipt.created);
        assert!(!receipt.wake);
        // A leading space is still a command; TS trimmed before the check.
        let mut spaced = f.event("a", "spaced", None, &mentions, 1014);
        spaced.event.body = "   !status".into();
        assert!(!f.db.admit_matrix_event(&spaced, 1015).unwrap().wake);
        // An `!` that is not at the start is ordinary text and still wakes.
        let mut trailing = f.event("a", "trailing", None, &mentions, 1016);
        trailing.event.body = "please run !status".into();
        assert!(f.db.admit_matrix_event(&trailing, 1017).unwrap().wake);
        // A file is never a command, even when named like one (:7122).
        let mut file = f.event("a", "file", None, &mentions, 1018);
        file.event.body = "!help".into();
        file.event.kind = "m.file".into();
        assert!(f.db.admit_matrix_event(&file, 1019).unwrap().wake);
    }
}

#[test]
fn native_verified_ingress_task_activation() {
    for direct in [false, true] {
        let mut f = Fixture::new(direct);
        let event = f.event("a", "root", None, &["@a:example.test"], 1010);
        let sql = f.sql();
        sql.execute_batch("CREATE TRIGGER fail_projection BEFORE INSERT ON session_inputs BEGIN SELECT RAISE(ABORT,'fixture projection failure'); END;").unwrap();
        assert!(f.db.admit_matrix_event(&event, 1011).is_err());
        assert_eq!(count(&sql, "admitted_messages"), 0);
        assert_eq!(count(&sql, "matrix_ingress_events"), 0);
        sql.execute_batch("DROP TRIGGER fail_projection").unwrap();
        let source = f.db.admit_matrix_event(&event, 1011).unwrap();
        let input = f.intent("a", "create", source.sequence);
        let sessions = count(&sql, "runner_sessions");
        sql.execute_batch("CREATE TRIGGER fail_anchor BEFORE INSERT ON task_notices BEGIN SELECT RAISE(ABORT,'fixture anchor failure'); END;").unwrap();
        assert!(f.db.create_verified_task_intent(&input, 1012).is_err());
        assert_eq!(count(&sql, "canonical_tasks"), 0);
        assert_eq!(count(&sql, "task_intents"), 0);
        assert_eq!(count(&sql, "runner_sessions"), sessions);
        sql.execute_batch("DROP TRIGGER fail_anchor").unwrap();
        let task = f.db.create_verified_task_intent(&input, 1012).unwrap();
        assert_eq!(task.activation, "pending");
        assert!(
            f.db.create_verified_task_intent(&input, 1013)
                .unwrap()
                .replayed
        );
        let mut changed = input.clone();
        changed.definition.title = "Different".into();
        assert!(matches!(
            f.db.create_verified_task_intent(&changed, 1013),
            Err(Error::Conflict)
        ));
        assert!(
            f.db.enqueue_inbox_dispatch(&dispatch("early", &task), &[source.sequence])
                .is_err()
        );
        assert!(f.db.claim_task_notice(1014, 1000).unwrap().is_none());
        let claim =
            f.db.claim_verified_task_notice(1014, 1000)
                .unwrap()
                .unwrap();
        assert_eq!(claim.source_event_id, "$root");
        assert_eq!(
            claim.claim.notice.thread_root,
            if direct { None } else { Some("$root".into()) }
        );
        assert_eq!(claim.route.thread_root, claim.claim.notice.thread_root);
        f.db.begin_verified_task_notice_send(&claim.claim.notice.id, &claim.claim.token, 1015)
            .unwrap();
        for field in ["sender", "device", "digest", "room", "encryption"] {
            let mut bad = notice_delivery(&claim);
            match field {
                "sender" => bad.sender_mxid = "@other:example.test".into(),
                "device" => bad.device_id = "OTHER".into(),
                "digest" => bad.digest = "0".repeat(64),
                "room" => bad.room_id = "!other:example.test".into(),
                _ => {
                    if !direct {
                        continue;
                    }
                    bad.encrypted = false
                }
            };
            assert!(
                f.db.deliver_verified_task_notice(
                    &claim.claim.notice.id,
                    &claim.claim.token,
                    &bad,
                    1015
                )
                .is_err(),
                "{field}"
            );
        }
        let old = NoticeDelivery {
            server_name: claim.route.server_name.clone(),
            room_id: claim.route.room_id.clone(),
            transaction_id: claim.claim.notice.transaction_id.clone(),
            event_id: "$old_receipt".into(),
        };
        assert!(
            f.db.deliver_task_notice(&claim.claim.notice.id, &claim.claim.token, &old, 1015)
                .is_err()
        );
        sql.execute_batch("CREATE TRIGGER fail_activation BEFORE UPDATE ON task_intents WHEN NEW.state='active' BEGIN SELECT RAISE(ABORT,'fixture activation failure'); END;").unwrap();
        assert!(
            f.db.deliver_verified_task_notice(
                &claim.claim.notice.id,
                &claim.claim.token,
                &notice_delivery(&claim),
                1015
            )
            .is_err()
        );
        assert_eq!(
            sql.query_row(
                "SELECT state FROM task_notices WHERE id=?1",
                [&claim.claim.notice.id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "sending"
        );
        sql.execute_batch("DROP TRIGGER fail_activation").unwrap();
        f.db.deliver_verified_task_notice(
            &claim.claim.notice.id,
            &claim.claim.token,
            &notice_delivery(&claim),
            1016,
        )
        .unwrap();
        let cap = f.start("work", &task, &[source.sequence], 1017);
        f.done(&cap, &task, 1019);
        let reply =
            f.db.submit_final_reply(
                &cap,
                &FinalReply {
                    call_id: "final".into(),
                    body: "Actual intent completed".into(),
                    incidental: false,
                },
                1020,
            )
            .unwrap();
        assert_eq!(reply.execution_epoch, 1);
        let send = f.db.claim_final_reply(1021, 1000).unwrap().unwrap();
        let output = f.db.begin_final_reply_send(&send, 1022).unwrap();
        assert_eq!(
            output.route.thread_root,
            if direct { None } else { Some("$root".into()) }
        );
    }
}

#[test]
fn native_verified_ingress_followup() {
    for direct in [false, true] {
        let mut f = Fixture::new(direct);
        let (_, seq, task) = setup_task(&mut f);
        f.activate(1013);
        let cap = f.start("first", &task, &[seq], 1015);
        f.done(&cap, &task, 1017);
        let first =
            f.db.submit_final_reply(
                &cap,
                &FinalReply {
                    call_id: "first".into(),
                    body: "First answer".into(),
                    incidental: false,
                },
                1018,
            )
            .unwrap();
        f.db.complete_dispatch(&cap, &json!({"done":true}), 1019)
            .unwrap();
        let mut event = f.event(
            &task.session_id,
            "next",
            if direct { None } else { Some("$root") },
            if direct { &[] } else { &["@a:example.test"] },
            1020,
        );
        let next = f.db.admit_matrix_event(&event, 1021).unwrap();
        assert!(next.wake);
        let same =
            f.db.create_verified_task_intent(
                &f.intent(&task.session_id, "continue", next.sequence),
                1022,
            )
            .unwrap();
        assert_eq!(same.task_id, task.task_id);
        assert_eq!(count(&f.sql(), "canonical_tasks"), 1);
        f.db.enqueue_inbox_dispatch(&dispatch("next", &task), &[next.sequence])
            .unwrap();
        assert_eq!(
            f.db.canonical_task(&task.task_id).unwrap().status,
            TaskState::Done
        );
        let nextcap =
            f.db.claim_dispatch("next_runner", 1023, 60000, 120000, 8)
                .unwrap()
                .unwrap();
        assert_eq!(
            f.db.canonical_task(&task.task_id).unwrap().execution_epoch,
            1
        );
        f.db.start_dispatch(&nextcap, 1024).unwrap();
        assert_eq!(
            f.db.canonical_task(&task.task_id).unwrap().execution_epoch,
            2
        );
        assert!(
            f.db.submit_final_reply(
                &cap,
                &FinalReply {
                    call_id: "old".into(),
                    body: "Old epoch".into(),
                    incidental: false,
                },
                1025
            )
            .is_err()
        );
        assert!(f.db.claim_final_reply(1025, 1000).unwrap().is_none());
        assert_eq!(
            f.sql()
                .query_row(
                    "SELECT state FROM final_replies WHERE id=?1",
                    [first.id],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "cancelled"
        );
        f.done(&nextcap, &task, 1026);
        let reply =
            f.db.submit_final_reply(
                &nextcap,
                &FinalReply {
                    call_id: "second".into(),
                    body: "Second answer".into(),
                    incidental: false,
                },
                1027,
            )
            .unwrap();
        // Reopening and the later Done transition each advance canonical authority.
        assert_eq!(reply.execution_epoch, 3);
        f.db.complete_dispatch(&nextcap, &json!({"done":true}), 1028)
            .unwrap();
        event.event.event_id = "$stale".into();
        event.event.origin_ts = 1010;
        assert!(!f.db.admit_matrix_event(&event, 1030).unwrap().wake);
        if !direct {
            event.event.event_id = "$foreign".into();
            event.event.origin_ts = 1031;
            event.event.sender_mxid = "@other:example.test".into();
            assert!(!f.db.admit_matrix_event(&event, 1032).unwrap().wake);
            event.event.sender_mxid = "@owner:example.test".into();
            event.event.event_id = "$unmentioned".into();
            event.mentions.clear();
            assert!(!f.db.admit_matrix_event(&event, 1032).unwrap().wake);
        }
    }
}

#[test]
fn native_verified_ingress_copies() {
    let mut f = Fixture::new(false);
    let event = f.event(
        "a",
        "shared",
        None,
        &["@a:example.test", "@b:example.test"],
        1010,
    );
    let a = f.db.admit_matrix_event(&event, 1011).unwrap();
    let mut second = event.clone();
    second.scope = f.db.matrix_ingress_scope("b").unwrap();
    let b = f.db.admit_matrix_event(&second, 1012).unwrap();
    assert_eq!(a.sequence, b.sequence);
    let at =
        f.db.create_verified_task_intent(&f.intent("a", "a_task", a.sequence), 1013)
            .unwrap();
    let bt =
        f.db.create_verified_task_intent(&f.intent("b", "b_task", b.sequence), 1013)
            .unwrap();
    assert_ne!(at.task_id, bt.task_id);
    assert_ne!(at.session_id, bt.session_id);
    f.activate(1014);
    f.activate(1016);
    let cap = f.start("a_work", &at, &[a.sequence], 1018);
    let before = f.db.runner_inbox(&cap, 0, 100, 1020).unwrap();
    assert_eq!(before[0].message.body, "Message shared");
    // Copied verified input remains independent of the shared source projection.
    f.sql().execute("UPDATE admitted_messages SET config=json_set(config,'$.body','changed shared projection') WHERE sequence=?1",[a.sequence]).unwrap();
    assert_eq!(
        f.db.runner_inbox(&cap, 0, 100, 1021).unwrap()[0]
            .message
            .body,
        "Message shared"
    );
    assert_eq!(
        f.db.inbox(&bt.session_id, 0, 100, None).unwrap()[0]
            .message
            .body,
        "Message shared"
    );
    let later = f.event(&at.session_id, "later", Some("$shared"), &[], 1022);
    f.db.admit_matrix_event(&later, 1023).unwrap();
    assert_eq!(f.db.runner_inbox(&cap, 0, 100, 1024).unwrap().len(), 1);
    f.done(&cap, &at, 1025);
    f.db.complete_dispatch(&cap, &json!({"done":true}), 1026)
        .unwrap();
    assert_eq!(f.db.inbox(&bt.session_id, 0, 100, None).unwrap().len(), 1);
    assert_eq!(
        f.db.inbox(&at.session_id, 0, 100, None).unwrap()[0]
            .message
            .event_id,
        "$later"
    );
    let bcap = f.start("b_work", &bt, &[b.sequence], 1027);
    assert_eq!(
        f.db.runner_inbox(&bcap, 0, 100, 1029).unwrap()[0]
            .message
            .body,
        "Message shared"
    );
}

#[test]
fn native_verified_ingress_task_activation_existing_thread() {
    for direct in [false, true] {
        let mut f = Fixture::new(direct);
        let root = f.event("a", "root", None, &[], 1010);
        let source = f.db.admit_matrix_event(&root, 1011).unwrap();
        f.db.resolve_verified_matrix_session(
            &SessionBinding {
                id: "thread".into(),
                engagement_id: f.agents[0].clone(),
                room_id: f.room.room_id.clone(),
                thread_root: Some("$root".into()),
            },
            1012,
        )
        .unwrap();
        let mention = f.event(
            "thread",
            "mention",
            Some("$root"),
            &["@a:example.test"],
            1013,
        );
        let admitted = f.db.admit_matrix_event(&mention, 1014).unwrap();
        let task =
            f.db.create_verified_task_intent(
                &f.intent("thread", "thread_task", admitted.sequence),
                1015,
            )
            .unwrap();
        assert_eq!(task.session_id, "thread");
        let more = f.event(
            "thread",
            "before_ack",
            Some("$root"),
            &["@a:example.test"],
            1016,
        );
        let more = f.db.admit_matrix_event(&more, 1017).unwrap();
        assert!(
            f.db.enqueue_inbox_dispatch(
                &dispatch("early", &task),
                &[admitted.sequence, more.sequence]
            )
            .is_err()
        );
        let (claim, _) = f.activate(1018);
        assert_eq!(claim.source_event_id, "$root");
        assert_ne!(claim.source_event_id, notice_delivery(&claim).event_id);
        assert_eq!(claim.claim.notice.thread_root.as_deref(), Some("$root"));
        let cap = f.start(
            "thread_work",
            &task,
            &[source.sequence, admitted.sequence, more.sequence],
            1020,
        );
        let inbox = f.db.runner_inbox(&cap, 0, 100, 1022).unwrap();
        assert_eq!(
            inbox
                .iter()
                .map(|i| i.message.event_id.as_str())
                .collect::<Vec<_>>(),
            vec!["$root", "$mention", "$before_ack"]
        );
        assert!(!inbox[0].wake);
        assert!(inbox[1].wake && inbox[2].wake);
        f.sql()
            .execute(
                "UPDATE matrix_session_routes SET retired=1 WHERE session_id='a'",
                [],
            )
            .unwrap();
        assert!(f.db.matrix_ingress_scope("thread").is_err());
        assert!(f.db.runner_inbox(&cap, 0, 100, 1023).is_err());
        assert!(
            f.db.deliver_verified_task_notice(
                &claim.claim.notice.id,
                &claim.claim.token,
                &notice_delivery(&claim),
                1023
            )
            .is_err()
        );
    }
    let mut f = Fixture::new(false);
    f.db.resolve_verified_matrix_session(
        &SessionBinding {
            id: "unknown_thread".into(),
            engagement_id: f.agents[0].clone(),
            room_id: f.room.room_id.clone(),
            thread_root: Some("$missing".into()),
        },
        1010,
    )
    .unwrap();
    let event = f.event(
        "unknown_thread",
        "mention",
        Some("$missing"),
        &["@a:example.test"],
        1011,
    );
    let input = f.db.admit_matrix_event(&event, 1012).unwrap();
    assert!(
        f.db.create_verified_task_intent(&f.intent("unknown_thread", "bad", input.sequence), 1013)
            .is_err()
    );
    assert_eq!(count(&f.sql(), "canonical_tasks"), 0);
    // A stored reply cannot be substituted for the authenticated top-level root.
    f.db.resolve_verified_matrix_session(
        &SessionBinding {
            id: "nested".into(),
            engagement_id: f.agents[0].clone(),
            room_id: f.room.room_id.clone(),
            thread_root: Some("$mention".into()),
        },
        1014,
    )
    .unwrap();
    let nested = f.event(
        "nested",
        "nested_request",
        Some("$mention"),
        &["@a:example.test"],
        1015,
    );
    let nested = f.db.admit_matrix_event(&nested, 1016).unwrap();
    assert!(
        f.db.create_verified_task_intent(&f.intent("nested", "nested", nested.sequence), 1017)
            .is_err()
    );
}

#[test]
fn native_verified_ingress_fencing() {
    for change in [
        "promotion",
        "negative",
        "device",
        "allocation",
        "registration",
        "parent",
    ] {
        let direct = change != "parent";
        let mut f = Fixture::new(direct);
        let (event, seq, task) = setup_task(&mut f);
        let source_request = f.intent("a", "retry", seq);
        let claim =
            f.db.claim_verified_task_notice(1013, 1000)
                .unwrap()
                .unwrap();
        match change {
            "promotion" => {
                f.room.generation = 2;
                f.room.privacy = RoomPrivacy::Group {};
                f.room.joined.insert("@other:example.test".into());
                f.db.observe_matrix_room(&f.room, 1020).unwrap();
            }
            "negative" => {
                let mut bad = f.room.clone();
                bad.generation = 2;
                bad.joined.remove("@owner:example.test");
                f.db.observe_matrix_room(&bad, 1020).unwrap();
            }
            "device" => {
                f.db.observe_matrix_transport(
                    &MatrixTransportObservation {
                        engagement_id: f.agents[0].clone(),
                        registration_generation: 1,
                        generation: 2,
                        sender_mxid: "@a:example.test".into(),
                        device_id: "ROTATED".into(),
                    },
                    1020,
                )
                .unwrap();
            }
            "allocation" => {
                f.db.revoke("revoke", &f.agents[0]).unwrap();
            }
            "registration" => {
                let mut reg = registration();
                reg.generation = 2;
                f.db.register(&reg).unwrap();
            }
            _ => {
                f.sql()
                    .execute(
                        "UPDATE matrix_session_routes SET retired=1 WHERE session_id='a'",
                        [],
                    )
                    .unwrap();
            }
        }
        assert!(f.db.admit_matrix_event(&event, 1021).is_err(), "{change}");
        assert!(
            f.db.create_verified_task_intent(&source_request, 1021)
                .is_err()
        );
        assert!(
            f.db.deliver_verified_task_notice(
                &claim.claim.notice.id,
                &claim.claim.token,
                &notice_delivery(&claim),
                1021
            )
            .is_err()
        );
        assert!(
            f.db.enqueue_inbox_dispatch(&dispatch("old", &task), &[seq])
                .is_err()
        );
        assert!(
            f.db.claim_verified_task_notice(1022, 1000)
                .unwrap()
                .is_none()
        );
        if change == "promotion" || change == "parent" {
            f.db.resolve_verified_matrix_session(
                &SessionBinding {
                    id: "fresh".into(),
                    engagement_id: f.agents[0].clone(),
                    room_id: f.room.room_id.clone(),
                    thread_root: None,
                },
                1023,
            )
            .unwrap();
            let mut old = event.clone();
            old.scope = f.db.matrix_ingress_scope("fresh").unwrap();
            assert!(f.db.admit_matrix_event(&old, 1024).is_err());
            assert!(
                f.db.create_verified_task_intent(&f.intent("fresh", "copy_old", seq), 1024)
                    .is_err()
            );
            let fresh = f.event("fresh", "public_new", None, &["@a:example.test"], 1025);
            let fresh = f.db.admit_matrix_event(&fresh, 1026).unwrap();
            let next = f
                .db
                .create_verified_task_intent(&f.intent("fresh", "new_task", fresh.sequence), 1027)
                .unwrap();
            f.activate(1028);
            let cap = f.start("new", &next, &[fresh.sequence], 1030);
            assert_eq!(
                f.db.runner_inbox(&cap, 0, 100, 1032)
                    .unwrap()
                    .iter()
                    .map(|i| i.message.event_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["$public_new"]
            );
        }
    }
}

#[test]
fn native_verified_ingress_recovery() {
    let mut f = Fixture::new(true);
    let event = f.event("a", "delayed", None, &[], 1010);
    let first = f.db.admit_matrix_event(&event, 1050).unwrap();
    let request = f.intent("a", "task", first.sequence);
    let task = f.db.create_verified_task_intent(&request, 1051).unwrap();
    let old = f.db.claim_verified_task_notice(1052, 5).unwrap().unwrap();
    f.db.observe_matrix_room(&f.room, 2000).unwrap();
    f.db.observe_matrix_transport(
        &MatrixTransportObservation {
            engagement_id: f.agents[0].clone(),
            registration_generation: 1,
            generation: 1,
            sender_mxid: "@a:example.test".into(),
            device_id: "DEVICE_a".into(),
        },
        2000,
    )
    .unwrap();
    assert_eq!(
        f.sql()
            .query_row(
                "SELECT ingress_since FROM matrix_session_routes WHERE session_id='a'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        1002
    );
    drop(f.db);
    f.db = DomainRepository::open(&f.root.path().join("state")).unwrap();
    let replay = f.db.admit_matrix_event(&event, 2001).unwrap();
    assert_eq!(replay.sequence, first.sequence);
    assert!(!replay.created && !replay.projected);
    assert!(
        f.db.create_verified_task_intent(&request, 2002)
            .unwrap()
            .replayed
    );
    let offline = f.event("a", "offline", None, &[], 1011);
    let offline = f.db.admit_matrix_event(&offline, 2003).unwrap();
    assert!(offline.wake);
    let mut conflict = event.clone();
    conflict.event.body = "Changed body".into();
    assert!(matches!(
        f.db.admit_matrix_event(&conflict, 2004),
        Err(Error::Conflict)
    ));
    let mut conflict = event.clone();
    conflict.mentions.insert("@a:example.test".into());
    assert!(matches!(
        f.db.admit_matrix_event(&conflict, 2004),
        Err(Error::Conflict)
    ));
    assert!(
        f.db.deliver_verified_task_notice(
            &old.claim.notice.id,
            &old.claim.token,
            &notice_delivery(&old),
            2004
        )
        .is_err()
    );
    let next =
        f.db.claim_verified_task_notice(2004, 1000)
            .unwrap()
            .unwrap();
    assert_eq!(
        next.claim.notice.transaction_id,
        old.claim.notice.transaction_id
    );
    assert_ne!(next.claim.token, old.claim.token);
    f.db.begin_verified_task_notice_send(&next.claim.notice.id, &next.claim.token, 2005)
        .unwrap();
    f.db.deliver_verified_task_notice(
        &next.claim.notice.id,
        &next.claim.token,
        &notice_delivery(&next),
        2005,
    )
    .unwrap();
    assert!(
        f.db.deliver_verified_task_notice(
            &next.claim.notice.id,
            &next.claim.token,
            &notice_delivery(&next),
            2006
        )
        .unwrap()
        .replayed
    );
    let cap = f.start(
        "after_restart",
        &task,
        &[first.sequence, offline.sequence],
        2007,
    );
    assert_eq!(f.db.runner_inbox(&cap, 0, 100, 2009).unwrap().len(), 2);
    f.done(&cap, &task, 2010);
    let reply =
        f.db.submit_final_reply(
            &cap,
            &FinalReply {
                call_id: "result".into(),
                body: "Recovered activation".into(),
                incidental: false,
            },
            2011,
        )
        .unwrap();
    f.db.complete_dispatch(&cap, &json!({"done":true}), 2012)
        .unwrap();
    drop(f.db);
    f.db = DomainRepository::open(&f.root.path().join("state")).unwrap();
    assert!(f.db.inbox("a", 0, 100, None).unwrap().is_empty());
    assert!(f.db.claim_final_reply(2013, 1000).unwrap().is_some());
    assert_eq!(
        f.db.canonical_task(&task.task_id).unwrap().execution_epoch,
        reply.execution_epoch
    );
}

#[test]
fn native_verified_ingress_recovery_schema_eleven_has_unknown_boundary() {
    let mut f = Fixture::new(true);
    drop(f.db);
    let sql = rusqlite::Connection::open(f.root.path().join("state/domain.sqlite3")).unwrap();
    remove_ingress_schema(&sql);
    sql.execute_batch("ALTER TABLE runner_sessions DROP COLUMN model_override; ALTER TABLE runner_sessions DROP COLUMN mode_override; DROP TABLE IF EXISTS agent_lifecycle; ALTER TABLE decisions DROP COLUMN kind; ALTER TABLE decisions DROP COLUMN at; DROP TABLE IF EXISTS quota_holds; DROP TABLE IF EXISTS owner_anchors; DROP TABLE IF EXISTS joined_rooms; ALTER TABLE engagements DROP COLUMN allocated_tokens; DROP TABLE IF EXISTS palpo_agent_retirements; DROP TABLE IF EXISTS project_command_receipts; DROP TABLE IF EXISTS project_grant_decisions; DROP TABLE IF EXISTS project_grant_agents; DROP TABLE IF EXISTS project_grants; DROP TABLE IF EXISTS resource_delegations; PRAGMA user_version=11;").unwrap();
    drop(sql);
    f.db = DomainRepository::open(&f.root.path().join("state")).unwrap();
    assert!(f.db.matrix_ingress_scope("a").is_err());
    f.db.observe_matrix_room(&f.room, 2000).unwrap();
    assert!(f.db.matrix_ingress_scope("a").is_err());
    f.db.observe_matrix_transport(
        &MatrixTransportObservation {
            engagement_id: f.agents[0].clone(),
            registration_generation: 1,
            generation: 2,
            sender_mxid: "@a:example.test".into(),
            device_id: "NEW".into(),
        },
        2001,
    )
    .unwrap();
    f.room.transport_generation = 2;
    f.room.generation = 2;
    f.db.observe_matrix_room(&f.room, 2002).unwrap();
    f.db.resolve_verified_matrix_session(
        &SessionBinding {
            id: "fresh".into(),
            engagement_id: f.agents[0].clone(),
            room_id: f.room.room_id.clone(),
            thread_root: None,
        },
        2003,
    )
    .unwrap();
    let fresh = f.event("fresh", "fresh", None, &[], 2004);
    assert!(f.db.admit_matrix_event(&fresh, 2005).unwrap().wake);
    assert!(f.db.matrix_ingress_scope("a").is_err());
}

#[test]
fn native_matrix_intake_rotation_historical_receipt_is_content_bound_read_only() {
    let mut f = Fixture::new(false);
    let event = f.event("a", "receipt", None, &["@a:example.test"], 1004);
    assert!(f.db.matrix_ingress_receipt(&event).unwrap().is_none());
    let receipt = f.db.admit_matrix_event(&event, 1004).unwrap();
    let found = f.db.matrix_ingress_receipt(&event).unwrap().unwrap();
    assert_eq!(found.sequence, receipt.sequence);
    assert!(!found.created && !found.projected);
    let mut changed = event.clone();
    changed.event.body.push('!');
    assert!(matches!(
        f.db.matrix_ingress_receipt(&changed),
        Err(Error::Conflict)
    ));
    let mut foreign = event.clone();
    foreign.scope.transport_generation += 1;
    assert!(matches!(
        f.db.matrix_ingress_receipt(&foreign),
        Err(Error::RunnerAuthority)
    ));
    f.db.invalidate_matrix_transport(
        &MatrixTransportInvalidation {
            expected: MatrixTransportObservation {
                engagement_id: f.agents[0].clone(),
                registration_generation: 1,
                generation: 1,
                sender_mxid: "@a:example.test".into(),
                device_id: "DEVICE_a".into(),
            },
            reason: "Retired authenticated source".into(),
        },
        1005,
    )
    .unwrap();
    assert!(f.db.admit_matrix_event(&event, 1006).is_err());
    assert_eq!(
        f.db.matrix_ingress_receipt(&event)
            .unwrap()
            .unwrap()
            .sequence,
        receipt.sequence
    );
    let mut absent = event.clone();
    absent.event.event_id = "$absent".into();
    assert!(f.db.matrix_ingress_receipt(&absent).unwrap().is_none());
    let n: u64 = f
        .sql()
        .query_row("SELECT COUNT(*) FROM admitted_messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

/// The retained product says the launch retry in the thread
/// (`router/src/store.ts` `requeueBeforeStart`).
#[test]
fn native_runner_launch_retry_notice() {
    let mut f = Fixture::new(false);
    let (_, seq, task) = setup_task(&mut f);
    f.activate(1013);
    f.db.enqueue_inbox_dispatch(&dispatch("first", &task), &[seq])
        .unwrap();
    let cap =
        f.db.claim_dispatch("fixture_runner", 1015, 60000, 120000, 8)
            .unwrap()
            .unwrap();
    f.db.fail_before_start(&cap, 1016, 1000).unwrap();
    let (kind, body): (String, String) = f
        .sql()
        .query_row(
            "SELECT json_extract(config,'$.kind'),json_extract(config,'$.body') FROM task_notices WHERE json_extract(config,'$.kind')='runner_launch_retry'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "runner_launch_retry");
    assert_eq!(
        body,
        "Runner could not start, but no work was executed and no input was lost. The dispatch remains queued and will retry automatically."
    );
}

/// Task #61 row 2. The retained product tells the thread why a queued dispatch
/// did not start when its own task is blocked, and SKIPS it — only an operator
/// can resume (`router/src/store.ts:1556-1563`). Review #54 found both halves
/// missing: Rust claimed the dispatch and even started it, silently.
#[test]
fn native_blocked_task_dispatch_is_skipped_and_explained() {
    let mut f = Fixture::new(false);
    let (_, seq, task) = setup_task(&mut f);
    f.activate(1013);
    let cap = f.start("first", &task, &[seq], 1015);
    // Block while the capability is live, then settle the first dispatch.
    f.db.mutate_task(
        &cap,
        &task.task_id,
        "hold",
        &TaskMutation::Transition {
            status: TaskState::Blocked,
            waiting_reason: Some("awaiting review".into()),
            waiting_until: Some("2026-09-25T09:00:00Z".into()),
        },
        1017,
    )
    .unwrap();
    f.db.complete_dispatch(&cap, &json!({"text":"first done"}), 1018)
        .unwrap();
    assert_eq!(
        f.db.canonical_task(&task.task_id).unwrap().status,
        TaskState::Blocked
    );
    // A follow-up arrives and is queued for the still-blocked task.
    let event = f.event(
        &task.session_id,
        "next",
        Some("$root"),
        &["@a:example.test"],
        1019,
    );
    let next = f.db.admit_matrix_event(&event, 1020).unwrap();
    let same = f
        .db
        .create_verified_task_intent(&f.intent(&task.session_id, "continue", next.sequence), 1021)
        .unwrap();
    assert_eq!(same.task_id, task.task_id);
    f.db.enqueue_inbox_dispatch(&dispatch("second", &task), &[next.sequence])
        .unwrap();
    // The next host claim runs nothing: only an operator can resume the task.
    assert!(
        f.db.claim_dispatch("next_runner", 1030, 60_000, 120_000, 8)
            .unwrap()
            .is_none()
    );
    // ...and the thread is told why, in the retained product's own words.
    let (kind, body): (String, String) = f
        .sql()
        .query_row(
            "SELECT json_extract(config,'$.kind'),json_extract(config,'$.body') FROM task_notices WHERE json_extract(config,'$.kind')='task_blocked'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "task_blocked");
    assert_eq!(
        body,
        "Waiting: this task is blocked and must be explicitly resumed by an operator before another runner can start."
    );
}

/// The retained product posts the new status into the task thread on every
/// non-replayed transition (`router/src/store.ts` `taskOperation`).
#[test]
fn native_task_status_notice() {
    let mut f = Fixture::new(false);
    let (_, seq, task) = setup_task(&mut f);
    f.activate(1013);
    let cap = f.start("first", &task, &[seq], 1015);
    f.db.mutate_task(
        &cap,
        &task.task_id,
        "hold",
        &TaskMutation::Transition {
            status: TaskState::Blocked,
            waiting_reason: Some("awaiting review".into()),
            waiting_until: Some("2026-09-25T09:00:00Z".into()),
        },
        1017,
    )
    .unwrap();
    f.db.mutate_task(
        &cap,
        &task.task_id,
        "resume",
        &TaskMutation::Transition {
            status: TaskState::InProgress,
            waiting_reason: None,
            waiting_until: None,
        },
        1018,
    )
    .unwrap();
    let bodies: Vec<String> = f
        .sql()
        .prepare("SELECT json_extract(config,'$.body') FROM task_notices WHERE json_extract(config,'$.body') LIKE 'Task status: %' ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(bodies, ["Task status: blocked", "Task status: in_progress"]);
}

/// The retained product explains in the thread why a completed task did not
/// continue when the follow-up lacks the requester's fresh authority
/// (`router/src/store.ts` `claimDispatch`, `completed_task_followup`).
#[test]
fn native_completed_task_followup_notice() {
    let mut f = Fixture::new(false);
    let (_, seq, task) = setup_task(&mut f);
    f.activate(1013);
    let cap = f.start("first", &task, &[seq], 1015);
    f.done(&cap, &task, 1017);
    f.db.complete_dispatch(&cap, &json!({"done":true}), 1018)
        .unwrap();
    // A member who is not the original requester mentions the agent in the
    // task thread: the turn does not start, and the thread hears why.
    let mut event = f.event(
        &task.session_id,
        "other",
        Some("$root"),
        &["@a:example.test"],
        1019,
    );
    event.event.sender_mxid = "@other:example.test".into();
    assert!(!f.db.admit_matrix_event(&event, 1020).unwrap().wake);
    let (kind, body): (String, String) = f
        .sql()
        .query_row(
            "SELECT json_extract(config,'$.kind'),json_extract(config,'$.body') FROM task_notices WHERE json_extract(config,'$.kind')='completed_task_followup'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "completed_task_followup");
    assert_eq!(
        body,
        "This task is complete. To continue it, the original requester must send a new message mentioning the agent in this thread. Other project members can start a new task by mentioning the agent in the main room."
    );
    // Each refused attempt is explained, like the retained product's
    // per-dispatch keys: a second distinct non-requester mention queues its
    // own explanation.
    let mut again = f.event(
        &task.session_id,
        "other2",
        Some("$root"),
        &["@a:example.test"],
        1021,
    );
    again.event.sender_mxid = "@other:example.test".into();
    f.db.admit_matrix_event(&again, 1022).unwrap();
    let n: u64 = f
        .sql()
        .query_row(
            "SELECT COUNT(*) FROM task_notices WHERE json_extract(config,'$.kind')='completed_task_followup'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 2);
}

/// The retained product says the operator's continue-resolution in the thread
/// (`router/src/store.ts` `resolveOutcome`, the `continue` branch).
#[test]
fn native_outcome_resolved_continue_notice() {
    let mut f = Fixture::new(false);
    let (_, seq, task) = setup_task(&mut f);
    f.activate(1013);
    // The lease lapses; the next claim's expiry sweep settles the started
    // dispatch as outcome_unknown (`lose` via `expire`, the state the
    // operator resolution path requires).
    let _cap = f.start("first", &task, &[seq], 1015);
    f.db.claim_dispatch("sweeper", 61_016, 60_000, 120_000, 8)
        .unwrap();
    let mut next = dispatch("recovery", &task);
    next.payload =
        json!({"instruction":"Inspect previous partial output and finish only remaining work"});
    f.db.recover_dispatch("first", &next, "Inspected result", 61_020)
        .unwrap();
    let (kind, body): (String, String) = f
        .sql()
        .query_row(
            "SELECT json_extract(config,'$.kind'),json_extract(config,'$.body') FROM task_notices WHERE json_extract(config,'$.kind')='outcome_resolved'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "outcome_resolved");
    assert_eq!(
        body,
        "Operator inspection completed. A new recovery dispatch was queued from an explicit recovery instruction; the previous dispatch remains outcome_unknown and was not replayed."
    );
}

/// The retained product says in the thread when a queued dispatch waits on a
/// workspace quarantined by an unresolved previous run
/// (`router/src/store.ts` `claimDispatch`, the dirty-resource skip).
#[test]
fn native_workspace_quarantined_notice() {
    let mut f = Fixture::new(false);
    let (_, seq, task) = setup_task(&mut f);
    f.activate(1013);
    f.db.register_workspace("ws").unwrap();
    let mut first = dispatch("first", &task);
    first.resources = vec![ResourceLease {
        id: "ws".into(),
        exclusive: true,
    }];
    f.db.enqueue_inbox_dispatch(&first, &[seq]).unwrap();
    let cap =
        f.db.claim_dispatch("fixture_runner", 1015, 60_000, 120_000, 8)
            .unwrap()
            .unwrap();
    f.db.start_dispatch(&cap, 1016).unwrap();
    // A different session's task needs the same workspace: its dispatch is
    // enqueued while the workspace is still clean (enqueue refuses a dirty
    // workspace, the claim is what waits).
    let event = f.event("b", "root_b", None, &["@b:example.test"], 1017);
    let source = f.db.admit_matrix_event(&event, 1018).unwrap();
    let other =
        f.db.create_verified_task_intent(&f.intent("b", "request", source.sequence), 1019)
            .unwrap();
    while let Some(claim) = f.db.claim_verified_task_notice(1020, 1000).unwrap() {
        f.db.begin_verified_task_notice_send(&claim.claim.notice.id, &claim.claim.token, 1020)
            .unwrap();
        f.db.deliver_verified_task_notice(
            &claim.claim.notice.id,
            &claim.claim.token,
            &notice_delivery(&claim),
            1021,
        )
        .unwrap();
    }
    let mut second = dispatch("second", &other);
    second.resources = vec![ResourceLease {
        id: "ws".into(),
        exclusive: true,
    }];
    f.db.enqueue_inbox_dispatch(&second, &[source.sequence])
        .unwrap();
    // The lease lapses: the sweep settles the started dispatch as
    // outcome_unknown and marks its exclusive workspace dirty.
    f.db.claim_dispatch("sweeper", 61_016, 60_000, 120_000, 8)
        .unwrap();
    // The queued dispatch is not a claim candidate, and the thread hears why.
    assert!(
        f.db.claim_dispatch("fixture_runner", 62_025, 60_000, 120_000, 8)
            .unwrap()
            .is_none()
    );
    let (kind, body): (String, String) = f
        .sql()
        .query_row(
            "SELECT json_extract(config,'$.kind'),json_extract(config,'$.body') FROM task_notices WHERE json_extract(config,'$.kind')='workspace_quarantined'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "workspace_quarantined");
    assert_eq!(
        body,
        "Waiting: this workspace is quarantined because a previous runner stopped after work may have started. An operator must inspect and resolve that outcome before another writer can run."
    );
}

/// The retained product says in the thread when a queued dispatch waits on a
/// workspace held by a parked task (`router/src/store.ts` `claimDispatch`,
/// the leased-resource skip).
#[test]
fn native_waiting_for_approval_notice() {
    let mut f = Fixture::new(false);
    let (_, seq, task) = setup_task(&mut f);
    f.activate(1013);
    f.db.register_workspace("ws").unwrap();
    let mut first = dispatch("first", &task);
    first.resources = vec![ResourceLease {
        id: "ws".into(),
        exclusive: true,
    }];
    f.db.enqueue_inbox_dispatch(&first, &[seq]).unwrap();
    let cap =
        f.db.claim_dispatch("fixture_runner", 1015, 60_000, 120_000, 8)
            .unwrap()
            .unwrap();
    f.db.start_dispatch(&cap, 1016).unwrap();
    f.db.park_dispatch(&cap, true, 1017).unwrap();
    let event = f.event("b", "root_b", None, &["@b:example.test"], 1018);
    let source = f.db.admit_matrix_event(&event, 1019).unwrap();
    let other =
        f.db.create_verified_task_intent(&f.intent("b", "request", source.sequence), 1020)
            .unwrap();
    while let Some(claim) = f.db.claim_verified_task_notice(1021, 1000).unwrap() {
        f.db.begin_verified_task_notice_send(&claim.claim.notice.id, &claim.claim.token, 1021)
            .unwrap();
        f.db.deliver_verified_task_notice(
            &claim.claim.notice.id,
            &claim.claim.token,
            &notice_delivery(&claim),
            1022,
        )
        .unwrap();
    }
    let mut second = dispatch("second", &other);
    second.resources = vec![ResourceLease {
        id: "ws".into(),
        exclusive: true,
    }];
    f.db.enqueue_inbox_dispatch(&second, &[source.sequence])
        .unwrap();
    assert!(
        f.db.claim_dispatch("fixture_runner", 1026, 60_000, 120_000, 8)
            .unwrap()
            .is_none()
    );
    let (kind, body): (String, String) = f
        .sql()
        .query_row(
            "SELECT json_extract(config,'$.kind'),json_extract(config,'$.body') FROM task_notices WHERE json_extract(config,'$.kind')='waiting_for_approval'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "waiting_for_approval");
    assert_eq!(
        body,
        "Waiting: this task is queued because its workspace is held by another task awaiting owner approval."
    );
}

/// The retained product says the operator's inspection outcome in the thread
/// (`router/src/store.ts` `resolveOutcome`): the accept_completed and
/// keep_blocked branches, each with its own words.
#[test]
fn native_outcome_resolved_settlement_notices() {
    for (action, expected) in [
        (
            OutcomeAction::AcceptCompleted,
            "Operator inspection completed. The current result was accepted as complete; no dispatch was replayed.",
        ),
        (
            OutcomeAction::KeepBlocked,
            "Operator inspection completed. The task remains blocked; no dispatch was replayed.",
        ),
    ] {
        let mut f = Fixture::new(false);
        let (_, seq, task) = setup_task(&mut f);
        f.activate(1013);
        // An exclusive workspace: the unknown run's quarantine then dirties
        // it, which is the operator-resolution precondition (`snapshot`'s
        // `held` check).
        f.db.register_workspace("ws").unwrap();
        let mut first = dispatch("first", &task);
        first.resources = vec![ResourceLease {
            id: "ws".into(),
            exclusive: true,
        }];
        f.db.enqueue_inbox_dispatch(&first, &[seq]).unwrap();
        let cap =
            f.db.claim_dispatch("fixture_runner", 1015, 60_000, 120_000, 8)
                .unwrap()
                .unwrap();
        f.db.start_dispatch(&cap, 1016).unwrap();
        // The lease lapses: the sweep settles the started dispatch as
        // outcome_unknown, quarantining the session and dirtying the
        // exclusive workspace.
        f.db.claim_dispatch("sweeper", 61_016, 60_000, 120_000, 8)
            .unwrap();
        // The guardian's stop was reported but unproven (ADR-182 decision 3):
        // the open agent fence plus the recorded stop evidence are the
        // inspection material; no host receipt exists to fingerprint.
        f.sql()
            .execute(
                "INSERT INTO dispatch_stops(dispatch_id,fence,reason,created_at,evidence,settled_at) VALUES('first',1,'cleanup_unproven',61_017,NULL,NULL)",
                [],
            )
            .unwrap();
        f.sql()
            .execute(
                "INSERT INTO agent_fences(engagement_id,dispatch_id,fence,reason,created_at) VALUES(?1,'first',1,'cleanup_unproven',61_017)",
                [&f.agents[0]],
            )
            .unwrap();
        f.sql()
            .execute(
                "INSERT INTO runner_attempt_events(dispatch_id,fence,seq,at_ms,phase,detail) VALUES('first',1,(SELECT COALESCE(MAX(seq),0)+1 FROM runner_attempt_events WHERE dispatch_id='first' AND fence=1),61_017,'stop_reported','\"fixture guardian reported the stop\"')",
                [],
            )
            .unwrap();
        let inspection =
            f.db.begin_outcome_inspection(&f.agents[0], "first", 60_000, 61_030)
                .unwrap();
        let command = OutcomeResolution {
            original: "first".into(),
            request_id: "operator_resolution".into(),
            inspection_id: inspection["inspectionId"].as_str().unwrap().into(),
            inspection_token: inspection["inspectionToken"].as_str().unwrap().into(),
            action,
            operator_note: "Fixture inspection of the stopped run".into(),
            replacement: None,
        };
        f.db.resolve_stopped_dispatch(&f.agents[0], &command, 61_031)
            .unwrap();
        let (kind, body): (String, String) = f
            .sql()
            .query_row(
                "SELECT json_extract(config,'$.kind'),json_extract(config,'$.body') FROM task_notices WHERE json_extract(config,'$.kind')='outcome_resolved'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(kind, "outcome_resolved");
        assert_eq!(body, expected);
    }
}
/// The delivery-feedback notice, end to end (task #5, bridge-matrix.js:6492-6572):
/// a human's group message whose mention cannot reach its target must leave the
/// TS notice text in the room, and the message must still be admitted - TS sends
/// the notice after acceptance and `sendDeliveryNotice` swallows its own failure.
#[test]
fn native_verified_ingress_emits_delivery_feedback_notice() {
    let mut f = Fixture::new(false);
    let (_, _, task) = setup_task(&mut f);
    let before = count(&f.sql(), "admitted_messages");
    // `@zoe` has no transport (unknown) and is not in the joined set: the
    // backend's `mentions_unknown` case (backend-v2.js:16817). A follow-up in
    // this group thread is rooted at `$root`, exactly as
    // `native_verified_ingress_followup` admits one.
    let event = f.event(
        &task.session_id,
        "stranger",
        Some("$root"),
        &["@zoe:example.test"],
        1014,
    );
    let receipt = f.db.admit_matrix_event(&event, 1015).unwrap();
    assert!(receipt.projected);
    // The message was still admitted: the notice is additive, never a refusal.
    assert_eq!(count(&f.sql(), "admitted_messages"), before + 1);
    let (kind, config): (String, String) = f
        .sql()
        .query_row(
            "SELECT json_extract(config,'$.kind'), config FROM task_notices WHERE json_extract(config,'$.kind') LIKE 'delivery_feedback_%'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, format!("delivery_feedback_{}", receipt.sequence));
    let notice: Value = serde_json::from_str(&config).unwrap();
    assert_eq!(notice["task_id"], json!(task.task_id));
    assert_eq!(
        notice["body"],
        json!("⚠️ Mention targets not found in agent registry: @zoe.")
    );
    // Idempotent: re-admitting the same event adds no second notice.
    assert!(!f.db.admit_matrix_event(&event, 1016).unwrap().created);
    assert_eq!(
        f.sql()
            .query_row(
                "SELECT COUNT(*) FROM task_notices WHERE json_extract(config,'$.kind') LIKE 'delivery_feedback_%'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
    // A mention that IS a provisioned member says nothing at all.
    let clean = f.event(
        &task.session_id,
        "clean",
        Some("$root"),
        &["@a:example.test"],
        1017,
    );
    f.db.admit_matrix_event(&clean, 1018).unwrap();
    assert_eq!(
        f.sql()
            .query_row(
                "SELECT COUNT(*) FROM task_notices WHERE json_extract(config,'$.kind') LIKE 'delivery_feedback_%'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
}

/// Board #97. A shared room answered `!help` once per joined agent because the
/// answer's identity was derived **per session** (`command_notices.rs:357`),
/// while every agent's driver answers its own intake
/// (`bootstrap/driver.rs:715-717`) — so three joined agents meant three
/// `=== Agent Bridge Bot Commands ===` notices. The retained bridge answered
/// ONCE because it deduplicated the command on the Matrix **event id** in one
/// process (`isDuplicateMatrixEvent`, bridge-matrix.js:4117-4120). Native keeps
/// that rule in the one thing all agents share — the store — so it holds across
/// processes and restarts too.
#[test]
fn native_bot_command_is_answered_once_for_the_event_not_once_per_agent() {
    let mut f = Fixture::with_agents(&["a", "b", "c"], false);
    // ONE event in a room all three agents are joined to. Every agent's intake
    // admits it — it is a fact in the room — and none of them wakes for it.
    for session in ["a", "b", "c"] {
        let mut event = f.event(session, "help", None, &[], 1010);
        event.event.body = "!help".into();
        assert!(!f.db.admit_matrix_event(&event, 1011).unwrap().wake);
    }
    // Three session_inputs rows, one admitted message: the same room event seen
    // by three agents, which is the race the live rig exposed.
    let sql = f.sql();
    assert_eq!(count(&sql, "admitted_messages"), 1);
    assert_eq!(
        sql.query_row(
            "SELECT COUNT(*) FROM session_inputs WHERE message_sequence=(SELECT sequence FROM admitted_messages LIMIT 1)",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        3
    );
    // Each agent's poll offers it — before anyone has answered.
    for session in ["a", "b", "c"] {
        assert_eq!(
            f.db.pending_command_lines(session, 16).unwrap().len(),
            1,
            "session {session} has the unanswered line before anyone answers"
        );
    }
    // Whoever gets there first answers FOR THE EVENT. The others are handed the
    // winner's own receipt and must not queue a second reply.
    let mut answer = |session: &str| {
        f.db.submit_command_notice(
            &CommandNoticeRequest {
                session_id: session.into(),
                body: "=== Agent Bridge Bot Commands ===".into(),
                html: None,
                source_event_id: "$help".into(),
            },
            1012,
        )
        .unwrap()
    };
    let first = answer("a");
    assert_eq!(first.session_id, "a");
    assert!(!first.replayed);
    for loser in ["b", "c"] {
        let receipt = answer(loser);
        assert_eq!(
            receipt.session_id, "a",
            "{loser} must be told the answer is a's, not queue its own"
        );
        assert!(receipt.replayed);
    }
    // Exactly ONE answer exists for the event, however many agents saw it.
    assert_eq!(count(&sql, "command_notices"), 1);
    // And the event is ANSWERED: no agent is offered it again, so the room hears
    // exactly one `!help` reply.
    for session in ["a", "b", "c"] {
        assert!(
            f.db.pending_command_lines(session, 16).unwrap().is_empty(),
            "session {session} must not be offered an answered event"
        );
    }
}

/// Board #113. The live room answered NOTHING to a later `!help`, though an
/// earlier one had been answered — the operator saw no reply and no log line,
/// while `command_notices` held only the two rows from the OLDER `!help`.
///
/// Cause: `pending_command_lines` applied its `LIMIT` to EVERY
/// `session_inputs` row of the session and only filtered for `!` lines in Rust,
/// so once a room had more than `limit` admitted rows the newest `!` line fell
/// outside the window and was offered to NOBODY (no row, no send, no trace).
/// The live sessions held 67 rows, so the operator's `!help` at `seq` 51 was
/// invisible.
///
/// This reproduces the live shape through the production custody path: two
/// agents in one room, a FIRST `!help` already answered, ordinary traffic that
/// pushes the session well past any first-`limit` window, then a SECOND
/// `!help`. It must still be offered to BOTH agents and answered exactly ONCE
/// (#97's rule).
#[test]
fn native_bot_command_is_answered_after_the_session_outgrows_the_window() {
    let mut f = Fixture::with_agents(&["a", "b"], false);
    let submit = |f: &mut Fixture, session: &str, event: &str| {
        f.db.submit_command_notice(
            &CommandNoticeRequest {
                session_id: session.into(),
                body: "=== Agent Bridge Bot Commands ===".into(),
                html: None,
                source_event_id: event.into(),
            },
            1012,
        )
        .unwrap()
    };
    // The FIRST `!help` — the live rig's older event, which already HAS rows.
    for session in ["a", "b"] {
        let mut first = f.event(session, "help1", None, &[], 1010);
        first.event.body = "!help".into();
        // A `!` line is admitted, recorded, and wakes nobody.
        assert!(!f.db.admit_matrix_event(&first, 1011).unwrap().wake);
    }
    let first = submit(&mut f, "a", "$help1");
    assert_eq!(first.session_id, "a");
    assert!(!first.replayed);
    let loser = submit(&mut f, "b", "$help1");
    assert_eq!(loser.session_id, "a", "the second agent gets a's receipt");
    assert!(loser.replayed);
    assert_eq!(count(&f.sql(), "command_notices"), 1);
    // The first `!help` was ANSWERED — driven to `delivered` through the same
    // custody the service uses. This is the live store's own shape: its two
    // `command_notices` rows are both `delivered`, belonging to the OLDER
    // `!help`; only the newer one was left unanswered.
    let first_claim =
        f.db.claim_command_notice_for_session("a", 1013, 60_000)
            .unwrap()
            .unwrap();
    assert_eq!(first_claim.claim.notice.source_event_id, "$help1");
    let first_send = f
        .db
        .begin_command_notice_send(&first_claim.claim.notice.id, &first_claim.claim.token, 1014)
        .unwrap();
    assert_eq!(
        f.db.deliver_command_notice(
            &first_claim.claim.notice.id,
            &first_claim.claim.token,
            &ReplyDeliveryObservation {
                transaction_id: first_send.notice.transaction_id.clone(),
                digest: first_send.digest.clone(),
                server_name: first_send.route.server_name.clone(),
                room_id: first_send.route.room_id.clone(),
                sender_mxid: first_send.route.sender_mxid.clone(),
                device_id: first_send.route.device_id.clone(),
                event_id: "$help1_answer".into(),
                encrypted: first_send.route.encrypted,
            },
            1015,
        )
        .unwrap()
        .state,
        "delivered"
    );
    // Ordinary room traffic, seen by both agents exactly as a real room event
    // is, carries the session far past any first-16 window — what the live
    // room's 67 rows did.
    for round in 0..40u64 {
        for session in ["a", "b"] {
            let filler = f.event(session, &format!("filler{round}"), None, &[], 1020 + round);
            f.db.admit_matrix_event(&filler, 1030 + round).unwrap();
        }
    }
    // The SECOND `!help`, later in the same room. Before the fix this line was
    // offered to NOBODY: it sat outside the window of raw inputs.
    for session in ["a", "b"] {
        let mut second = f.event(session, "help2", None, &[], 1200);
        second.event.body = "!help".into();
        assert!(!f.db.admit_matrix_event(&second, 1201).unwrap().wake);
    }
    for session in ["a", "b"] {
        let offered = f.db.pending_command_lines(session, 16).unwrap();
        assert_eq!(
            offered.len(),
            1,
            "session {session} must still be offered the later !help once the \
             session outgrew the window; the earlier one stays quiet because it \
             is already answered"
        );
        assert_eq!(offered[0].event_id, "$help2");
    }
    // ...and it is answered exactly ONCE, however many agents are joined: the
    // first session to submit owns the event, the other is handed its receipt.
    let winner = submit(&mut f, "a", "$help2");
    assert_eq!(winner.session_id, "a");
    assert!(!winner.replayed);
    let loser = submit(&mut f, "b", "$help2");
    assert_eq!(
        loser.session_id, "a",
        "b must be told the later !help is a's answer, not queue a second reply"
    );
    assert!(loser.replayed);
    assert_eq!(count(&f.sql(), "command_notices"), 2);
    // The winner really SAYS it — through the same custody the service uses
    // (claim -> one-shot begin -> deliver), so "exactly one reply" is a
    // delivered `m.notice`, not merely one queued row.
    let claimed =
        f.db.claim_command_notice_for_session("a", 1013, 60_000)
            .unwrap()
            .expect("the later !help is claimable by its owning session");
    assert_eq!(claimed.claim.notice.source_event_id, "$help2");
    // The other agent cannot claim it: the row belongs to `a`.
    assert!(
        f.db.claim_command_notice_for_session("b", 1013, 60_000)
            .unwrap()
            .is_none(),
        "b must have nothing to claim for a's answer"
    );
    let send =
        f.db.begin_command_notice_send(&claimed.claim.notice.id, &claimed.claim.token, 1014)
            .unwrap();
    let delivered =
        f.db.deliver_command_notice(
            &claimed.claim.notice.id,
            &claimed.claim.token,
            &ReplyDeliveryObservation {
                transaction_id: send.notice.transaction_id.clone(),
                digest: send.digest.clone(),
                server_name: send.route.server_name.clone(),
                room_id: send.route.room_id.clone(),
                sender_mxid: send.route.sender_mxid.clone(),
                device_id: send.route.device_id.clone(),
                event_id: "$help2_answer".into(),
                encrypted: send.route.encrypted,
            },
            1015,
        )
        .unwrap();
    assert_eq!(delivered.state, "delivered");
    // Exactly ONE answer reached the room for the later event.
    assert_eq!(
        f.sql()
            .query_row(
                "SELECT COUNT(*) FROM command_notices WHERE source_event_id='$help2' AND state='delivered'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
    for session in ["a", "b"] {
        assert!(
            f.db.pending_command_lines(session, 16).unwrap().is_empty(),
            "session {session} must not be re-offered an answered command"
        );
    }
}

/// Board #113, the `/thread` sibling. `pending_thread_directives` shared the
/// identical window defect: its `LIMIT` bounded every `session_inputs` row and
/// the `/thread` filter ran in Rust afterwards, so a directive past the
/// session's first `limit` inputs was dropped with no trace. It is fixed the
/// same way and must stay reachable on a session that has outgrown the window.
#[test]
fn native_thread_directive_is_offered_after_the_session_outgrows_the_window() {
    let mut f = Fixture::with_agents(&["a"], true);
    // Ordinary traffic first, so the directive is nowhere near the window.
    for round in 0..40u64 {
        let filler = f.event("a", &format!("filler{round}"), None, &[], 1020 + round);
        f.db.admit_matrix_event(&filler, 1030 + round).unwrap();
    }
    let mut directive = f.event("a", "directive", None, &[], 1200);
    directive.event.body = "/thread mode plan".into();
    // A directive is consumed before routing: admitted, recorded, wakes nobody.
    assert!(!f.db.admit_matrix_event(&directive, 1201).unwrap().wake);
    let offered = f.db.pending_thread_directives("a", 16).unwrap();
    assert_eq!(
        offered.len(),
        1,
        "the directive must still be offered once the session outgrew the window"
    );
    assert_eq!(offered[0].event_id, "$directive");
    assert_eq!(offered[0].body, "/thread mode plan");
}

/// The other half of TS's rule: a command reply whose send FAILED is recorded
/// and never repeated — `bridge-matrix.js:6921-6925` posts "The command will
/// not be repeated automatically." Native does not re-answer either: the
/// winner's unknown outcome parks the row as `uncertain`, and no agent — the
/// winner included — is offered the event again. Without that, a failed send
/// would hand the event to the next agent, which is the same defect wearing a
/// different hat.
#[test]
fn native_failed_bot_command_answer_is_not_repeated_by_another_agent() {
    let mut f = Fixture::with_agents(&["a", "b", "c"], false);
    for session in ["a", "b", "c"] {
        let mut event = f.event(session, "help", None, &[], 1010);
        event.event.body = "!help".into();
        f.db.admit_matrix_event(&event, 1011).unwrap();
    }
    f.db.submit_command_notice(
        &CommandNoticeRequest {
            session_id: "a".into(),
            body: "=== Agent Bridge Bot Commands ===".into(),
            html: None,
            source_event_id: "$help".into(),
        },
        1012,
    )
    .unwrap();
    // `a` claims it and begins the send, and the outcome is never established:
    // the row stays `sending` under a lease that then lapses, which is exactly
    // the unknown outcome the send path refuses to guess about.
    let claimed =
        f.db.claim_command_notice_for_session("a", 1012, 1000)
            .unwrap()
            .unwrap();
    f.db.begin_command_notice_send(&claimed.claim.notice.id, &claimed.claim.token, 1013)
        .unwrap();
    // Past the lease, the store reconciles the un-settled send to `uncertain`.
    assert!(
        f.db.claim_command_notice_for_session("b", 3013, 1000)
            .unwrap()
            .is_none(),
        "b must not claim an answer whose send outcome is unknown"
    );
    assert_eq!(
        f.db.command_notice_receipt(&claimed.claim.notice.id)
            .unwrap()
            .state,
        "uncertain"
    );
    // The event is neither offered again nor answered twice, by anyone.
    for session in ["a", "b", "c"] {
        assert!(
            f.db.pending_command_lines(session, 16).unwrap().is_empty(),
            "session {session} must not re-offer a command whose reply was attempted"
        );
    }
    assert_eq!(count(&f.sql(), "command_notices"), 1);
}

/// Board #116: the ⏳/✅ activity notice about a question asked IN A THREAD must
/// land IN that thread, like the answer it tracks. The session that answers is
/// ROOM-scoped (board #112 — the thread root has no task binding), so the stored
/// notice route carries no thread root; it is read from the SOURCE message's own
/// `threadRootEventId`, exactly as the answer is
/// (`bridge-matrix.js:3318-3393`). The STORED route is untouched, so `current()`
/// keeps matching it.
#[test]
fn native_verified_ingress_activity_notice_stays_in_source_thread() {
    use hagency_store::{AttemptEvent, AttemptPhase};
    for direct in [false, true] {
        let mut f = Fixture::new(direct);
        let mentions: Vec<&str> = if direct {
            vec![]
        } else {
            vec!["@a:example.test"]
        };
        let mut follow = f.event("a", "question", None, &mentions, 1010);
        follow.event.thread_root = Some("$question".into());
        let source = f.db.admit_matrix_event(&follow, 1011).unwrap();
        f.db.create_canonical_task("t", "a", "Add 11", 1012)
            .unwrap();
        f.db.enqueue_inbox_dispatch(
            &DispatchInput {
                id: "d".into(),
                session_id: "a".into(),
                task_id: Some("t".into()),
                resources: vec![],
                payload: json!({"instruction": "Add 11"}),
            },
            &[source.sequence],
        )
        .unwrap();
        let cap =
            f.db.claim_dispatch("runner", 1013, 60_000, 120_000, 8)
                .unwrap()
                .unwrap();
        f.db.start_dispatch(&cap, 1014).unwrap();
        // The lifecycle hook that queues the ⏳ notice.
        f.db.record_attempt_event(
            &AttemptEvent {
                dispatch_id: cap.dispatch_id.clone(),
                fence: cap.fence,
                phase: AttemptPhase::Initialized,
                detail: json!({}),
            },
            1015,
        )
        .unwrap();
        let claim =
            f.db.claim_verified_task_notice(1016, 1000)
                .unwrap()
                .unwrap();
        assert_eq!(
            claim.route.thread_root.as_deref(),
            if direct { None } else { Some("$question") },
            "the notice lands in the thread the question arrived in — group only, \
             a DM strips the relation (`lib/matrix-direct-chat.js:270`)"
        );
        assert_eq!(
            claim.claim.notice.thread_root.as_deref(),
            claim.route.thread_root.as_deref(),
            "the notice's own thread root agrees with the route it was claimed on"
        );
        assert_eq!(
            claim.source_event_id, "$question",
            "it still names the question it is about"
        );
        // The STORED route keeps its room scope: `current()` compares against it.
        assert_eq!(
            f.sql()
                .query_row(
                    "SELECT json_extract(verified_route,'$.thread_root') FROM task_notices WHERE id=?1",
                    [&claim.claim.notice.id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .unwrap(),
            None,
            "the stored route is not rewritten"
        );
    }
}
