//! Task #1, the store's end-to-end half: the attempt lifecycle hooks queue
//! the activity notice, revisions supersede unclaimed predecessors, and the
//! anchor of a delivered revision rides the next revision's kind so the send
//! arm can build the edit. The fixture is copied from
//! `tests/over_budget_notice.rs` on purpose (that file's own header says the
//! same about its source): same verified thread, same addressed request,
//! same working dispatch.
mod common;
use common::*;
use hagency_core::{
    agent_inbox::{AgentInboxPlan, AgentInboxSelection},
    ingress::MatrixEventObservation,
    messages::InboundMessage,
    replies::*,
    tasks::*,
};
use hagency_store::{AttemptEvent, AttemptPhase, DomainRepository, EffectOutcome};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const ROOM: &str = "!project:example.test";

struct Fixture {
    root: tempfile::TempDir,
    db: DomainRepository,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
        db.register(&registration()).unwrap();
        let pool = resource("pool", "seat", 1000);
        db.put_resource(&pool).unwrap();
        let p = proof(&request("one", "Worker", &pool, 100));
        let e = db.admit(&p, 1000).unwrap();
        db.approve("approve_one", &p, 1000).unwrap();
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
                engagement_id: e.id.clone(),
                registration_generation: 1,
                generation: 1,
                sender_mxid: "@worker:example.test".into(),
                device_id: "DEVICE_WORKER".into(),
            },
            1001,
        )
        .unwrap();
        db.observe_matrix_room(
            &MatrixRoomObservation {
                engagement_id: e.id.clone(),
                registration_generation: 1,
                transport_generation: 1,
                room_id: ROOM.into(),
                generation: 1,
                privacy: RoomPrivacy::Group {},
                joined: BTreeSet::from([
                    "@owner:example.test".to_owned(),
                    "@worker:example.test".to_owned(),
                ]),
                invite_only: true,
                encrypted: true,
            },
            1002,
        )
        .unwrap();
        db.register_workspace("work_thread").unwrap();
        db.resolve_verified_matrix_session(
            &SessionBinding {
                id: "threaded".into(),
                engagement_id: e.id,
                room_id: ROOM.into(),
                thread_root: Some("$thread_threaded".into()),
            },
            1003,
        )
        .unwrap();
        Self { root, db }
    }

    fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.root.path().join("state/domain.sqlite3")).unwrap()
    }

    /// The owner's addressed request in the thread — the wake the dispatch
    /// answers and the notice is rooted at.
    fn admit(&mut self, event_id: &str, origin_ts: u64) {
        let observation = MatrixEventObservation {
            scope: self.db.matrix_ingress_scope("threaded").unwrap(),
            event: InboundMessage {
                server_name: "example.test".into(),
                room_id: ROOM.into(),
                event_id: event_id.into(),
                sender_mxid: "@owner:example.test".into(),
                thread_root: Some("$thread_threaded".into()),
                body: "@worker:example.test draft the quarterly report".into(),
                kind: "m.text".into(),
                origin_ts,
            },
            mentions: BTreeSet::from(["@worker:example.test".to_owned()]),
            encrypted: true,
        };
        let receipt = self
            .db
            .admit_matrix_event(&observation, origin_ts + 1)
            .unwrap();
        assert!(receipt.wake, "an addressed request wakes the session");
    }

    /// The agent's inbox dispatch on the thread, claimed and started the way
    /// the host does it.
    fn working(&mut self, now: u64) -> RunnerCapability {
        let plan = AgentInboxPlan {
            session_id: "threaded".into(),
            workspace_id: "work_thread".into(),
        };
        let AgentInboxSelection::Selected { dispatch_id, .. } =
            self.db.select_agent_inbox(&plan, now).unwrap()
        else {
            panic!("the verified wake did not create a dispatch")
        };
        let cap = self
            .db
            .claim_dispatch("runner", now + 1, 60_000, 120_000, 8)
            .unwrap()
            .unwrap();
        assert_eq!(cap.dispatch_id, dispatch_id);
        self.db.start_dispatch(&cap, now + 2).unwrap();
        cap
    }

    /// One attempt observation, the way the host records it.
    fn observe(&mut self, cap: &RunnerCapability, phase: AttemptPhase, now: u64) {
        self.db
            .record_attempt_event(
                &AttemptEvent {
                    dispatch_id: cap.dispatch_id.clone(),
                    fence: cap.fence,
                    phase,
                    detail: json!({}),
                },
                now,
            )
            .unwrap();
    }

    /// The activity notice rows this dispatch owns, in enqueue order.
    fn activity_rows(&self) -> Vec<(String, String, String)> {
        self.sql()
            .prepare(
                "SELECT json_extract(config,'$.kind'),json_extract(config,'$.body'),state \
                 FROM task_notices WHERE json_extract(config,'$.kind') LIKE 'activity:%' \
                 ORDER BY rowid",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }
}

