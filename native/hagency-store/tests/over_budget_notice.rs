//! ADR-183 decision D, the store's part: the one thread notice an
//! over-budget turn earns. Queued once per task for a verified thread
//! session, rooted at the request the dispatch answers, in the same custody
//! the driver's notice delivery already posts; a dispatch with nowhere to
//! say it says nothing. The fixture is copied from `tests/agent_fences.rs`
//! on purpose: that file belongs to other work and must not be edited.
mod common;
use common::*;
use hagency_core::{
    agent_inbox::{AgentInboxPlan, AgentInboxSelection},
    ingress::MatrixEventObservation,
    messages::InboundMessage,
    replies::*,
    tasks::*,
};
use hagency_store::{DomainRepository, EffectOutcome, Error, OverBudgetNotice};
use serde_json::json;
use std::collections::BTreeSet;

const ROOM: &str = "!project:example.test";

struct Fixture {
    root: tempfile::TempDir,
    db: DomainRepository,
    engagement: String,
}
impl Fixture {
    /// One agent with an available transport and the Group room observed,
    /// plus a plain (non-Matrix) session of a second engagement for the
    /// no-thread case.
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
        // A verified thread session of the agent, like `tests/agent_fences.rs`.
        db.resolve_verified_matrix_session(
            &SessionBinding {
                id: "threaded".into(),
                engagement_id: e.id.clone(),
                room_id: ROOM.into(),
                thread_root: Some("$thread_threaded".into()),
            },
            1003,
        )
        .unwrap();
        // The plain session of another engagement: no Matrix route at all.
        let other_proof = proof(&request("two", "Other", &pool, 100));
        let other = db.admit(&other_proof, 1000).unwrap();
        db.approve("approve_two", &other_proof, 1000).unwrap();
        let other_effect = db.claim_effect().unwrap().unwrap();
        db.observe_effect(
            &other_effect.id,
            other_effect.fence,
            &EffectOutcome::Applied {
                receipt: "fixture account".into(),
            },
        )
        .unwrap();
        db.register_session(&SessionBinding {
            id: "plain".into(),
            engagement_id: other.id,
            room_id: ROOM.into(),
            thread_root: None,
        })
        .unwrap();
        db.register_workspace("work_plain").unwrap();
        Self {
            root,
            db,
            engagement: e.id,
        }
    }
    fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.root.path().join("state/domain.sqlite3")).unwrap()
    }
    /// The owner's request in the thread, addressed to the agent: the wake
    /// the dispatch answers and the notice is rooted at.
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
    /// The agent's own inbox dispatch on the thread, claimed and started the
    /// way the host does it.
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
}

/// Scenario "The execution budget notifies and the turn continues", the
/// store half of the notice: queued once, in the thread the dispatch
/// answers, with fixed words around the elapsed time; a second queue of the
/// same task finds the first; a dispatch with no verified thread says
/// nothing; an unknown dispatch is refused.
#[test]
fn native_over_budget_notice_queued_once() {
    let mut f = Fixture::new();
    f.admit("$request", 3000);
    let cap = f.working(3004);
    assert_eq!(
        f.db.queue_over_budget_notice(&cap.dispatch_id, 125_000, 3100)
            .unwrap(),
        OverBudgetNotice::Queued
    );
    let (kind, body, state, routed, thread, room, sender): (
        String,
        String,
        String,
        bool,
        String,
        String,
        String,
    ) = f
        .sql()
        .query_row(
            "SELECT json_extract(config,'$.kind'),json_extract(config,'$.body'),state,verified_route IS NOT NULL,json_extract(config,'$.thread_root'),json_extract(config,'$.room_id'),json_extract(config,'$.sender_engagement') FROM task_notices",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(kind, "over_budget");
    assert_eq!(
        body,
        hagency_store::over_budget_notice_body(hagency_core::notice::NoticeLanguage::En, 125_000)
    );
    assert!(
        body.starts_with("Still running after 2 min"),
        "fixed words around the elapsed time: {body}"
    );
    assert_eq!(state, "pending");
    assert!(routed, "a verified session's notice carries its route");
    assert_eq!(thread, "$thread_threaded");
    assert_eq!(room, ROOM);
    assert_eq!(sender, f.engagement);
    // Below a minute the body counts seconds; never free text either way.
    assert!(
        hagency_store::over_budget_notice_body(hagency_core::notice::NoticeLanguage::En, 45_500)
            .starts_with("Still running after 45 s")
    );
    // A second elapse of the same attempt adds nothing.
    assert_eq!(
        f.db.queue_over_budget_notice(&cap.dispatch_id, 200_000, 3200)
            .unwrap(),
        OverBudgetNotice::AlreadyQueued
    );
    let count = |f: &Fixture| {
        f.sql()
            .query_row("SELECT COUNT(*) FROM task_notices", [], |r| {
                r.get::<_, u64>(0)
            })
            .unwrap()
    };
    assert_eq!(count(&f), 1);
    // The driver's existing delivery claims it like any verified notice.
    let claim =
        f.db.claim_verified_task_notice_for(&f.engagement, 3300, 1000)
            .unwrap()
            .expect("the notice is claimable for the agent that posts it");
    assert_eq!(claim.claim.notice.kind, "over_budget");
    // A dispatch on a session with no Matrix route has nowhere to say it.
    f.db.enqueue_dispatch(&DispatchInput {
        id: "plain_dispatch".into(),
        session_id: "plain".into(),
        task_id: None,
        resources: vec![],
        payload: json!({"instruction":"fixture"}),
    })
    .unwrap();
    assert_eq!(
        f.db.queue_over_budget_notice("plain_dispatch", 90_000, 3400)
            .unwrap(),
        OverBudgetNotice::NoThread
    );
    assert_eq!(count(&f), 1);
    // An unknown dispatch is a refusal, not a silent no-op.
    assert!(matches!(
        f.db.queue_over_budget_notice("no_such_dispatch", 90_000, 3500),
        Err(Error::NotFound)
    ));
}
