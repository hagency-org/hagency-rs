//! Custody for a host-authored answer to a `!` command line.
//!
//! The sender is this agent, which is a member of the room that carried the
//! line; the answer is an `m.notice` in the same thread the retained bridge
//! used (`lib/bot-commands.js` `reply`/`sendInto`). There is no task and no
//! owner approval behind it — the command layer renders it — so it borrows the
//! custody DISCIPLINE of `final_replies`/`task_notices` (claim, one-shot begin,
//! validate before every write, honestly-inspectable uncertain) without
//! borrowing either table, whose rows would have to lie about a task.
use super::{DomainRepository, bounded_row, execution, matrix_routes, serialize};
use crate::Error;
use hagency_core::{
    JSON_SAFE_MAX, canonical,
    commands::{
        CommandNotice, CommandNoticeClaim, CommandNoticeClaimed, CommandNoticeReceipt,
        CommandNoticeRequest, CommandNoticeSend,
    },
    project::identifier,
    replies::{ReplyDeliveryObservation, ReplyReconciliation, ReplyRoute, generation},
    tasks::{clock, text},
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::json;

fn receipt(db: &Connection, id: &str, replayed: bool) -> Result<CommandNoticeReceipt, Error> {
    db.query_row(
        "SELECT session_id,state,fence,cancel_requested FROM command_notices WHERE id=?1",
        [id],
        |r| {
            Ok(CommandNoticeReceipt {
                id: id.into(),
                session_id: r.get(0)?,
                state: r.get(1)?,
                fence: r.get(2)?,
                cancel_requested: r.get(3)?,
                replayed,
            })
        },
    )
    .optional()?
    .ok_or(Error::NotFound)
}
fn frozen(db: &Connection, id: &str) -> Result<(CommandNotice, ReplyRoute, String), Error> {
    #[allow(clippy::type_complexity)]
    let row: Option<(String, String, String, Option<String>, String, String, String)> = db
        .query_row(
            "SELECT session_id,transaction_id,body,html,route,digest,source_event_id FROM command_notices WHERE id=?1",
            [id],
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
        .optional()?;
    let (session_id, transaction_id, body, html, route, digest, source_event_id) =
        row.ok_or(Error::NotFound)?;
    Ok((
        CommandNotice {
            id: id.into(),
            session_id,
            transaction_id,
            body,
            html,
            source_event_id,
        },
        serde_json::from_str(&route)?,
        digest,
    ))
}
/// A command answer lives exactly as long as its session still has a current
/// route AND that route is the one the answer was frozen against: a room
/// generation that moved retires it rather than delivering into a room the
/// agent may have left.
fn current(db: &Connection, id: &str) -> Result<bool, Error> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM current_command_notices WHERE id=?1)",
        [id],
        |r| r.get(0),
    )?)
}
fn check_claim(db: &Connection, id: &str, token: &str, now: u64) -> Result<String, Error> {
    identifier(id, 128)?;
    text(token, 128)?;
    clock(now)?;
    let row: Option<(String, Option<String>, Option<u64>)> = db
        .query_row(
            "SELECT state,claim_hash,claim_until FROM command_notices WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (state, hash, until) = row.ok_or(Error::RunnerAuthority)?;
    if !execution::matches_secret(hash.as_deref().unwrap_or(""), token)?
        || (state != "delivered" && until.is_none_or(|until| until <= now))
    {
        return Err(Error::RunnerAuthority);
    }
    Ok(state)
}
fn observed(db: &Connection, id: &str, input: &ReplyDeliveryObservation) -> Result<String, Error> {
    input.validate()?;
    let (notice, route, digest) = frozen(db, id)?;
    if input.transaction_id != notice.transaction_id
        || input.digest != digest
        || input.server_name != route.server_name
        || input.room_id != route.room_id
        || input.sender_mxid != route.sender_mxid
        || input.device_id != route.device_id
        || (route.encrypted && !input.encrypted)
    {
        return Err(Error::RunnerAuthority);
    }
    serialize(input)
}
pub(super) fn reconcile(tx: &Transaction<'_>, now: u64, restart: bool) -> Result<(), Error> {
    matrix_routes::reconcile(tx, now)?;
    tx.execute("UPDATE command_notices SET state=CASE WHEN state='sending' THEN 'uncertain' ELSE 'pending' END,claim_hash=NULL,claim_until=NULL,updated_at=?1 WHERE state IN ('claimed','sending') AND (?2 OR claim_until<=?1)",params![now,restart])?;
    Ok(())
}
impl DomainRepository {
    /// Host-only intent to answer one admitted `!` line. The id is derived from
    /// the session and the source event, so answering the same line twice
    /// converges on the same answer instead of sending a second one.
    pub fn submit_command_notice(
        &mut self,
        input: &CommandNoticeRequest,
        now: u64,
    ) -> Result<CommandNoticeReceipt, Error> {
        input.validate()?;
        clock(now)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        matrix_routes::reconcile(&tx, now)?;
        matrix_routes::check(&tx, &input.session_id)?;
        let route = matrix_routes::route(&tx, &input.session_id)?;
        let id = format!(
            "cmdn_{}",
            canonical::digest(&json!([input.session_id, input.source_event_id]))?
        );
        let digest = canonical::digest(&json!([
            "command_notice",
            input.session_id,
            input.source_event_id,
            input.body,
            input.html
        ]))?;
        let prior: Option<String> = tx
            .query_row(
                "SELECT digest FROM command_notices WHERE id=?1",
                [&id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = prior {
            if old != digest {
                return Err(Error::Conflict);
            }
            let result = receipt(&tx, &id, true)?;
            tx.commit()?;
            return Ok(result);
        }
        // ONE EVENT, ONE ANSWER. The retained bridge deduplicated a command by
        // its Matrix event id (`isDuplicateMatrixEvent`, bridge-matrix.js:4117)
        // in ONE process, so a `!help` several agents share a room for was
        // answered once. Native runs one driver per agent but they share this
        // store, so the event id is where the same rule is enforced across
        // processes: an answer already queued by ANY session owns the event,
        // and the late session returns that winner's receipt instead of queuing
        // a second reply. The claim, not each agent's own id, decides.
        let winner: Option<String> = tx
            .query_row(
                "SELECT id FROM command_notices WHERE source_event_id=?1 ORDER BY rowid LIMIT 1",
                [&input.source_event_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(winner) = winner {
            let result = receipt(&tx, &winner, true)?;
            tx.commit()?;
            return Ok(result);
        }
        bounded_row(&tx, "command_notices", "id", &id, 100_000)?;
        tx.execute("INSERT INTO command_notices(id,session_id,transaction_id,digest,body,html,route,source_event_id,state,created_at,updated_at) VALUES(?1,?2,?1,?3,?4,?5,?6,?7,'pending',?8,?8)",params![id,input.session_id,digest,input.body,input.html,serialize(&route)?,input.source_event_id,now])?;
        let result = receipt(&tx, &id, false)?;
        tx.commit()?;
        Ok(result)
    }
    /// Claim the next pending answer for this session. `None` when there is
    /// nothing to say — never a claim that authorizes nothing.
    pub fn claim_command_notice_for_session(
        &mut self,
        session: &str,
        now: u64,
        lease_ms: u64,
    ) -> Result<Option<CommandNoticeClaimed>, Error> {
        clock(now)?;
        if !(1..=60_000).contains(&lease_ms) || now > JSON_SAFE_MAX - lease_ms {
            return Err(hagency_core::InvalidInput("invalid command notice lease").into());
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        reconcile(&tx, now, false)?;
        let id: Option<String> = tx
            .query_row("SELECT n.id FROM command_notices n JOIN current_command_notices c ON c.id=n.id WHERE n.session_id=?1 AND n.state='pending' AND n.cancel_requested=0 ORDER BY n.rowid LIMIT 1",[session],|r|r.get(0))
            .optional()?;
        let Some(id) = id else {
            tx.commit()?;
            return Ok(None);
        };
        let fence: u64 = tx.query_row(
            "SELECT fence+1 FROM command_notices WHERE id=?1",
            [&id],
            |r| r.get(0),
        )?;
        generation(fence)?;
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|_| Error::Unavailable)?;
        let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
        tx.execute("UPDATE command_notices SET state='claimed',fence=?2,claim_hash=?3,claim_until=?4,updated_at=?5 WHERE id=?1",params![id,fence,canonical::digest(&json!(token))?,now+lease_ms,now])?;
        let (notice, route, digest) = frozen(&tx, &id)?;
        tx.commit()?;
        Ok(Some(CommandNoticeClaimed {
            claim: CommandNoticeClaim {
                notice,
                token,
                deadline: now + lease_ms,
            },
            route,
            digest,
        }))
    }
    /// Commit send-start before touching Matrix. Losing this response is
    /// uncertain; the host must inspect rather than execute the same begin twice.
    pub fn begin_command_notice_send(
        &mut self,
        id: &str,
        token: &str,
        now: u64,
    ) -> Result<CommandNoticeSend, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if check_claim(&tx, id, token, now)? != "claimed" || !current(&tx, id)? {
            return Err(Error::RunnerAuthority);
        }
        let (notice, route, digest) = frozen(&tx, id)?;
        let fence: u64 =
            tx.query_row("SELECT fence FROM command_notices WHERE id=?1", [id], |r| {
                r.get(0)
            })?;
        tx.execute(
            "UPDATE command_notices SET state='sending',updated_at=?2 WHERE id=?1",
            params![id, now],
        )?;
        tx.commit()?;
        Ok(CommandNoticeSend {
            notice,
            route,
            digest,
            fence,
        })
    }
    /// Recheck the exact still-current Sending claim immediately before each
    /// host transport write. It is not an atomic fence on a remote homeserver.
    pub fn validate_command_notice_send(
        &self,
        id: &str,
        token: &str,
        fence: u64,
        now: u64,
    ) -> Result<(), Error> {
        if check_claim(&self.db, id, token, now)? != "sending" {
            return Err(Error::State);
        }
        let found: u64 = self
            .db
            .query_row("SELECT fence FROM command_notices WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or(Error::NotFound)?;
        if found != fence || !current(&self.db, id)? {
            return Err(Error::RunnerAuthority);
        }
        Ok(())
    }
    pub fn deliver_command_notice(
        &mut self,
        id: &str,
        token: &str,
        input: &ReplyDeliveryObservation,
        now: u64,
    ) -> Result<CommandNoticeReceipt, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = check_claim(&tx, id, token, now)?;
        let observation = observed(&tx, id, input)?;
        if state == "delivered" {
            let old: Option<String> = tx
                .query_row(
                    "SELECT observation FROM command_notices WHERE id=?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            if old.as_deref() != Some(observation.as_str()) {
                return Err(Error::Conflict);
            }
            return receipt(&tx, id, true);
        }
        if state != "sending" || !current(&tx, id)? {
            return Err(Error::RunnerAuthority);
        }
        tx.execute("UPDATE command_notices SET state='delivered',observation=?2,event_id=?3,claim_hash=NULL,claim_until=NULL,updated_at=?4 WHERE id=?1",params![id,observation,input.event_id,now])?;
        let result = receipt(&tx, id, false)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn command_notice_receipt(&self, id: &str) -> Result<CommandNoticeReceipt, Error> {
        identifier(id, 128)?;
        receipt(&self.db, id, false)
    }
    /// Secret-free read used only by a resumed send (task #9): is this journaled
    /// command answer still authorized to be re-put — still `sending`, same
    /// fence, and not cancelled? False means park it as uncertain for a human
    /// rather than re-sending.
    pub fn command_notice_send_current(&self, id: &str, fence: u64) -> Result<bool, Error> {
        identifier(id, 128)?;
        generation(fence)?;
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM command_notices WHERE id=?1 AND fence=?2 AND state='sending' AND cancel_requested=0)",
            params![id, fence],
            |row| row.get(0),
        )?)
    }
    /// Secret-free read for a resumed send: did the host itself cancel this
    /// undelivered command answer? Its journal slot may then be released
    /// without re-sending it.
    pub fn command_notice_send_cancelled(&self, id: &str, fence: u64) -> Result<bool, Error> {
        identifier(id, 128)?;
        generation(fence)?;
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM command_notices WHERE id=?1 AND fence=?2 AND cancel_requested=1 AND state<>'delivered')",
            params![id, fence],
            |row| row.get(0),
        )?)
    }
    /// The admitted `!` lines in this session that have no answer queued yet,
    /// oldest first. A command is answered once: the answer's id is derived from
    /// the session and the source event, so a line already answered (or being
    /// answered) is not offered again. Files and images are never commands,
    /// exactly as `is_bot_command` decides at intake.
    pub fn pending_command_lines(
        &self,
        session: &str,
        limit: i64,
    ) -> Result<Vec<hagency_core::commands::CommandLine>, Error> {
        identifier(session, 128)?;
        if !(1..=1024).contains(&limit) {
            return Err(hagency_core::InvalidInput("invalid command line limit").into());
        }
        // The limit counts COMMAND lines, not raw inputs. It used to bound
        // every `session_inputs` row of the session and filter for `!` in the
        // Rust loop below, so once a room had more than `limit` admitted rows
        // the newest `!` line fell outside the window and was offered to
        // nobody — board #113: the live room's session held 67 rows, so the
        // operator's `!help` (seq 51) was hidden behind `LIMIT 16`,
        // `command_notices` stayed empty and NO agent answered it.
        //
        // The prefilter tests the MESSAGE BODY, never the whole row: every
        // config carries `room_id` (`!project:…`), so a filter on the encoded
        // config matched all 67 rows and reproduced the bug (measured against
        // the live store). It is a deliberately SUPERSET of the Rust check
        // below (`kind == m.text` && trimmed body starts with `!`) — SQL may
        // be too permissive, never too strict, so the narrowed window can
        // never drop a line the Rust loop would have accepted. Rust stays the
        // authority on what a command is (intake's `is_bot_command`).
        let encoded: Vec<String> = self
            .db
            .prepare("SELECT CASE WHEN s.matrix_generation>0 THEN i.config ELSE m.config END FROM session_inputs i JOIN admitted_messages m ON m.sequence=i.message_sequence JOIN runner_sessions s ON s.id=i.session_id WHERE i.session_id=?1 AND instr(json_extract(CASE WHEN s.matrix_generation>0 THEN i.config ELSE m.config END,'$.body'),'!')>0 ORDER BY i.message_sequence LIMIT ?2")?
            .query_map(params![session, limit], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let mut lines = Vec::new();
        for encoded in encoded {
            let message: hagency_core::messages::Message = serde_json::from_str(&encoded)?;
            // Board #113: every skip says WHY. The live regression left no
            // trace, so a dropped command and a command nobody had admitted
            // yet looked identical in the log.
            if !matches!(message.kind.as_str(), "m.text")
                || !message.body.trim_start().starts_with('!')
            {
                tracing::debug!(
                    session,
                    event_id = %message.event_id,
                    kind = %message.kind,
                    "bot command skipped: not a `!` line"
                );
                continue;
            }
            let id = format!(
                "cmdn_{}",
                canonical::digest(&json!([session, message.event_id]))?
            );
            let answered: bool = self.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM command_notices WHERE id=?1)",
                [&id],
                |r| r.get(0),
            )?;
            if answered {
                tracing::debug!(
                    session,
                    event_id = %message.event_id,
                    "bot command skipped: this session already queued an answer"
                );
                continue;
            }
            // The event id, not this session's copy of it, is what makes a
            // command answered ONCE: if any session already owns an answer for
            // this event, this session stays quiet (see `submit_command_notice`
            // for the whole rule). A line whose answer is the winner's is not
            // this session's to say.
            let owned: bool = self.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM command_notices WHERE source_event_id=?1)",
                [&message.event_id],
                |r| r.get(0),
            )?;
            if owned {
                tracing::debug!(
                    session,
                    event_id = %message.event_id,
                    "bot command skipped: another session already owns the answer"
                );
                continue;
            }
            lines.push(hagency_core::commands::CommandLine {
                session_id: session.to_owned(),
                server_name: message.server_name,
                room_id: message.room_id,
                event_id: message.event_id,
                thread_root: message.thread_root,
                body: message.body,
                sender_mxid: message.sender_mxid,
            });
        }
        Ok(lines)
    }
    /// Historical conflict query only: never supplies acceptance or authority.
    pub fn command_notice_history_conflicts(&self, id: &str, fence: u64) -> Result<bool, Error> {
        identifier(id, 128)?;
        generation(fence)?;
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM command_notices WHERE id=?1 AND (fence!=?2 OR state!='delivered'))",
            params![id, fence], |row| row.get(0))?)
    }
    pub fn cancel_command_notice(
        &mut self,
        id: &str,
        now: u64,
    ) -> Result<CommandNoticeReceipt, Error> {
        identifier(id, 128)?;
        clock(now)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if receipt(&tx, id, false)?.state == "delivered" {
            return Err(Error::State);
        }
        tx.execute("UPDATE command_notices SET cancel_requested=1,state=CASE WHEN state IN ('sending','uncertain') THEN 'uncertain' ELSE 'cancelled' END,claim_hash=NULL,claim_until=NULL,updated_at=?2 WHERE id=?1",params![id,now])?;
        let result = receipt(&tx, id, false)?;
        tx.commit()?;
        Ok(result)
    }
    /// Settle a claimed-but-unfinished answer the host adapter provably never
    /// sent, or one it delivered late. A lost response requires inspection:
    /// `NotSent` is only honest with evidence.
    pub fn reconcile_command_notice(
        &mut self,
        id: &str,
        fence: u64,
        input: &ReplyReconciliation,
        now: u64,
    ) -> Result<CommandNoticeReceipt, Error> {
        identifier(id, 128)?;
        generation(fence)?;
        clock(now)?;
        match input {
            ReplyReconciliation::Delivered(v) => v.validate()?,
            ReplyReconciliation::NotSent { evidence } => text(evidence, 4000)?,
        };
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        reconcile(&tx, now, false)?;
        let digest = canonical::digest(&json!([id, fence, input]))?;
        let prior: Option<String> = tx
            .query_row(
                "SELECT digest FROM command_notice_inspections WHERE notice_id=?1 AND fence=?2",
                params![id, fence],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(prior) = prior {
            if prior != digest {
                return Err(Error::Conflict);
            }
            return receipt(&tx, id, true);
        }
        let before = receipt(&tx, id, false)?;
        if (before.state != "uncertain"
            && !(before.state == "sending" && matches!(input, ReplyReconciliation::Delivered(_))))
            || before.fence != fence
        {
            return Err(Error::RunnerAuthority);
        }
        let count: u64 =
            tx.query_row("SELECT COUNT(*) FROM command_notice_inspections", [], |r| {
                r.get(0)
            })?;
        let own: u64 = tx.query_row(
            "SELECT COUNT(*) FROM command_notice_inspections WHERE notice_id=?1",
            [id],
            |r| r.get(0),
        )?;
        if count >= 100_000 || own >= 32 {
            return Err(Error::Capacity);
        }
        match input {
            ReplyReconciliation::Delivered(observation) => {
                let observed = observed(&tx, id, observation)?;
                tx.execute("UPDATE command_notices SET state='delivered',observation=?2,event_id=?3,claim_hash=NULL,claim_until=NULL,updated_at=?4 WHERE id=?1",params![id,observed,observation.event_id,now])?;
            }
            ReplyReconciliation::NotSent { .. } => {
                tx.execute("UPDATE command_notices SET state=CASE WHEN ?3 THEN 'pending' ELSE 'cancelled' END,claim_hash=NULL,claim_until=NULL,updated_at=?2 WHERE id=?1",params![id,now,current(&tx,id)?])?;
            }
        }
        tx.execute("INSERT INTO command_notice_inspections(notice_id,fence,digest,observation) VALUES(?1,?2,?3,?4)",params![id,fence,digest,serialize(input)?])?;
        let result = receipt(&tx, id, false)?;
        tx.commit()?;
        Ok(result)
    }
}
