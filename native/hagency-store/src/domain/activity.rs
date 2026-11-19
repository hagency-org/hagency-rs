//! The activity notice state machine (task #1): the retained
//! `router/src/activity.ts:1-78` `ActivityStore`, under this store's
//! names (`dispatch_activity` keyed by dispatch id).
//!
//! A delivery projection only — routing and lifecycle authority stay in
//! the dispatch state machine that calls it. `update()` admits an event
//! only where the TS state machine does: a terminal row
//! (`completed`/`interrupted`) admits nothing; `started` only opens a
//! row; tool events dedupe by event key, count on first insert only, and
//! `tool_end` requires its `tool_start`.
//!
//! The BODY follows TS (`activity.ts:64-75`): an icon, the phase words,
//! and a counter line, in the operator's [`NoticeLanguage`]; `zh` is the
//! retained product's text verbatim. The coalescing windows are
//! TS's too (`:56-62`): immediate for lifecycle phases and the FIRST
//! tool, else due when `now - queued_at >= 30_000` (heartbeat) / `5_000`.
//!
//! The anchor (`:76-78`): the FIRST delivered activity event id, kept by
//! COALESCE — the event every later revision edits in place.
use super::DomainRepository;
use crate::{Error, domain::task_intents, domain::verified_ingress};
use hagency_core::{notice::NoticeLanguage, project::identifier};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;

/// The tool kinds a tool event names (`activity.ts:14-19`). Only these
/// kinds are admitted; each language words them in [`label`].
const KINDS: &[&str] = &["command", "files", "search", "delegate", "tool"];

/// The display label of a tool kind, or None for a kind the notice does not
/// name: the fragment after "Now" / "Sedang" / `正在`.
fn label(language: NoticeLanguage, kind: &str) -> Option<&'static str> {
    if !KINDS.contains(&kind) {
        return None;
    }
    Some(match (language, kind) {
        (NoticeLanguage::En, "command") => "running a command",
        (NoticeLanguage::En, "files") => "reading or changing files",
        (NoticeLanguage::En, "search") => "searching",
        (NoticeLanguage::En, "delegate") => "delegating a task",
        (NoticeLanguage::En, _) => "calling a tool",
        (NoticeLanguage::Id, "command") => "menjalankan perintah",
        (NoticeLanguage::Id, "files") => "membaca atau mengubah file",
        (NoticeLanguage::Id, "search") => "mencari informasi",
        (NoticeLanguage::Id, "delegate") => "mendelegasikan tugas",
        (NoticeLanguage::Id, _) => "memanggil tool",
        (NoticeLanguage::Zh, "command") => "运行命令",
        (NoticeLanguage::Zh, "files") => "读取或修改文件",
        (NoticeLanguage::Zh, "search") => "搜索资料",
        (NoticeLanguage::Zh, "delegate") => "委派任务",
        (NoticeLanguage::Zh, _) => "调用工具",
    })
}

/// One observation the lifecycle asks the store to keep. The TS
/// `ActivityEvent` union: a lifecycle phase, or a tool phase carrying
/// its kind and the runner's dedupe id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ActivityEvent {
    Started,
    Heartbeat,
    Waiting,
    Resumed,
    Completed,
    Interrupted,
    ToolStart { kind: String, event_id: String },
    ToolEnd { kind: String, event_id: String },
}

impl ActivityEvent {
    /// The stored `phase` word — the TS vocabulary exactly, which the
    /// migration's CHECK constraint also lists.
    fn phase(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Heartbeat => "heartbeat",
            Self::Waiting => "waiting",
            Self::Resumed => "resumed",
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
            Self::ToolStart { .. } => "tool_start",
            Self::ToolEnd { .. } => "tool_end",
        }
    }
    /// The phases whose bodies go out immediately (TS `:56-58`).
    fn immediate(&self) -> bool {
        matches!(
            self,
            Self::Started | Self::Waiting | Self::Resumed | Self::Completed | Self::Interrupted
        )
    }
}

