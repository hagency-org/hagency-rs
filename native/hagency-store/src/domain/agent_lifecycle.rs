//! Operator agent lifecycle (#21): the durable stop fence the retained
//! product called `manualDown`, the start that re-arms serving, and the
//! preset (resource) rebind. TS parity: backend-v2.js:12577-12708 (stop),
//! :12712-12775 (start), :11484-11522 (preset).
use super::{DomainRepository, conversation_lifecycle};
use crate::Error;
use hagency_core::{
    project::{identifier, public_resource_id},
    tasks::clock,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde_json::json;

/// The engagement's live dispatch ids, oldest first — the set the retained
/// stop route cancelled before it declared the agent stopped.
fn live_dispatches(tx: &rusqlite::Transaction<'_>, engagement: &str) -> Result<Vec<String>, Error> {
    let mut stmt = tx.prepare(
        "SELECT d.id FROM runner_dispatches d JOIN runner_sessions s ON s.id=d.session_id \
         WHERE s.engagement_id=?1 AND d.state IN ('queued','leased','started','parked') \
         ORDER BY d.id",
    )?;
    let rows = stmt
        .query_map([engagement], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

impl DomainRepository {
    /// Stop — TS parity `stopManagedAgent` (backend-v2.js:12577): fence every
    /// live dispatch (the retained `cancelledDispatches`), settle the stop
    /// rows this host can prove, clear the agent fences the operator just
    /// resolved, and record the durable stopped row. Idempotent: a second
    /// call finds no live work and answers `stopped: true` again.
    pub fn stop_agent(
        &mut self,
        engagement: &str,
        operator: &str,
        now: u64,
    ) -> Result<serde_json::Value, Error> {
        identifier(engagement, 128)?;
        hagency_core::tasks::text(operator, 128)?;
        clock(now)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = stop_in_transaction(&tx, engagement, operator, now)?;
        tx.commit()?;
        Ok(result)
    }

    /// Start — TS parity backend-v2.js:12712: refuses while lifecycle cleanup
    /// is pending (409 `agent_lifecycle_busy`), refuses an agent that never
    /// stopped (the retained `agent already online`), and otherwise records
    /// the return to serving.
    pub fn start_agent(&mut self, engagement: &str, now: u64) -> Result<(), Error> {
        identifier(engagement, 128)?;
        clock(now)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        start_in_transaction(&tx, engagement, now)?;
        tx.commit()?;
        Ok(())
    }

    /// Preset (resource) rebind — TS parity `PUT /api/agents/:name/preset`
    /// (backend-v2.js:11484): the binding, the ceiling and the profile move
    /// together, so the next dispatch the host claims runs on the new
    /// resource. Native has no agent record outside its engagement, so the
    /// rebind rewrites the engagement's durable association (row, projection
    /// and the provision effect's resource payload) in one transaction.
    /// Unbinding is refused: a native engagement cannot exist without a
    /// resource (`resource_id NOT NULL`).
    pub fn rebind_agent_resource(
        &mut self,
        engagement: &str,
        preset: &str,
        now: u64,
    ) -> Result<serde_json::Value, Error> {
        identifier(engagement, 128)?;
        identifier(preset, 128)?;
        clock(now)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (resource_id, previous): (String, String) = tx
            .query_row(
                "SELECT resource_id,COALESCE(preset_id,'') FROM engagements WHERE id=?1",
                [engagement],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        let resource = super::read_resource(&tx, &public_resource_id(preset))
            .map_err(|_| Error::Invalid(hagency_core::InvalidInput("unknown preset")))?;
        let report = super::usage::ceiling_report(&tx, &resource.id(), now)?;
        let remaining = report
            .ceiling_tokens
            .map(|ceiling| ceiling.saturating_sub(report.drawn));
        let response = json!({
            "ok": true,
            "engagement_id": engagement,
            "preset_id": resource.preset_id,
            "resource_id": resource.id(),
            "previous_preset_id": previous,
            "ceiling_tokens": report.ceiling_tokens,
            "remaining": remaining,
            "tier": tier_word(&resource),
        });
        if resource.id() == resource_id {
            // Same binding replayed: the retained route is idempotent here.
            tx.commit()?;
            return Ok(response);
        }
        tx.execute(
            "UPDATE engagements SET resource_id=?2,preset_id=?3,seat_id=?4 WHERE id=?1",
            params![
                engagement,
                resource.id(),
                resource.preset_id,
                resource.seat_id
            ],
        )?;
        // `json()` takes JSON text — a bare id string is malformed JSON.
        tx.execute(
            "UPDATE engagements SET projection=json_set(projection,'$.resourceId',json(?2)) WHERE id=?1",
            params![engagement, serde_json::json!(resource.id()).to_string()],
        )?;
        // The claim selector's account gate reads this payload's resource —
        // the single place "the next dispatch uses it" is decided.
        tx.execute(
            "UPDATE effects SET payload=json_set(payload,'$.resource',json(?2)) \
             WHERE engagement_id=?1 AND kind='provision'",
            params![engagement, serde_json::to_string(&resource)?],
        )?;
        tx.commit()?;
        Ok(response)
    }
}

/// The policy tier word beside the budget, as the retained response carried
/// `tier` (backend-v2.js:11524).
fn tier_word(resource: &hagency_core::project::Resource) -> serde_json::Value {
    hagency_core::qualification::model(&resource.profile())
        .0
        .and_then(|tier| serde_json::to_value(tier).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// A scoped owner pause fences work but cannot resolve unknown runtime cleanup
/// or clear an operator inspection fence. Resume still checks those receipts.
pub(super) fn pause_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    engagement: &str,
    actor: &str,
    now: u64,
) -> Result<(), Error> {
    for dispatch in live_dispatches(tx, engagement)? {
        conversation_lifecycle::fence_dispatch(tx, &dispatch, engagement, now)?;
    }
    tx.execute("INSERT INTO agent_lifecycle(engagement_id,stopped_at,reason,operator,started_at) VALUES(?1,?2,'operator-stop-requested',?3,NULL) ON CONFLICT(engagement_id) DO UPDATE SET stopped_at=excluded.stopped_at,reason=excluded.reason,operator=excluded.operator,started_at=NULL",params![engagement,now,actor])?;
    Ok(())
}

pub(super) fn stop_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    engagement: &str,
    operator: &str,
    now: u64,
) -> Result<serde_json::Value, Error> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM engagements WHERE id=?1)",
        [engagement],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(Error::NotFound);
    }
    let mut dispatches = live_dispatches(tx, engagement)?;
    // A stop row the fence-only route left unsettled is resolved by this
    // operator stop too — the retained route waited for the same cleanup.
    let mut legacy: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT s.dispatch_id FROM dispatch_stops s \
                 JOIN runner_dispatches d ON d.id=s.dispatch_id \
                 JOIN runner_sessions n ON n.id=d.session_id \
                 WHERE n.engagement_id=?1 AND s.settled_at IS NULL ORDER BY s.dispatch_id",
        )?;

        stmt.query_map([engagement], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    for dispatch in &legacy {
        if !dispatches.contains(dispatch) {
            dispatches.push(dispatch.clone());
        }
    }
    legacy.clear();
    for dispatch in &dispatches {
        conversation_lifecycle::fence_dispatch(tx, dispatch, engagement, now)?;
        // A dispatch the fence left uncertain is settled by this host's
        // own stop decision — the retained route's confirmed cleanup.
        let uncertain: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM runner_dispatches WHERE id=?1 AND state='outcome_unknown')",
                [dispatch],
                |r| r.get(0),
            )?;
        if uncertain {
            let fence: u64 = tx.query_row(
                "SELECT fence FROM runner_dispatches WHERE id=?1",
                [dispatch],
                |r| r.get(0),
            )?;
            let evidence = format!("operator stop of {engagement} at {now}");
            conversation_lifecycle::settle_stop_in_transaction(
                tx, dispatch, fence, &evidence, now, true,
            )
            .or_else(|error| match error {
                Error::State | Error::Conflict | Error::RunnerAuthority => Ok(()),
                other => Err(other),
            })?;
        }
    }
    // The operator's stop IS the resolution ADR-182 waits for.
    tx.execute(
        "UPDATE agent_fences SET cleared_at=?2,cleared_by='operator-stop' \
             WHERE engagement_id=?1 AND cleared_at IS NULL",
        params![engagement, now],
    )?;
    tx.execute(
        "INSERT INTO agent_lifecycle(engagement_id,stopped_at,reason,operator,started_at) \
             VALUES(?1,?2,'operator-stopped',?3,NULL) \
             ON CONFLICT(engagement_id) DO UPDATE SET stopped_at=excluded.stopped_at,\
             reason='operator-stopped',operator=excluded.operator,started_at=NULL",
        params![engagement, now, operator],
    )?;
    Ok(json!({
        "ok": true,
        "stopped": true,
        "engagement_id": engagement,
        "cancelled_dispatches": dispatches,
        "state": "stopped",
    }))
}

pub(super) fn start_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    engagement: &str,
    now: u64,
) -> Result<(), Error> {
    let stopped: Option<u64> = tx
        .query_row(
            "SELECT stopped_at FROM agent_lifecycle WHERE engagement_id=?1 AND started_at IS NULL",
            [engagement],
            |r| r.get(0),
        )
        .optional()?;
    if stopped.is_none() {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM engagements WHERE id=?1)",
            [engagement],
            |r| r.get(0),
        )?;
        return Err(if exists {
            Error::Conflict
        } else {
            Error::NotFound
        });
    }
    let pending: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM dispatch_stops s JOIN runner_dispatches d ON d.id=s.dispatch_id \
             JOIN runner_sessions n ON n.id=d.session_id \
             WHERE n.engagement_id=?1 AND s.settled_at IS NULL) \
             OR EXISTS(SELECT 1 FROM agent_fences WHERE engagement_id=?1 AND cleared_at IS NULL)",
            [engagement],
            |r| r.get(0),
        )?;
    if pending {
        return Err(Error::State);
    }
    tx.execute(
        "UPDATE agent_lifecycle SET started_at=?2 WHERE engagement_id=?1",
        params![engagement, now],
    )?;

    Ok(())
}
