//! ADR-181: the per-attempt evidence a lost agent is diagnosed from. Nothing
//! here authorizes anything, promotes a status or releases custody: every
//! write is an observation beside the existing rules, bounded to fixed keys,
//! integers and control-stripped text (ADR-175), and a write that fails never
//! changes the attempt's outcome.
use super::{DomainRepository, serialize};
use crate::Error;
use hagency_core::{InvalidInput, JSON_SAFE_MAX, project::identifier, tasks::clock};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The phases an attempt can visit, in the order it can visit them.
/// `OverBudget` (ADR-183 decision D) is visited at most once, during the
/// turn, when the operation budget elapses while Codex is still working: the
/// budget is notify-only, so the phase records the fact and nothing acts on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptPhase {
    Claimed,
    SpawnStarted,
    SpawnDone,
    Initialized,
    TurnStarted,
    OverBudget,
    ApprovalRequested,
    ApprovalDecided,
    Parked,
    Resumed,
    StopRequested,
    StopReported,
    Settled,
    Failed,
    Lost,
}
impl AttemptPhase {
    const ALL: [Self; 15] = [
        Self::Claimed,
        Self::SpawnStarted,
        Self::SpawnDone,
        Self::Initialized,
        Self::TurnStarted,
        Self::OverBudget,
        Self::ApprovalRequested,
        Self::ApprovalDecided,
        Self::Parked,
        Self::Resumed,
        Self::StopRequested,
        Self::StopReported,
        Self::Settled,
        Self::Failed,
        Self::Lost,
    ];
    /// The stored word; the 039 CHECK constraint lists exactly these.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::SpawnStarted => "spawn_started",
            Self::SpawnDone => "spawn_done",
            Self::Initialized => "initialized",
            Self::TurnStarted => "turn_started",
            Self::OverBudget => "over_budget",
            Self::ApprovalRequested => "approval_requested",
            Self::ApprovalDecided => "approval_decided",
            Self::Parked => "parked",
            Self::Resumed => "resumed",
            Self::StopRequested => "stop_requested",
            Self::StopReported => "stop_reported",
            Self::Settled => "settled",
            Self::Failed => "failed",
            Self::Lost => "lost",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|phase| phase.as_str() == value)
    }
}
/// One observation the host asks the store to keep. `detail` is bounded on
/// the way in: an object of identifier keys, control-stripped strings of at
/// most 4 KiB, at most two containers deep inside it, 8 KiB serialized.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptEvent {
    pub dispatch_id: String,
    pub fence: u64,
    pub phase: AttemptPhase,
    pub detail: Value,
}
/// One row of the event log, as read back in `seq` order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptEventRow {
    pub seq: u64,
    pub at_ms: u64,
    pub phase: AttemptPhase,
    pub detail: Value,
}
/// The attempt row's clock columns (ADR-181 point 2). `Started`, `Parked` and
/// `Settled` are written once — the first observation stands, a repeat is a
/// no-op; `LastRenew` is the renewal's own mark and always moves forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptClock {
    Started,
    Parked,
    LastRenew,
    Settled,
}
impl AttemptClock {
    fn column(self) -> &'static str {
        match self {
            Self::Started => "started_at",
            Self::Parked => "parked_at",
            Self::LastRenew => "last_renew_at",
            Self::Settled => "settled_at",
        }
    }
}
/// The attempt row's clock and terminal reason, every field absent until its
/// writer observed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptClockRow {
    pub started_at: Option<u64>,
    pub parked_at: Option<u64>,
    pub last_renew_at: Option<u64>,
    pub settled_at: Option<u64>,
    pub terminal_reason: Option<String>,
}

/// At most this many events per (dispatch, fence): fifteen phases, a few
/// approval and park cycles, and the host's repeats after a lost reply.
const EVENT_LIMIT: u32 = 256;
/// The notice kind of the one thread message an over-budget turn earns
/// (ADR-183 decision D); with the task it names the notice's id, so a second
/// queue of the same attempt finds the first.
pub const OVER_BUDGET_NOTICE_KIND: &str = "over_budget";

/// What queuing the over-budget notice found (ADR-183 decision D). Fixed
/// labels for the attempt event's `notice` key; never authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverBudgetNotice {
    /// One notice is now pending for the dispatch's thread.
    Queued,
    /// The same task already has one; nothing is added.
    AlreadyQueued,
    /// The dispatch answers no verified thread (no task, no addressed
    /// request, or a session without a Matrix route): nowhere to say it.
    NoThread,
}
impl OverBudgetNotice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::AlreadyQueued => "already_queued",
            Self::NoThread => "no_thread",
        }
    }
}
/// The serialized `detail` bound.
const DETAIL_LIMIT: usize = 8192;
/// Every string inside `detail` is cut here, on a char boundary.
const STRING_LIMIT: usize = 4096;
/// Every key inside `detail` is an ASCII identifier of at most this length.
const KEY_LIMIT: usize = 64;
/// Containers nested inside the root object: `{"rows":[{"pid":1}]}` is two
/// deep (the guardian's live-row list, ADR-181 point 5b); a container inside
/// those rows would be three and is refused.
const DEPTH_LIMIT: usize = 2;
/// `terminal_reason` (ADR-181 point 2): failure, exit identity and the 512
/// byte stderr tail with their separators.
const TERMINAL_REASON_LIMIT: usize = 640;