/// One activity row, as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityRow {
    pub phase: String,
    pub kind: Option<String>,
    pub tools: i64,
    pub finished: i64,
    pub started_at: i64,
    pub updated_at: i64,
    pub queued_at: i64,
    pub revision: i64,
    pub anchor: Option<String>,
}

/// What a due update produced: the revision for the notice id
/// (`activity:<dispatch>:<revision>`) and the verbatim body to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActivityUpdate {
    pub revision: i64,
    pub body: String,
}

/// The notice body (`activity.ts:64-75`) in `language`: an icon, the phase
/// words, then the elapsed time and the tool counters.
pub fn activity_body(
    language: NoticeLanguage,
    phase: &str,
    kind: Option<&str>,
    tools: i64,
    finished: i64,
    started_at: i64,
    now: i64,
) -> String {
    let elapsed = 0.max((now - started_at) / 1000);
    let icon = match phase {
        "completed" => "✅",
        "interrupted" => "⚠️",
        "waiting" => "⏸️",
        _ => "⏳",
    };
    let words = match phase {
        "tool_start" => {
            let fallback = label(language, "tool").unwrap_or_default();
            let label = kind
                .and_then(|kind| label(language, kind))
                .unwrap_or(fallback);
            match language {
                NoticeLanguage::En => format!("Now {label}"),
                NoticeLanguage::Id => format!("Sedang {label}"),
                NoticeLanguage::Zh => format!("正在{label}"),
            }
        }
        phase => phase_words(language, phase).to_owned(),
    };
    let counters = match language {
        NoticeLanguage::En => {
            format!("Running for {elapsed} s · {tools} tool calls, {finished} returned")
        }
        NoticeLanguage::Id => {
            format!("Berjalan {elapsed} detik · {tools} panggilan tool, {finished} selesai")
        }
        NoticeLanguage::Zh => {
            format!("已运行 {elapsed} 秒 · 工具调用 {tools} 次，已返回 {finished} 次")
        }
    };
    format!("{icon} {words}\n{counters}")
}

/// The words of every phase but `tool_start` (which names its tool).
fn phase_words(language: NoticeLanguage, phase: &str) -> &'static str {
    match (language, phase) {
        (NoticeLanguage::En, "waiting") => {
            "Waiting for the owner's approval; answer it in your private approval room"
        }
        (NoticeLanguage::En, "completed") => "This turn is finished",
        (NoticeLanguage::En, "interrupted") => "Interrupted; the result is not confirmed",
        (NoticeLanguage::En, "resumed") => "Approval decision received; continuing",
        (NoticeLanguage::En, "tool_end") => "Tool call returned; continuing",
        (NoticeLanguage::En, _) => "Started; waiting for the runner's next event",
        (NoticeLanguage::Id, "waiting") => {
            "Menunggu persetujuan pemilik; jawab di room persetujuan pribadi Anda"
        }
        (NoticeLanguage::Id, "completed") => "Giliran ini selesai",
        (NoticeLanguage::Id, "interrupted") => "Terhenti; hasilnya belum pasti",
        (NoticeLanguage::Id, "resumed") => "Keputusan persetujuan diterima; melanjutkan",
        (NoticeLanguage::Id, "tool_end") => "Panggilan tool selesai; melanjutkan",
        (NoticeLanguage::Id, _) => "Mulai diproses; menunggu langkah runner berikutnya",
        (NoticeLanguage::Zh, "waiting") => "等待负责人授权；请在私人审批房间处理",
        (NoticeLanguage::Zh, "completed") => "本轮处理已结束",
        (NoticeLanguage::Zh, "interrupted") => "执行已中断，结果待确认",
        (NoticeLanguage::Zh, "resumed") => "已收到审批决定，继续处理",
        (NoticeLanguage::Zh, "tool_end") => "工具调用已返回，继续处理",
        (NoticeLanguage::Zh, _) => "已开始处理，等待运行器的下一步事件",
    }
}