/// The acceptance's core loop, store half: the lifecycle's first
/// observation queues ONE m.notice body (revision 1, no anchor); the
/// terminal observation supersedes nothing delivered and queues the edit
/// revision carrying the delivered anchor; a third observation after the
/// terminal one queues nothing (terminal admits no events).
#[test]
fn native_activity_notice_first_send_then_edit_revision() {
    let mut f = Fixture::new();
    f.admit("$request", 3000);
    let cap = f.working(3004);
    let dispatch = cap.dispatch_id.clone();

    // First observation: the started body in the default language (English),
    // revision 1.
    f.observe(&cap, AttemptPhase::Initialized, 3005);
    let rows = f.activity_rows();
    assert_eq!(rows.len(), 1, "one notice for the first observation");
    assert_eq!(rows[0].0, format!("activity:{dispatch}:1"));
    assert_eq!(
        rows[0].1,
        "⏳ Started; waiting for the runner's next event\nRunning for 0 s · 0 tool calls, 0 returned"
    );
    assert_eq!(rows[0].2, "pending");

    // The first revision was DELIVERED (the driver posts it): record the
    // delivery the custody writer would, then observe the tool counter.
    let delivered = params_json(&json!({"server_name":"example.test","room_id":ROOM,
        "transaction_id":"t1","event_id":"$activity_first"}));
    f.sql()
        .execute(
            "UPDATE task_notices SET state='delivered',delivery=?1 \
             WHERE json_extract(config,'$.kind')=?2",
            rusqlite::params![delivered, format!("activity:{dispatch}:1")],
        )
        .unwrap();

    // A tool event through the runner entry: the first tool is immediate,
    // and the notice kind now CARRIES the delivered anchor for the edit.
    f.db.record_activity_event(
        &dispatch,
        &hagency_store::ActivityEvent::ToolStart {
            kind: "command".into(),
            event_id: "tool_1".into(),
        },
        7005,
    )
    .unwrap()
    .unwrap();
    let rows = f.activity_rows();
    assert_eq!(rows.len(), 2, "revision 2 queued beside the delivered 1");
    assert_eq!(
        rows[1].0,
        format!("activity:{dispatch}:2:$activity_first"),
        "the edit revision carries the delivered anchor"
    );
    assert_eq!(
        rows[1].1,
        "⏳ Now running a command\nRunning for 4 s · 1 tool calls, 0 returned"
    );

    // The terminal observation: revision 3 (still anchored), and nothing
    // after it admits an event.
    let delivered = params_json(&json!({"server_name":"example.test","room_id":ROOM,
        "transaction_id":"t2","event_id":"$activity_second"}));
    f.sql()
        .execute(
            "UPDATE task_notices SET state='delivered',delivery=?1 \
             WHERE json_extract(config,'$.kind')=?2",
            rusqlite::params![delivered, format!("activity:{dispatch}:2:$activity_first")],
        )
        .unwrap();
    f.observe(&cap, AttemptPhase::Settled, 9000);
    let rows = f.activity_rows();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[2].1,
        "✅ This turn is finished\nRunning for 5 s · 1 tool calls, 0 returned"
    );
    // The anchor COALESCED onto the first delivered event, not the second.
    assert_eq!(
        rows[2].0,
        format!("activity:{dispatch}:3:$activity_first"),
        "the anchor keeps the FIRST delivered event (COALESCE)"
    );
    // Terminal: nothing more.
    f.observe(&cap, AttemptPhase::Lost, 9500);
    assert!(
        f.db.record_activity_event(&dispatch, &hagency_store::ActivityEvent::Heartbeat, 9600)
            .unwrap()
            .is_none()
    );
    assert_eq!(f.activity_rows().len(), 3);
}