/// Control characters become U+FFFD, then the text is cut to `limit` bytes
/// on a char boundary. Never rejects: the tail of a runtime's stderr is the
/// one free text ADR-181 admits, and it is admitted bounded, not refused.
fn bounded_text(text: &str, limit: usize) -> String {
    let mut clean: String = text
        .chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect();
    clean.truncate(clean.floor_char_boundary(limit));
    clean
}
fn key(name: &str) -> Result<(), Error> {
    let head = name
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_');
    if !head
        || name.len() > KEY_LIMIT
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(InvalidInput("attempt event detail key is not an identifier").into());
    }
    Ok(())
}
fn sanitize_object(fields: &Map<String, Value>, depth: usize) -> Result<Map<String, Value>, Error> {
    fields
        .iter()
        .map(|(name, value)| {
            key(name)?;
            Ok((name.clone(), sanitize_value(value, depth)?))
        })
        .collect()
}
fn sanitize_value(value: &Value, depth: usize) -> Result<Value, Error> {
    let nested = || {
        if depth >= DEPTH_LIMIT {
            Err(Error::Invalid(InvalidInput(
                "attempt event detail is nested too deeply",
            )))
        } else {
            Ok(depth + 1)
        }
    };
    Ok(match value {
        Value::String(text) => Value::String(bounded_text(text, STRING_LIMIT)),
        Value::Object(fields) => Value::Object(sanitize_object(fields, nested()?)?),
        Value::Array(items) => {
            let depth = nested()?;
            Value::Array(
                items
                    .iter()
                    .map(|item| sanitize_value(item, depth))
                    .collect::<Result<_, _>>()?,
            )
        }
        scalar => scalar.clone(),
    })
}
/// The bounded copy of `detail` the row stores, or why it cannot be one.
fn sanitize(detail: &Value) -> Result<String, Error> {
    let Value::Object(fields) = detail else {
        return Err(InvalidInput("attempt event detail must be an object").into());
    };
    let encoded = serialize(&Value::Object(sanitize_object(fields, 0)?))?;
    if encoded.len() > DETAIL_LIMIT {
        return Err(InvalidInput("attempt event detail exceeds 8 KiB").into());
    }
    Ok(encoded)
}
/// The one insert, inside the caller's transaction: `lose` records its
/// `lost` event through this in the settlement's own transaction, the host's
/// observations through `record_attempt_event`. Returns the new 1-based seq.
pub(super) fn insert(
    tx: &Transaction<'_>,
    dispatch_id: &str,
    fence: u64,
    phase: AttemptPhase,
    detail: &Value,
    now: u64,
) -> Result<u64, Error> {
    identifier(dispatch_id, 128)?;
    if fence > JSON_SAFE_MAX {
        return Err(InvalidInput("attempt fence exceeds integer contract").into());
    }
    clock(now)?;
    let encoded = sanitize(detail)?;
    let known: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM runner_dispatches WHERE id=?1)",
        [dispatch_id],
        |r| r.get(0),
    )?;
    if !known {
        return Err(Error::NotFound);
    }
    let (count, seq): (u32, u64) = tx.query_row(
        "SELECT COUNT(*),COALESCE(MAX(seq),0)+1 FROM runner_attempt_events WHERE dispatch_id=?1 AND fence=?2",
        params![dispatch_id, fence],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if count >= EVENT_LIMIT {
        return Err(Error::Capacity);
    }
    tx.execute(
        "INSERT INTO runner_attempt_events(dispatch_id,fence,seq,at_ms,phase,detail) VALUES(?1,?2,?3,?4,?5,?6)",
        params![dispatch_id, fence, seq, now, phase.as_str(), encoded],
    )?;
    Ok(seq)
}