/// Read one dispatch's activity row, inside the caller's transaction.
pub(super) fn read(tx: &Transaction<'_>, dispatch_id: &str) -> Result<Option<ActivityRow>, Error> {
    Ok(tx
        .query_row(
            "SELECT phase,kind,tools,finished,started_at,updated_at,queued_at,revision,anchor \
             FROM dispatch_activity WHERE dispatch_id=?1",
            [dispatch_id],
            |row| {
                Ok(ActivityRow {
                    phase: row.get(0)?,
                    kind: row.get(1)?,
                    tools: row.get(2)?,
                    finished: row.get(3)?,
                    started_at: row.get(4)?,
                    updated_at: row.get(5)?,
                    queued_at: row.get(6)?,
                    revision: row.get(7)?,
                    anchor: row.get(8)?,
                })
            },
        )
        .optional()?)
}

/// The TS `update()` state machine (`activity.ts:31-62`), on the caller's
/// open transaction. Returns the due update's revision and body, or None
/// when the event was refused or coalesced away.
pub(super) fn update(
    tx: &Transaction<'_>,
    language: NoticeLanguage,
    dispatch_id: &str,
    event: &ActivityEvent,
    now: i64,
) -> Result<Option<ActivityUpdate>, Error> {
    let previous = read(tx, dispatch_id)?;
    // A terminal row admits no further events.
    if previous
        .as_ref()
        .is_some_and(|row| matches!(row.phase.as_str(), "completed" | "interrupted"))
    {
        return Ok(None);
    }
    // `started` only opens a row: no row without it, no second one with it.
    if previous.is_none() && !matches!(event, ActivityEvent::Started) {
        return Ok(None);
    }
    if previous.is_some() && matches!(event, ActivityEvent::Started) {
        return Ok(None);
    }
    let existed = previous.is_some();
    let mut row = previous.unwrap_or(ActivityRow {
        phase: "started".into(),
        kind: None,
        tools: 0,
        finished: 0,
        started_at: now,
        updated_at: now,
        queued_at: now,
        revision: 0,
        anchor: None,
    });
    if !existed {
        tx.execute(
            "INSERT INTO dispatch_activity(dispatch_id,phase,started_at,updated_at,queued_at) \
             VALUES(?1,'started',?2,?2,?2)",
            rusqlite::params![dispatch_id, now],
        )?;
    }
    match event {
        ActivityEvent::ToolStart { kind, event_id } | ActivityEvent::ToolEnd { kind, event_id } => {
            if !KINDS.contains(&kind.as_str()) {
                return Ok(None);
            }
            let key = format!("{}:{}", event.phase(), event_id);
            if matches!(event, ActivityEvent::ToolEnd { .. })
                && !tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM dispatch_activity_events \
                         WHERE dispatch_id=?1 AND event_key=?2)",
                    rusqlite::params![dispatch_id, format!("tool_start:{event_id}")],
                    |r| r.get::<_, bool>(0),
                )?
            {
                return Ok(None);
            }
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO dispatch_activity_events(dispatch_id,event_key) \
                 VALUES(?1,?2)",
                rusqlite::params![dispatch_id, key],
            )?;
            if inserted == 0 {
                return Ok(None);
            }
            row.phase = event.phase().into();
            row.kind = Some(kind.clone());
            if matches!(event, ActivityEvent::ToolStart { .. }) {
                row.tools += 1;
            } else {
                row.finished += 1;
            }
        }
        ActivityEvent::Heartbeat => {}
        other => {
            row.phase = other.phase().into();
            row.kind = None;
        }
    }
    let immediate =
        event.immediate() || (matches!(event, ActivityEvent::ToolStart { .. }) && row.tools == 1);
    let due = immediate
        || now - row.queued_at
            >= if matches!(event, ActivityEvent::Heartbeat) {
                30_000
            } else {
                5_000
            };
    tx.execute(
        "UPDATE dispatch_activity SET phase=?2,kind=?3,tools=?4,finished=?5,updated_at=?6 \
         WHERE dispatch_id=?1",
        rusqlite::params![
            dispatch_id,
            row.phase,
            row.kind,
            row.tools,
            row.finished,
            now
        ],
    )?;
    if !due {
        return Ok(None);
    }
    row.revision += 1;
    tx.execute(
        "UPDATE dispatch_activity SET queued_at=?2,revision=?3 WHERE dispatch_id=?1",
        rusqlite::params![dispatch_id, now, row.revision],
    )?;
    Ok(Some(ActivityUpdate {
        revision: row.revision,
        body: activity_body(
            language,
            &row.phase,
            row.kind.as_deref(),
            row.tools,
            row.finished,
            row.started_at,
            now,
        ),
    }))
}