/// Supersede only UNCLAIMED projections (store.ts:2777-2780): a pending
/// revision is failed away when its successor queues; a claimed one stays.
/// Worded in `zh`, which keeps the retained product's text verbatim.
#[test]
fn native_activity_supersede_touches_only_pending_rows() {
    let mut f = Fixture::new();
    f.db.set_notice_language(hagency_core::notice::NoticeLanguage::Zh);
    f.admit("$request", 3000);
    let cap = f.working(3004);
    let dispatch = cap.dispatch_id.clone();
    f.observe(&cap, AttemptPhase::Initialized, 3005);
    // Claim the first revision the driver would.
    f.sql()
        .execute(
            "UPDATE task_notices SET state='claimed' \
             WHERE json_extract(config,'$.kind')=?1",
            [format!("activity:{dispatch}:1")],
        )
        .unwrap();
    f.observe(&cap, AttemptPhase::Parked, 4000);
    let rows = f.activity_rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].2, "claimed",
        "a claimed transaction stays immutable"
    );
    assert_eq!(rows[1].2, "pending");
    assert_eq!(
        rows[1].1,
        "⏸️ 等待负责人授权；请在私人审批房间处理\n已运行 0 秒 · 工具调用 0 次，已返回 0 次"
    );
    // Now it is pending: its successor supersedes it.
    f.sql()
        .execute(
            "UPDATE task_notices SET state='pending' WHERE json_extract(config,'$.kind')=?1",
            [format!("activity:{dispatch}:2")],
        )
        .unwrap();
    f.observe(&cap, AttemptPhase::Resumed, 11_000);
    let rows = f.activity_rows();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1].2, "failed");
    assert_eq!(
        rows[2].1,
        "⏳ 已收到审批决定，继续处理\n已运行 7 秒 · 工具调用 0 次，已返回 0 次"
    );
}

/// The operator's language words the notice; the language only changes the
/// words, never the revision, routing or anchor.
#[test]
fn native_activity_notice_is_worded_in_the_configured_language() {
    let mut f = Fixture::new();
    f.db.set_notice_language(hagency_core::notice::NoticeLanguage::Id);
    f.admit("$request", 3000);
    let cap = f.working(3004);
    let dispatch = cap.dispatch_id.clone();
    f.observe(&cap, AttemptPhase::Initialized, 3005);
    f.db.record_activity_event(
        &dispatch,
        &hagency_store::ActivityEvent::ToolStart {
            kind: "files".into(),
            event_id: "tool_1".into(),
        },
        5005,
    )
    .unwrap()
    .unwrap();
    let rows = f.activity_rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, format!("activity:{dispatch}:1"));
    assert_eq!(
        rows[0].1,
        "⏳ Mulai diproses; menunggu langkah runner berikutnya\nBerjalan 0 detik · 0 panggilan tool, 0 selesai"
    );
    assert_eq!(
        rows[1].1,
        "⏳ Sedang membaca atau mengubah file\nBerjalan 2 detik · 1 panggilan tool, 0 selesai"
    );
    for (_, body, _) in &rows {
        assert!(
            !body.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "{body}"
        );
    }
}

fn params_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap()
}