impl DomainRepository {
    /// Host observation, best effort: its own savepoint inside its own
    /// transaction, released on success and rolled back on failure, so a
    /// refused detail leaves nothing behind and the next observation on the
    /// same attempt is unaffected. The caller counts a refusal; it never
    /// retries it and never lets it change the attempt's outcome.
    pub fn record_attempt_event(&mut self, event: &AttemptEvent, now: u64) -> Result<u64, Error> {
        let language = self.notice_language;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch("SAVEPOINT attempt_event")?;
        let seq = match insert(
            &tx,
            &event.dispatch_id,
            event.fence,
            event.phase,
            &event.detail,
            now,
        ) {
            Ok(seq) => {
                tx.execute_batch("RELEASE attempt_event")?;
                seq
            }
            Err(error) => {
                tx.execute_batch("ROLLBACK TO attempt_event; RELEASE attempt_event")?;
                return Err(error);
            }
        };
        // The activity projection rides the same transaction the TS
        // lifecycle hooks do (store.ts:2004/2068/2252/2282/2644: each
        // transition calls updateActivity inside its own store
        // transaction), under its OWN savepoint: a refused activity (no
        // task, no addressed root) leaves the event observation intact —
        // the same best-effort contract the event itself carries.
        let activity_event = match event.phase {
            // `Claimed` opens the activity row at the attempt's start. TS posts
            // the `started` notice from the dispatch transition itself
            // (`acknowledgeRunnerEffect`, store.ts:2004), not from a runner
            // phase, and its 1 s outbox timer (bridge-matrix.js:4553) sends it
            // while the turn runs. Opening it here lets the driver deliver it
            // BEFORE the operation exists (no contention for the agent's own
            // Matrix client), so it is the delivered anchor every later
            // revision edits. Observed before: the only drain ran at the poll
            // boundary BEFORE the claim, so `initialized`'s revision stayed
            // `pending` and the terminal revision superseded it unsent
            // (`update_and_enqueue`, activity.rs:345) — only the final ✅ ever
            // went out, with no anchor, hence never an `m.replace` edit.
            AttemptPhase::Claimed | AttemptPhase::Initialized | AttemptPhase::TurnStarted => {
                Some(super::activity::ActivityEvent::Started)
            }
            AttemptPhase::Parked => Some(super::activity::ActivityEvent::Waiting),
            AttemptPhase::Resumed => Some(super::activity::ActivityEvent::Resumed),
            AttemptPhase::Settled => Some(super::activity::ActivityEvent::Completed),
            AttemptPhase::Failed | AttemptPhase::Lost => {
                Some(super::activity::ActivityEvent::Interrupted)
            }
            AttemptPhase::SpawnStarted
            | AttemptPhase::SpawnDone
            | AttemptPhase::OverBudget
            | AttemptPhase::ApprovalRequested
            | AttemptPhase::ApprovalDecided
            | AttemptPhase::StopRequested
            | AttemptPhase::StopReported => None,
        };
        if let Some(activity) = activity_event {
            tx.execute_batch("SAVEPOINT activity_notice")?;
            match super::activity::update_and_enqueue(
                &tx,
                language,
                &event.dispatch_id,
                &activity,
                now,
            ) {
                Ok(_) => tx.execute_batch("RELEASE activity_notice")?,
                Err(_) => {
                    tx.execute_batch("ROLLBACK TO activity_notice; RELEASE activity_notice")?;
                }
            }
        }
        tx.commit()?;
        Ok(seq)
    }
    /// ADR-183 decision D: the one thread message an over-budget turn earns.
    /// Queued once per task, for a verified session only, rooted at the
    /// request the dispatch was answering — the same route and custody as
    /// `outcome_unknown_notice`, so the driver's existing notice delivery
    /// posts it. A dispatch with no task, no addressed request or no Matrix
    /// route has nowhere to say it and says nothing (`NoThread`). Its own
    /// transaction; the host records what it found and never lets a refusal
    /// change the turn.
    pub fn queue_over_budget_notice(
        &mut self,
        dispatch_id: &str,
        elapsed_ms: u64,
        now: u64,
    ) -> Result<OverBudgetNotice, Error> {
        identifier(dispatch_id, 128)?;
        clock(now)?;
        let language = self.notice_language;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let bound: Option<(Option<String>, String, bool)> = tx
            .query_row(
                "SELECT d.task_id,d.session_id,s.matrix_generation>0 FROM runner_dispatches d JOIN runner_sessions s ON s.id=d.session_id WHERE d.id=?1",
                [dispatch_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (task_id, session) = match bound {
            None => return Err(Error::NotFound),
            Some((Some(task_id), session, true)) => (task_id, session),
            Some(_) => return Ok(OverBudgetNotice::NoThread),
        };
        let root: Option<u64> = tx.query_row(
            "SELECT MAX(message_sequence) FROM dispatch_inputs WHERE dispatch_id=?1 AND addressed=1",
            [dispatch_id],
            |r| r.get(0),
        )?;
        let Some(root) = root else {
            return Ok(OverBudgetNotice::NoThread);
        };
        let existing: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM task_notices WHERE task_id=?1 AND json_extract(config,'$.kind')=?2)",
            params![task_id, OVER_BUDGET_NOTICE_KIND],
            |r| r.get(0),
        )?;
        if existing {
            return Ok(OverBudgetNotice::AlreadyQueued);
        }
        let task = super::execution::task(&tx, &task_id)?;
        let root = super::verified_ingress::input_message(&tx, &session, root)?;
        super::task_intents::add_notice(
            &tx,
            &task,
            &root,
            OVER_BUDGET_NOTICE_KIND,
            super::notice_text::over_budget_notice_body(language, elapsed_ms),
            now,
        )?;
        tx.commit()?;
        Ok(OverBudgetNotice::Queued)
    }
    /// The attempt's event log in visit order. Operator-private evidence;
    /// no runtime route reads it.
    pub fn attempt_events(
        &self,
        dispatch_id: &str,
        fence: u64,
    ) -> Result<Vec<AttemptEventRow>, Error> {
        identifier(dispatch_id, 128)?;
        self.db
            .prepare("SELECT seq,at_ms,phase,detail FROM runner_attempt_events WHERE dispatch_id=?1 AND fence=?2 ORDER BY seq")?
            .query_map(params![dispatch_id, fence], |r| {
                Ok((
                    r.get::<_, u64>(0)?,
                    r.get::<_, u64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .map(|row| {
                let (seq, at_ms, phase, detail) = row?;
                // The 039 CHECK admits only the fifteen words; a row outside
                // them is a schema fault, not a phase.
                let phase = AttemptPhase::parse(&phase).ok_or(Error::Schema)?;
                Ok(AttemptEventRow {
                    seq,
                    at_ms,
                    phase,
                    detail: serde_json::from_str(&detail)?,
                })
            })
            .collect()
    }
    /// One clock column on the attempt row. `Started`, `Parked` and `Settled`
    /// keep their first value (idempotent under the host's repeats);
    /// `LastRenew` always overwrites. `NotFound` when no attempt row exists
    /// for the fence: the clock never invents an attempt.
    pub fn set_attempt_clock(
        &mut self,
        dispatch_id: &str,
        fence: u64,
        clock: AttemptClock,
        at_ms: u64,
    ) -> Result<(), Error> {
        identifier(dispatch_id, 128)?;
        hagency_core::tasks::clock(at_ms)?;
        let column = clock.column();
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        attempt_exists(&tx, dispatch_id, fence)?;
        let statement = if clock == AttemptClock::LastRenew {
            format!("UPDATE runner_attempts SET {column}=?3 WHERE dispatch_id=?1 AND fence=?2")
        } else {
            format!(
                "UPDATE runner_attempts SET {column}=?3 WHERE dispatch_id=?1 AND fence=?2 AND {column} IS NULL"
            )
        };
        tx.execute(&statement, params![dispatch_id, fence, at_ms])?;
        tx.commit()?;
        Ok(())
    }
    /// Written once, at settlement or failure: the first writer wins and a
    /// later reason is dropped, not merged. Control characters are replaced
    /// and the text is cut to 640 bytes — the retained product's shape, and
    /// the only free text the private store admits (ADR-181 point 2).
    pub fn set_attempt_terminal_reason(
        &mut self,
        dispatch_id: &str,
        fence: u64,
        reason: &str,
    ) -> Result<(), Error> {
        identifier(dispatch_id, 128)?;
        let reason = bounded_text(reason, TERMINAL_REASON_LIMIT);
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        attempt_exists(&tx, dispatch_id, fence)?;
        tx.execute(
            "UPDATE runner_attempts SET terminal_reason=?3 WHERE dispatch_id=?1 AND fence=?2 AND terminal_reason IS NULL",
            params![dispatch_id, fence, reason],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// The attempt row's clock and terminal reason; `NotFound` when the
    /// fence never had an attempt.
    pub fn attempt_clock(&self, dispatch_id: &str, fence: u64) -> Result<AttemptClockRow, Error> {
        identifier(dispatch_id, 128)?;
        self.db
            .query_row(
                "SELECT started_at,parked_at,last_renew_at,settled_at,terminal_reason FROM runner_attempts WHERE dispatch_id=?1 AND fence=?2",
                params![dispatch_id, fence],
                |r| {
                    Ok(AttemptClockRow {
                        started_at: r.get(0)?,
                        parked_at: r.get(1)?,
                        last_renew_at: r.get(2)?,
                        settled_at: r.get(3)?,
                        terminal_reason: r.get(4)?,
                    })
                },
            )
            .optional()?
            .ok_or(Error::NotFound)
    }
}
fn attempt_exists(tx: &Transaction<'_>, dispatch_id: &str, fence: u64) -> Result<(), Error> {
    let known: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM runner_attempts WHERE dispatch_id=?1 AND fence=?2)",
        params![dispatch_id, fence],
        |r| r.get(0),
    )?;
    if known { Ok(()) } else { Err(Error::NotFound) }
}