/// The TS `delivered()` (`:76-78`): keep the FIRST delivered activity
/// event id — the anchor every later revision edits.
pub(super) fn delivered(tx: &Connection, dispatch_id: &str, event_id: &str) -> Result<(), Error> {
    tx.execute(
        "UPDATE dispatch_activity SET anchor=COALESCE(anchor,?2) WHERE dispatch_id=?1",
        [dispatch_id, event_id],
    )?;
    Ok(())
}

/// TS `updateActivity` (`store.ts:2775-2783`): apply the event; on a due
/// revision, supersede only UNCLAIMED activity projections of this
/// dispatch (a claimed transaction stays immutable for retries), then
/// enqueue a thread notice with the id `activity:<dispatch>:<revision>`.
/// The notice routing follows the same session/task binding the
/// over-budget notice uses — never a second routing authority.
pub(super) fn update_and_enqueue(
    tx: &Transaction<'_>,
    language: NoticeLanguage,
    dispatch_id: &str,
    event: &ActivityEvent,
    now: u64,
) -> Result<Option<ActivityUpdate>, Error> {
    let now_i64 = i64::try_from(now).map_err(|_| Error::Unavailable)?;
    let Some(update) = update(tx, language, dispatch_id, event, now_i64)? else {
        return Ok(None);
    };
    // The anchor every later revision edits: the FIRST delivered activity
    // event of this dispatch (TS `delivered()`, COALESCE semantics).
    // Resolved at enqueue from the delivery columns — the store's own
    // record of what the homeserver answered — and kept in
    // `dispatch_activity.anchor` so the earliest one survives later ones.
    // The prefix match is length-pinned `substr`, never LIKE: a dispatch
    // id may contain `_`, which LIKE would read as a wildcard.
    let prefix = format!("activity:{dispatch_id}:");
    let anchor: Option<String> = tx
        .query_row(
            "SELECT json_extract(delivery,'$.event_id') FROM task_notices \
             WHERE substr(json_extract(config,'$.kind'),1,?1)=?2 \
             AND state='delivered' AND delivery IS NOT NULL \
             ORDER BY rowid LIMIT 1",
            rusqlite::params![prefix.len() as i64, prefix.clone()],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(anchor) = &anchor {
        delivered(tx, dispatch_id, anchor)?;
    }
    // The TS dedupe key `activity:${dispatch_id}:${revision}` rides the
    // notice's KIND: `notice_id` digests (task_id, kind), so the revision
    // is what makes each edit its own outbox row. When an anchor exists
    // it is appended after the revision — the send arm then builds the
    // edit form without a second store read. Dispatch ids are colon-free
    // (`identifier()`), so the split stays unambiguous; a Matrix event id
    // carries a colon of its own and sits last.
    let kind = match &anchor {
        Some(anchor) => format!("{prefix}{}:{anchor}", update.revision),
        None => format!("{prefix}{}", update.revision),
    };
    // Supersede only UNCLAIMED projections: a claimed transaction stays
    // immutable for retries (store.ts:2777-2780).
    tx.execute(
        "UPDATE task_notices SET state='failed',error_code='activity_superseded' \
         WHERE state='pending' AND substr(json_extract(config,'$.kind'),1,?1)=?2",
        rusqlite::params![prefix.len() as i64, prefix],
    )?;
    let (task_id, session_id): (Option<String>, String) = tx
        .query_row(
            "SELECT task_id,session_id FROM runner_dispatches WHERE id=?1",
            [dispatch_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    let Some(task_id) = task_id else {
        // A dispatch with no task has no thread to say anything in —
        // the same `NoThread` shape the over-budget notice returns.
        return Ok(None);
    };
    let task = super::execution::task(tx, &task_id)?;
    // MAX() is NULL over an empty set: no addressed input, no thread —
    // the same `NoThread` shape the over-budget notice returns.
    let root: Option<u64> = tx.query_row(
        "SELECT MAX(message_sequence) FROM dispatch_inputs WHERE dispatch_id=?1 AND addressed=1",
        [dispatch_id],
        |r| r.get(0),
    )?;
    let Some(root) = root else {
        return Ok(None);
    };
    let root = verified_ingress::input_message(tx, &session_id, root)?;
    task_intents::add_notice(tx, &task, &root, &kind, update.body.clone(), now)?;
    Ok(Some(update))
}

impl DomainRepository {
    /// The runner's own activity events (TS `recordRunnerActivity`,
    /// `store.ts:2692-2705`): only tool and heartbeat events — the
    /// runner cannot set lifecycle activity, that belongs to the dispatch
    /// transitions. Refused input counts as a host observation refusal:
    /// its own savepoint, nothing left behind, the turn unaffected.
    pub fn record_activity_event(
        &mut self,
        dispatch_id: &str,
        event: &ActivityEvent,
        now: u64,
    ) -> Result<Option<ActivityUpdate>, Error> {
        identifier(dispatch_id, 128)?;
        if !matches!(
            event,
            ActivityEvent::Heartbeat
                | ActivityEvent::ToolStart { .. }
                | ActivityEvent::ToolEnd { .. }
        ) {
            return Err(hagency_core::InvalidInput("runner cannot set lifecycle activity").into());
        }
        let language = self.notice_language;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: Option<String> = tx
            .query_row(
                "SELECT state FROM runner_dispatches WHERE id=?1",
                [dispatch_id],
                |r| r.get(0),
            )
            .optional()?;
        // `started` is the ordinary window. Board #116: `complete_task_with_reply`
        // fences the dispatch MID-TURN (`owned_completion.rs:151`), so the
        // completion tool's OWN trailing `item/completed` — the `tool_end` that
        // makes the counter read N/N — arrives when the dispatch is already
        // `outcome_unknown`. TS records it: the dispatch is settled only at turn
        // end (`router/src/runner.ts:875`), so every item of the turn counts and
        // ✅ reads N/N. The fence here is the completion HOLD (an unsettled
        // `dispatch_stops` row), not a dead runner: the runner is alive and still
        // emitting, so the window stays open for exactly as long as the held
        // completion is pending, and closes when the hold resolves.
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM dispatch_stops WHERE dispatch_id=?1 \
             AND reason='owned_completion' AND settled_at IS NULL)",
            [dispatch_id],
            |r| r.get(0),
        )?;
        match state.as_deref() {
            None => return Err(Error::NotFound),
            Some("started") => {}
            Some(_) if held => {}
            Some(_) => {
                return Err(
                    hagency_core::InvalidInput("activity requires an active runner").into(),
                );
            }
        }
        tx.execute_batch("SAVEPOINT activity_notice")?;
        let result = match update_and_enqueue(&tx, language, dispatch_id, event, now) {
            Ok(value) => {
                tx.execute_batch("RELEASE activity_notice")?;
                value
            }
            Err(error) => {
                tx.execute_batch("ROLLBACK TO activity_notice; RELEASE activity_notice")?;
                return Err(error);
            }
        };
        tx.commit()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A store worded in `zh`: these cases pin the retained TS text verbatim.
    fn open() -> (tempfile::TempDir, crate::DomainRepository) {
        let root = tempfile::tempdir().unwrap();
        let mut db = crate::DomainRepository::open(&root.path().join("domain")).unwrap();
        db.set_notice_language(NoticeLanguage::Zh);
        (root, db)
    }

    /// The FK graph `dispatch_activity.dispatch_id` requires: a dispatch,
    /// its session, the session's engagement, and that engagement's
    /// registration/project/resource parents. Raw SQL is the in-crate
    /// pattern (conversations.rs does the same) — every JSON column gets
    /// a valid object, every CHECK list a legal member.
    const DISPATCH: &str = "dispatch_activity_fixture";

    fn seeded(db: &mut crate::DomainRepository) -> &'static str {
        db.db
            .execute_batch(
                "INSERT INTO registrations(fleet_id,generation,config) VALUES('fleet_a',1,'{}'); \
             INSERT INTO resources(id,preset_id,config) VALUES('res_a','preset_a','{}'); \
             INSERT INTO projects(fleet_id,id,generation,room_id,owner_mxid,owner_room_id) \
             VALUES('fleet_a','p1',1,'!r:example.test','@o:example.test','!d:example.test'); \
             INSERT INTO engagements(id,fleet_id,generation,request_id,digest,context,evidence,\
             project_id,name,resource_id,tokens,state,projection) VALUES('eng_a','fleet_a',1,\
             'req_a','digest_a','{}','{}','p1','Worker','res_a',100,'active','{}'); \
             INSERT INTO runner_sessions(id,engagement_id,binding) VALUES('sess_a','eng_a','{}'); \
             INSERT INTO runner_dispatches(id,session_id,task_id,input,digest,state) \
             VALUES('dispatch_activity_fixture','sess_a',NULL,'{}','digest_a','started');",
            )
            .unwrap();
        DISPATCH
    }

    /// A trait so tests can reach the private transaction helpers without
    /// adding public surface.
    trait Update {
        fn update(
            &mut self,
            dispatch: &str,
            event: &ActivityEvent,
            now: i64,
        ) -> Option<ActivityUpdate>;
        fn delivered(&mut self, dispatch: &str, event_id: &str) -> bool;
    }
    impl Update for crate::DomainRepository {
        fn update(
            &mut self,
            dispatch: &str,
            event: &ActivityEvent,
            now: i64,
        ) -> Option<ActivityUpdate> {
            let language = self.notice_language;
            let tx = self.db.transaction().expect("transaction opens");
            let update =
                super::update(&tx, language, dispatch, event, now).expect("update applies");
            tx.commit().expect("commit");
            update
        }
        fn delivered(&mut self, dispatch: &str, event_id: &str) -> bool {
            let tx = self.db.transaction().expect("transaction opens");
            super::delivered(&tx, dispatch, event_id).expect("delivered applies");
            tx.commit().expect("commit");
            true
        }
    }

    /// The first send, verbatim TS: ⏳, the started words, zero counters.
    #[test]
    fn started_body_is_verbatim_ts() {
        let (_root, mut db) = open();
        let dispatch = seeded(&mut db);
        let update = db.update(dispatch, &ActivityEvent::Started, 1000).unwrap();
        assert_eq!(
            update.body,
            "⏳ 已开始处理，等待运行器的下一步事件\n已运行 0 秒 · 工具调用 0 次，已返回 0 次"
        );
        assert_eq!(update.revision, 1);
    }

    /// An edit body with elapsed time and counters, verbatim TS.
    #[test]
    fn waiting_and_tool_bodies_are_verbatim_ts() {
        let (_root, mut db) = open();
        let dispatch = seeded(&mut db);
        db.update(dispatch, &ActivityEvent::Started, 1000);
        // First tool is immediate; second coalesces until the 5 s window.
        let first = db
            .update(
                dispatch,
                &ActivityEvent::ToolStart {
                    kind: "command".into(),
                    event_id: "e1".into(),
                },
                4000,
            )
            .unwrap();
        assert_eq!(
            first.body,
            "⏳ 正在运行命令\n已运行 3 秒 · 工具调用 1 次，已返回 0 次"
        );
        let none = db.update(
            dispatch,
            &ActivityEvent::ToolStart {
                kind: "files".into(),
                event_id: "e2".into(),
            },
            6000,
        );
        assert!(none.is_none(), "a second tool within the window coalesces");
        let due = db
            .update(
                dispatch,
                &ActivityEvent::ToolStart {
                    kind: "files".into(),
                    event_id: "e3".into(),
                },
                9500,
            )
            .unwrap();
        assert_eq!(
            due.body,
            "⏳ 正在读取或修改文件\n已运行 8 秒 · 工具调用 3 次，已返回 0 次"
        );
        // Waiting is immediate, with its own icon and words.
        let waiting = db.update(dispatch, &ActivityEvent::Waiting, 9600).unwrap();
        assert_eq!(
            waiting.body,
            "⏸️ 等待负责人授权；请在私人审批房间处理\n已运行 8 秒 · 工具调用 3 次，已返回 0 次"
        );
    }

    /// Terminal rows admit nothing further; heartbeats coalesce on the
    /// 30 s window; `tool_end` requires its `tool_start`; a repeat event
    /// counts nothing.
    #[test]
    fn the_state_machine_refuses_where_ts_refuses() {
        let (_root, mut db) = open();
        let dispatch = seeded(&mut db);
        // No row, no lifecycle event but started.
        assert!(
            db.update(dispatch, &ActivityEvent::Heartbeat, 1000)
                .is_none()
        );
        assert!(db.update(dispatch, &ActivityEvent::Waiting, 1000).is_none());
        db.update(dispatch, &ActivityEvent::Started, 1000);
        // A second started admits nothing.
        assert!(db.update(dispatch, &ActivityEvent::Started, 2000).is_none());
        // tool_end without tool_start.
        assert!(
            db.update(
                dispatch,
                &ActivityEvent::ToolEnd {
                    kind: "command".into(),
                    event_id: "nope".into()
                },
                2000
            )
            .is_none()
        );
        // A repeated tool_start counts nothing (dedupe).
        db.update(
            dispatch,
            &ActivityEvent::ToolStart {
                kind: "command".into(),
                event_id: "e1".into(),
            },
            2000,
        );
        let before = read(&db.db.transaction().unwrap(), dispatch)
            .unwrap()
            .unwrap();
        assert_eq!(before.tools, 1);
        assert!(
            db.update(
                dispatch,
                &ActivityEvent::ToolStart {
                    kind: "command".into(),
                    event_id: "e1".into()
                },
                3000
            )
            .is_none()
        );
        // Heartbeat coalesces until 30 s passes. TS heartbeat does NOT
        // change the phase (activity.ts:50): the row stays tool_start, so
        // the due body still reads 正在运行命令 with the elapsed time.
        assert!(
            db.update(dispatch, &ActivityEvent::Heartbeat, 20_000)
                .is_none()
        );
        let beat = db
            .update(dispatch, &ActivityEvent::Heartbeat, 32_000)
            .unwrap();
        assert_eq!(
            beat.body,
            "⏳ 正在运行命令\n已运行 31 秒 · 工具调用 1 次，已返回 0 次"
        );
        // Terminal: completed, then nothing.
        let done = db
            .update(dispatch, &ActivityEvent::Completed, 33_000)
            .unwrap();
        assert_eq!(
            done.body,
            "✅ 本轮处理已结束\n已运行 32 秒 · 工具调用 1 次，已返回 0 次"
        );
        assert!(
            db.update(dispatch, &ActivityEvent::Heartbeat, 34_000)
                .is_none()
        );
    }

    /// The anchor is the FIRST delivered id and never moves (COALESCE).
    #[test]
    fn the_anchor_keeps_the_first_delivered_event() {
        let (_root, mut db) = open();
        let dispatch = seeded(&mut db);
        db.update(dispatch, &ActivityEvent::Started, 1000);
        assert!(db.delivered(dispatch, "$first"));
        assert!(db.delivered(dispatch, "$second"));
        let row = read(&db.db.transaction().unwrap(), dispatch)
            .unwrap()
            .unwrap();
        assert_eq!(row.anchor.as_deref(), Some("$first"));
    }
}
