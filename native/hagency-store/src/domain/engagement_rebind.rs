//! ADR-197: guarded per-engagement rebind to a new runtime package.
//!
//! A rebind rewrites only the frozen `resource.runtimePlugin` of the
//! engagement's provision effect, in one immediate store transaction that also
//! checks the idle boundary: no leased/started/parked dispatch, no unresolved
//! dispatch, no open agent fence and no pending outgoing custody. It never
//! cancels, redoes or re-fences work; a busy engagement is refused with a
//! named reason and stays `upgrade_pending`. Identity (engagement id, Matrix
//! identity, rooms, workspace, allocation, budget) is not touched.
use super::DomainRepository;
use super::engagement_runtime_upgrade::UpgradeState;
use crate::Error;
use hagency_core::{
    InvalidInput,
    project::{Resource, RuntimePluginIdentity, identifier},
    tasks::clock,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;

/// Why a rebind was refused. The word is stored in the audit record as
/// `refused:<word>` and shown to the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RebindRefusal {
    NotMarked,
    Pinned,
    RolloutHalted,
    CanaryPending,
    EngagementNotActive,
    AgentFenced,
    DispatchLeased,
    DispatchStarted,
    DispatchParked,
    UnresolvedDispatch,
    OutgoingCustodyPending,
    PluginMismatch,
    Unqualified,
    TargetNotLaunchable,
    NoPrevious,
}
impl RebindRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotMarked => "not_marked",
            Self::Pinned => "pinned",
            Self::RolloutHalted => "rollout_halted",
            Self::CanaryPending => "canary_pending",
            Self::EngagementNotActive => "engagement_not_active",
            Self::AgentFenced => "agent_fenced",
            Self::DispatchLeased => "dispatch_leased",
            Self::DispatchStarted => "dispatch_started",
            Self::DispatchParked => "dispatch_parked",
            Self::UnresolvedDispatch => "unresolved_dispatch",
            Self::OutgoingCustodyPending => "outgoing_custody_pending",
            Self::PluginMismatch => "plugin_mismatch",
            Self::Unqualified => "unqualified",
            Self::TargetNotLaunchable => "target_not_launchable",
            Self::NoPrevious => "no_previous",
        }
    }
    /// Refusals that only mean "not idle yet": a rollout skips the engagement
    /// and retries later.
    fn is_busy(self) -> bool {
        matches!(
            self,
            Self::AgentFenced
                | Self::DispatchLeased
                | Self::DispatchStarted
                | Self::DispatchParked
                | Self::UnresolvedDispatch
                | Self::OutgoingCustodyPending
        )
    }
}

/// Whether the plugin registry can still launch a package for a framework.
/// The store cannot see the registry; the host answers, and a rebind to a
/// package it cannot launch is refused (ADR-197).
pub type Launchable<'a> = &'a dyn Fn(&RuntimePluginIdentity, &str) -> bool;

/// One row of the append-only audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRebindAudit {
    pub id: u64,
    pub engagement_id: String,
    pub resource_id: String,
    /// `rebind`, `rollback`, `pin`, `unpin` or `rollout`.
    pub action: String,
    pub old_package_sha256: Option<String>,
    pub new_package_sha256: Option<String>,
    pub old_settings_sha256: Option<String>,
    pub new_settings_sha256: Option<String>,
    pub actor: String,
    pub at: u64,
    /// `applied`, `rolled_back`, `pinned`, `unpinned`, `halted` or `refused:<reason>`.
    pub outcome: String,
    pub detail: Option<String>,
}

/// Result of one rebind or rollback attempt. A refusal is a committed audit
/// record, not an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RebindResult {
    pub engagement_id: String,
    pub applied: bool,
    pub refusal: Option<RebindRefusal>,
    pub audit: RuntimeRebindAudit,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RolloutBlocker {
    pub engagement_id: String,
    pub reason: RebindRefusal,
}

/// One step of a rolling rollout.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum RolloutStep {
    /// One engagement was rebound; `canary` is true for the first.
    #[serde(rename_all = "camelCase")]
    Rebound { engagement_id: String, canary: bool },
    /// The canary has not passed yet; nothing else is rebound.
    #[serde(rename_all = "camelCase")]
    AwaitingCanary { engagement_id: String },
    /// No pending engagement could be rebound now; each blocker is named.
    Blocked { blockers: Vec<RolloutBlocker> },
    /// The canary failed or was rolled back; the rollout is stopped.
    Halted,
    /// No `upgrade_pending` engagement is left (pinned ones are skipped).
    Complete,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RolloutPending {
    pub engagement_id: String,
    pub state: UpgradeState,
    /// The blocker a rebind would meet right now, if any.
    pub blocker: Option<RebindRefusal>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RolloutStatus {
    pub resource_id: String,
    /// `none`, `running`, `halted` or `complete`.
    pub state: String,
    pub target_runtime: Option<RuntimePluginIdentity>,
    pub canary_engagement_id: Option<String>,
    /// `passed`, `failed` or absent while the canary is still being observed.
    pub canary_result: Option<String>,
    pub detail: Option<String>,
    pub pending: Vec<RolloutPending>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngagementUpgradeStatus {
    pub engagement_id: String,
    pub resource_id: String,
    pub current_runtime: Option<RuntimePluginIdentity>,
    pub target_runtime: Option<RuntimePluginIdentity>,
    /// `upgrade_pending`, `pinned`, `halted`, or absent when not marked.
    pub state: Option<UpgradeState>,
    /// What a rebind would be refused for right now.
    pub blocker: Option<RebindRefusal>,
    /// The one-step rollback target, when one exists.
    pub previous_runtime: Option<RuntimePluginIdentity>,
    pub audit: Vec<RuntimeRebindAudit>,
}

struct Frozen {
    resource_id: String,
    role: String,
    state: String,
    resource: Option<Resource>,
}
impl Frozen {
    fn runtime(&self) -> Option<&RuntimePluginIdentity> {
        self.resource.as_ref()?.runtime_plugin.as_ref()
    }
}

fn read_frozen(tx: &Connection, id: &str) -> Result<Frozen, Error> {
    let row: Option<(String, Option<String>, String, Option<String>)> = tx
        .query_row(
            "SELECT e.resource_id,json_extract(e.context,'$.role'),e.state,\
             (SELECT json_extract(f.payload,'$.resource') FROM effects f \
              WHERE f.engagement_id=e.id AND f.kind='provision' AND f.state='complete') \
             FROM engagements e WHERE e.id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let (resource_id, role, state, resource) = row.ok_or(Error::NotFound)?;
    Ok(Frozen {
        resource_id,
        role: role.unwrap_or_default(),
        state,
        resource: resource.map(|v| serde_json::from_str(&v)).transpose()?,
    })
}

fn count(tx: &Connection, sql: &str, id: &str) -> Result<u64, Error> {
    Ok(tx.query_row(sql, [id], |r| r.get(0))?)
}

/// The idle boundary. `None` means idle.
fn idle_blocker(tx: &Connection, id: &str) -> Result<Option<RebindRefusal>, Error> {
    if count(
        tx,
        "SELECT COUNT(*) FROM agent_fences WHERE engagement_id=?1 AND cleared_at IS NULL",
        id,
    )? != 0
    {
        return Ok(Some(RebindRefusal::AgentFenced));
    }
    for (state, refusal) in [
        ("leased", RebindRefusal::DispatchLeased),
        ("started", RebindRefusal::DispatchStarted),
        ("parked", RebindRefusal::DispatchParked),
    ] {
        let busy: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runner_dispatches d JOIN runner_sessions s ON s.id=d.session_id \
             WHERE s.engagement_id=?1 AND d.state=?2)",
            params![id, state],
            |r| r.get(0),
        )?;
        if busy {
            return Ok(Some(refusal));
        }
    }
    if count(
        tx,
        "SELECT COUNT(*) FROM unresolved_dispatches u JOIN runner_sessions s ON s.id=u.session_id WHERE s.engagement_id=?1",
        id,
    )? != 0
    {
        return Ok(Some(RebindRefusal::UnresolvedDispatch));
    }
    // Outgoing custody held in the store: final replies, command notices and
    // task notices that are not yet delivered or cancelled.
    for sql in [
        "SELECT COUNT(*) FROM final_replies f JOIN runner_sessions s ON s.id=f.session_id \
         WHERE s.engagement_id=?1 AND f.state IN ('pending','claimed','sending','uncertain')",
        "SELECT COUNT(*) FROM command_notices n JOIN runner_sessions s ON s.id=n.session_id \
         WHERE s.engagement_id=?1 AND n.state IN ('pending','claimed','sending','uncertain')",
        "SELECT COUNT(*) FROM task_notices n JOIN canonical_tasks t ON t.id=n.task_id \
         JOIN runner_sessions s ON s.id=t.session_id \
         WHERE s.engagement_id=?1 AND n.state IN ('pending','claimed','sending','uncertain')",
    ] {
        if count(tx, sql, id)? != 0 {
            return Ok(Some(RebindRefusal::OutgoingCustodyPending));
        }
    }
    Ok(None)
}

/// The new package and role must be qualified for the frozen resource's
/// framework, model, provider and reasoning, and must keep plugin and adapter.
fn target_blocker(
    tx: &Connection,
    frozen: &Frozen,
    from: &RuntimePluginIdentity,
    to: &RuntimePluginIdentity,
) -> Result<Option<RebindRefusal>, Error> {
    let Some(resource) = &frozen.resource else {
        return Ok(Some(RebindRefusal::EngagementNotActive));
    };
    if from.plugin_id != to.plugin_id || from.adapter_id != to.adapter_id {
        return Ok(Some(RebindRefusal::PluginMismatch));
    }
    let roles: Option<String> = tx
        .query_row(
            "SELECT roles FROM plugin_runtime_qualifications WHERE plugin_id=?1 AND adapter_id=?2 \
             AND package_sha256=?3 AND settings_sha256=?4 AND framework=?5 AND model=?6 \
             AND provider IS ?7 AND reasoning IS ?8",
            params![
                to.plugin_id,
                to.adapter_id,
                to.package_sha256,
                to.settings_sha256,
                resource.framework,
                resource.model,
                resource.provider,
                resource.reasoning,
            ],
            |r| r.get(0),
        )
        .optional()?;
    let Some(roles) = roles else {
        return Ok(Some(RebindRefusal::Unqualified));
    };
    let roles: Vec<String> = serde_json::from_str(&roles)?;
    if !roles.contains(&frozen.role) {
        return Ok(Some(RebindRefusal::Unqualified));
    }
    Ok(None)
}

struct Audit<'a> {
    engagement_id: &'a str,
    resource_id: &'a str,
    action: &'a str,
    old: Option<&'a RuntimePluginIdentity>,
    new: Option<&'a RuntimePluginIdentity>,
    actor: &'a str,
    at: u64,
    outcome: String,
    detail: Option<&'a str>,
}
fn write_audit(tx: &Connection, a: Audit<'_>) -> Result<RuntimeRebindAudit, Error> {
    tx.execute(
        "INSERT INTO engagement_runtime_rebinds(engagement_id,resource_id,action,old_package_sha256,new_package_sha256,\
         old_settings_sha256,new_settings_sha256,actor,at,outcome,detail) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            a.engagement_id,
            a.resource_id,
            a.action,
            a.old.map(|r| &r.package_sha256),
            a.new.map(|r| &r.package_sha256),
            a.old.map(|r| &r.settings_sha256),
            a.new.map(|r| &r.settings_sha256),
            a.actor,
            a.at,
            a.outcome,
            a.detail,
        ],
    )?;
    let id = u64::try_from(tx.last_insert_rowid()).map_err(|_| Error::Schema)?;
    Ok(RuntimeRebindAudit {
        id,
        engagement_id: a.engagement_id.into(),
        resource_id: a.resource_id.into(),
        action: a.action.into(),
        old_package_sha256: a.old.map(|r| r.package_sha256.clone()),
        new_package_sha256: a.new.map(|r| r.package_sha256.clone()),
        old_settings_sha256: a.old.map(|r| r.settings_sha256.clone()),
        new_settings_sha256: a.new.map(|r| r.settings_sha256.clone()),
        actor: a.actor.into(),
        at: a.at,
        outcome: a.outcome,
        detail: a.detail.map(str::to_owned),
    })
}

fn upgrade_row(
    tx: &Connection,
    id: &str,
) -> Result<Option<(UpgradeState, RuntimePluginIdentity)>, Error> {
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT state,target_runtime FROM engagement_runtime_upgrades WHERE engagement_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(state, target)| {
        let state = match state.as_str() {
            "upgrade_pending" => UpgradeState::UpgradePending,
            "pinned" => UpgradeState::Pinned,
            "halted" => UpgradeState::Halted,
            _ => return Err(Error::Schema),
        };
        Ok((state, serde_json::from_str(&target)?))
    })
    .transpose()
}

fn set_frozen_runtime(
    tx: &Connection,
    id: &str,
    runtime: &RuntimePluginIdentity,
) -> Result<(), Error> {
    let changed = tx.execute(
        "UPDATE effects SET payload=json_set(payload,'$.resource.runtimePlugin',json(?2)) \
         WHERE engagement_id=?1 AND kind='provision' AND state='complete'",
        params![id, serde_json::to_string(runtime)?],
    )?;
    if changed != 1 {
        return Err(Error::State);
    }
    Ok(())
}

struct Rollout {
    state: String,
    target: RuntimePluginIdentity,
    canary: Option<String>,
    canary_result: Option<String>,
    canary_rebound_at: Option<u64>,
    detail: Option<String>,
}
fn read_rollout(tx: &Connection, resource_id: &str) -> Result<Option<Rollout>, Error> {
    let row = tx
        .query_row(
            "SELECT state,target_runtime,canary_engagement_id,canary_result,canary_rebound_at,detail \
             FROM engagement_runtime_rollouts WHERE resource_id=?1",
            [resource_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(state, target, canary, canary_result, canary_rebound_at, detail)| {
            Ok(Rollout {
                state,
                target: serde_json::from_str(&target)?,
                canary,
                canary_result,
                canary_rebound_at,
                detail,
            })
        },
    )
    .transpose()
}
#[allow(clippy::too_many_arguments)]
fn write_rollout(
    tx: &Connection,
    resource_id: &str,
    state: &str,
    target: &RuntimePluginIdentity,
    canary: Option<&str>,
    canary_result: Option<&str>,
    canary_rebound_at: Option<u64>,
    detail: Option<&str>,
    now: u64,
) -> Result<(), Error> {
    tx.execute(
        "INSERT INTO engagement_runtime_rollouts(resource_id,state,target_runtime,canary_engagement_id,canary_result,\
         canary_rebound_at,detail,started_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8) \
         ON CONFLICT(resource_id) DO UPDATE SET state=excluded.state,target_runtime=excluded.target_runtime,\
         canary_engagement_id=excluded.canary_engagement_id,canary_result=excluded.canary_result,\
         canary_rebound_at=excluded.canary_rebound_at,detail=excluded.detail,updated_at=excluded.updated_at",
        params![
            resource_id,
            state,
            serde_json::to_string(target)?,
            canary,
            canary_result,
            canary_rebound_at,
            detail,
            now,
        ],
    )?;
    Ok(())
}

/// Refuse (with an audit record) and return the result.
#[allow(clippy::too_many_arguments)]
fn refuse(
    tx: &Connection,
    id: &str,
    frozen: &Frozen,
    action: &str,
    old: Option<&RuntimePluginIdentity>,
    new: Option<&RuntimePluginIdentity>,
    actor: &str,
    now: u64,
    reason: RebindRefusal,
) -> Result<RebindResult, Error> {
    let audit = write_audit(
        tx,
        Audit {
            engagement_id: id,
            resource_id: &frozen.resource_id,
            action,
            old,
            new,
            actor,
            at: now,
            outcome: format!("refused:{}", reason.as_str()),
            detail: None,
        },
    )?;
    Ok(RebindResult {
        engagement_id: id.into(),
        applied: false,
        refusal: Some(reason),
        audit,
    })
}

/// Rebind to the marked target. Runs inside the caller's immediate transaction.
fn rebind_in(
    tx: &Transaction<'_>,
    id: &str,
    actor: &str,
    now: u64,
    in_rollout: bool,
    launchable: Launchable<'_>,
) -> Result<RebindResult, Error> {
    let frozen = read_frozen(tx, id)?;
    let current = frozen.runtime().cloned();
    let marked = upgrade_row(tx, id)?;
    let target = marked.as_ref().map(|(_, t)| t.clone());
    let refused = |reason| {
        refuse(
            tx,
            id,
            &frozen,
            "rebind",
            current.as_ref(),
            target.as_ref().or(current.as_ref()),
            actor,
            now,
            reason,
        )
    };
    let Some((state, target)) = marked else {
        return refused(RebindRefusal::NotMarked);
    };
    let Some(current) = current.clone() else {
        return refused(RebindRefusal::PluginMismatch);
    };
    match state {
        UpgradeState::Pinned => return refused(RebindRefusal::Pinned),
        UpgradeState::Halted => return refused(RebindRefusal::RolloutHalted),
        UpgradeState::UpgradePending => {}
    }
    if !in_rollout && let Some(rollout) = read_rollout(tx, &frozen.resource_id)? {
        if rollout.state == "halted" {
            return refused(RebindRefusal::RolloutHalted);
        }
        if rollout.state == "running"
            && rollout.canary_result.is_none()
            && rollout.canary.as_deref().is_some_and(|c| c != id)
        {
            return refused(RebindRefusal::CanaryPending);
        }
    }
    if frozen.state != "active" {
        return refused(RebindRefusal::EngagementNotActive);
    }
    if let Some(reason) = idle_blocker(tx, id)? {
        return refused(reason);
    }
    if let Some(reason) = target_blocker(tx, &frozen, &current, &target)? {
        return refused(reason);
    }
    if frozen
        .resource
        .as_ref()
        .is_none_or(|resource| !launchable(&target, &resource.framework))
    {
        return refused(RebindRefusal::TargetNotLaunchable);
    }
    set_frozen_runtime(tx, id, &target)?;
    tx.execute(
        "INSERT INTO engagement_runtime_rollbacks(engagement_id,previous_runtime,rebound_at) VALUES(?1,?2,?3) \
         ON CONFLICT(engagement_id) DO UPDATE SET previous_runtime=excluded.previous_runtime,rebound_at=excluded.rebound_at",
        params![id, serde_json::to_string(&current)?, now],
    )?;
    tx.execute(
        "DELETE FROM engagement_runtime_upgrades WHERE engagement_id=?1",
        [id],
    )?;
    let audit = write_audit(
        tx,
        Audit {
            engagement_id: id,
            resource_id: &frozen.resource_id,
            action: "rebind",
            old: Some(&current),
            new: Some(&target),
            actor,
            at: now,
            outcome: "applied".into(),
            detail: None,
        },
    )?;
    Ok(RebindResult {
        engagement_id: id.into(),
        applied: true,
        refusal: None,
        audit,
    })
}

/// One-step rollback to the runtime held in the rollback slot.
fn rollback_in(
    tx: &Transaction<'_>,
    id: &str,
    actor: &str,
    now: u64,
) -> Result<RebindResult, Error> {
    let frozen = read_frozen(tx, id)?;
    let current = frozen.runtime().cloned();
    let previous: Option<String> = tx
        .query_row(
            "SELECT previous_runtime FROM engagement_runtime_rollbacks WHERE engagement_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let previous: Option<RuntimePluginIdentity> =
        previous.map(|v| serde_json::from_str(&v)).transpose()?;
    let refused = |reason| {
        refuse(
            tx,
            id,
            &frozen,
            "rollback",
            current.as_ref(),
            previous.as_ref(),
            actor,
            now,
            reason,
        )
    };
    let (Some(current), Some(previous)) = (current.clone(), previous.clone()) else {
        return refused(RebindRefusal::NoPrevious);
    };
    if frozen.state != "active" {
        return refused(RebindRefusal::EngagementNotActive);
    }
    if let Some(reason) = idle_blocker(tx, id)? {
        return refused(reason);
    }
    if let Some(reason) = target_blocker(tx, &frozen, &current, &previous)? {
        return refused(reason);
    }
    set_frozen_runtime(tx, id, &previous)?;
    tx.execute(
        "DELETE FROM engagement_runtime_rollbacks WHERE engagement_id=?1",
        [id],
    )?;
    // The package it was rolled back from stays visible as `halted`: the
    // rollout skips it until the operator starts the rollout again.
    tx.execute(
        "INSERT INTO engagement_runtime_upgrades(engagement_id,resource_id,state,target_runtime,marked_at,updated_at) \
         VALUES(?1,?2,'halted',?3,?4,?4) \
         ON CONFLICT(engagement_id) DO UPDATE SET state='halted',target_runtime=excluded.target_runtime,updated_at=excluded.updated_at",
        params![id, frozen.resource_id, serde_json::to_string(&current)?, now],
    )?;
    if let Some(rollout) = read_rollout(tx, &frozen.resource_id)?
        && rollout.canary.as_deref() == Some(id)
        && rollout.state != "halted"
    {
        write_rollout(
            tx,
            &frozen.resource_id,
            "halted",
            &rollout.target,
            Some(id),
            Some("failed"),
            rollout.canary_rebound_at,
            Some("canary rolled back"),
            now,
        )?;
    }
    let audit = write_audit(
        tx,
        Audit {
            engagement_id: id,
            resource_id: &frozen.resource_id,
            action: "rollback",
            old: Some(&current),
            new: Some(&previous),
            actor,
            at: now,
            outcome: "rolled_back".into(),
            detail: None,
        },
    )?;
    Ok(RebindResult {
        engagement_id: id.into(),
        applied: true,
        refusal: None,
        audit,
    })
}

fn check(id: &str, actor: &str, now: u64) -> Result<(), Error> {
    identifier(id, 128)?;
    if actor.is_empty()
        || actor.len() > 128
        || !actor
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':' | b'@'))
    {
        return Err(InvalidInput("invalid actor").into());
    }
    clock(now)?;
    Ok(())
}

impl DomainRepository {
    fn immediate(&mut self) -> Result<Transaction<'_>, Error> {
        Ok(self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }

    /// Rebind one engagement to the runtime its resource now names, only at an
    /// idle boundary. A refusal is committed as an audit record and returned.
    pub fn rebind_engagement_runtime(
        &mut self,
        engagement_id: &str,
        actor: &str,
        now: u64,
        launchable: Launchable<'_>,
    ) -> Result<RebindResult, Error> {
        check(engagement_id, actor, now)?;
        let tx = self.immediate()?;
        let result = rebind_in(&tx, engagement_id, actor, now, false, launchable)?;
        tx.commit()?;
        Ok(result)
    }

    /// Return to the package the engagement ran before its last rebind (one
    /// step). Obeys the same idle boundary and qualification as a rebind.
    pub fn rollback_engagement_runtime(
        &mut self,
        engagement_id: &str,
        actor: &str,
        now: u64,
    ) -> Result<RebindResult, Error> {
        check(engagement_id, actor, now)?;
        let tx = self.immediate()?;
        let result = rollback_in(&tx, engagement_id, actor, now)?;
        tx.commit()?;
        Ok(result)
    }

    /// Hold an engagement on its current package. The rollout skips it.
    pub fn pin_engagement_runtime(
        &mut self,
        engagement_id: &str,
        actor: &str,
        now: u64,
    ) -> Result<RuntimeRebindAudit, Error> {
        check(engagement_id, actor, now)?;
        let tx = self.immediate()?;
        let frozen = read_frozen(&tx, engagement_id)?;
        let current = frozen.runtime().cloned().ok_or(Error::State)?;
        if !matches!(frozen.state.as_str(), "reserved" | "active") {
            return Err(Error::State);
        }
        tx.execute(
            "INSERT INTO engagement_runtime_upgrades(engagement_id,resource_id,state,target_runtime,marked_at,updated_at) \
             VALUES(?1,?2,'pinned',?3,?4,?4) \
             ON CONFLICT(engagement_id) DO UPDATE SET state='pinned',updated_at=excluded.updated_at",
            params![
                engagement_id,
                frozen.resource_id,
                serde_json::to_string(&current)?,
                now
            ],
        )?;
        let audit = write_audit(
            &tx,
            Audit {
                engagement_id,
                resource_id: &frozen.resource_id,
                action: "pin",
                old: Some(&current),
                new: Some(&current),
                actor,
                at: now,
                outcome: "pinned".into(),
                detail: None,
            },
        )?;
        tx.commit()?;
        Ok(audit)
    }

    /// Return a pinned engagement to the rollout queue.
    pub fn unpin_engagement_runtime(
        &mut self,
        engagement_id: &str,
        actor: &str,
        now: u64,
    ) -> Result<RuntimeRebindAudit, Error> {
        check(engagement_id, actor, now)?;
        let tx = self.immediate()?;
        let frozen = read_frozen(&tx, engagement_id)?;
        let current = frozen.runtime().cloned().ok_or(Error::State)?;
        match upgrade_row(&tx, engagement_id)? {
            Some((UpgradeState::Pinned, target)) => {
                if target == current {
                    tx.execute(
                        "DELETE FROM engagement_runtime_upgrades WHERE engagement_id=?1",
                        [engagement_id],
                    )?;
                } else {
                    tx.execute(
                        "UPDATE engagement_runtime_upgrades SET state='upgrade_pending',updated_at=?2 WHERE engagement_id=?1",
                        params![engagement_id, now],
                    )?;
                }
            }
            _ => return Err(Error::State),
        }
        let audit = write_audit(
            &tx,
            Audit {
                engagement_id,
                resource_id: &frozen.resource_id,
                action: "unpin",
                old: Some(&current),
                new: Some(&current),
                actor,
                at: now,
                outcome: "unpinned".into(),
                detail: None,
            },
        )?;
        tx.commit()?;
        Ok(audit)
    }

    /// Start (or restart after a halt) the rollout of a resource: the next
    /// step picks a new canary and `halted` engagements return to the queue.
    pub fn start_resource_rollout(
        &mut self,
        resource_id: &str,
        actor: &str,
        now: u64,
    ) -> Result<RolloutStatus, Error> {
        check(resource_id, actor, now)?;
        let tx = self.immediate()?;
        reset_rollout(&tx, resource_id, now)?;
        tx.commit()?;
        self.resource_rollout_status(resource_id)
    }

    /// Record the canary's probe result. A failure halts the rollout; the
    /// remaining engagements stay `upgrade_pending` on their old package.
    pub fn record_canary_result(
        &mut self,
        resource_id: &str,
        passed: bool,
        actor: &str,
        detail: Option<&str>,
        now: u64,
    ) -> Result<RolloutStatus, Error> {
        check(resource_id, actor, now)?;
        let tx = self.immediate()?;
        let rollout = read_rollout(&tx, resource_id)?.ok_or(Error::NotFound)?;
        let canary = rollout.canary.clone().ok_or(Error::State)?;
        if rollout.state != "running" || rollout.canary_result.is_some() {
            return Err(Error::State);
        }
        if passed {
            write_rollout(
                &tx,
                resource_id,
                "running",
                &rollout.target,
                Some(&canary),
                Some("passed"),
                rollout.canary_rebound_at,
                detail,
                now,
            )?;
        } else {
            halt(&tx, resource_id, &rollout, &canary, actor, detail, now)?;
        }
        tx.commit()?;
        self.resource_rollout_status(resource_id)
    }

    /// Advance a rollout by at most one engagement. The first engagement is the
    /// canary; nothing else is rebound until it passed (a completed task after
    /// its rebind, or an explicit `record_canary_result`).
    pub fn rollout_step(
        &mut self,
        resource_id: &str,
        actor: &str,
        now: u64,
        launchable: Launchable<'_>,
    ) -> Result<RolloutStep, Error> {
        check(resource_id, actor, now)?;
        let tx = self.immediate()?;
        let step = rollout_step_in(&tx, resource_id, actor, now, launchable)?;
        tx.commit()?;
        Ok(step)
    }

    pub fn resource_rollout_status(&self, resource_id: &str) -> Result<RolloutStatus, Error> {
        identifier(resource_id, 128)?;
        let rollout = read_rollout(&self.db, resource_id)?;
        let mut statement = self.db.prepare(
            "SELECT u.engagement_id,u.state FROM engagement_runtime_upgrades u \
             JOIN engagements e ON e.id=u.engagement_id \
             WHERE u.resource_id=?1 AND e.state IN ('pending','reserved','active') ORDER BY u.engagement_id",
        )?;
        let rows = statement
            .query_map([resource_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut pending = Vec::new();
        for (engagement_id, state) in rows {
            let state = match state.as_str() {
                "upgrade_pending" => UpgradeState::UpgradePending,
                "pinned" => UpgradeState::Pinned,
                "halted" => UpgradeState::Halted,
                _ => return Err(Error::Schema),
            };
            let blocker = idle_blocker(&self.db, &engagement_id)?;
            pending.push(RolloutPending {
                engagement_id,
                state,
                blocker,
            });
        }
        Ok(match rollout {
            Some(r) => RolloutStatus {
                resource_id: resource_id.into(),
                state: r.state,
                target_runtime: Some(r.target),
                canary_engagement_id: r.canary,
                canary_result: r.canary_result,
                detail: r.detail,
                pending,
            },
            None => RolloutStatus {
                resource_id: resource_id.into(),
                state: "none".into(),
                target_runtime: None,
                canary_engagement_id: None,
                canary_result: None,
                detail: None,
                pending,
            },
        })
    }

    pub fn engagement_upgrade_status(
        &self,
        engagement_id: &str,
    ) -> Result<EngagementUpgradeStatus, Error> {
        identifier(engagement_id, 128)?;
        let frozen = read_frozen(&self.db, engagement_id)?;
        let marked = upgrade_row(&self.db, engagement_id)?;
        let previous: Option<String> = self
            .db
            .query_row(
                "SELECT previous_runtime FROM engagement_runtime_rollbacks WHERE engagement_id=?1",
                [engagement_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(EngagementUpgradeStatus {
            engagement_id: engagement_id.into(),
            resource_id: frozen.resource_id.clone(),
            current_runtime: frozen.runtime().cloned(),
            target_runtime: marked.as_ref().map(|(_, t)| t.clone()),
            state: marked.map(|(s, _)| s),
            blocker: idle_blocker(&self.db, engagement_id)?,
            previous_runtime: previous.map(|v| serde_json::from_str(&v)).transpose()?,
            audit: self.engagement_runtime_rebinds(Some(engagement_id), 50)?,
        })
    }

    /// Newest-first audit, optionally of one engagement.
    pub fn engagement_runtime_rebinds(
        &self,
        engagement_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RuntimeRebindAudit>, Error> {
        let limit = i64::try_from(limit.clamp(1, 500)).map_err(|_| Error::Schema)?;
        let mut statement = self.db.prepare(
            "SELECT id,engagement_id,resource_id,action,old_package_sha256,new_package_sha256,old_settings_sha256,\
             new_settings_sha256,actor,at,outcome,detail FROM engagement_runtime_rebinds \
             WHERE (?1 IS NULL OR engagement_id=?1) ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![engagement_id, limit], |r| {
                Ok(RuntimeRebindAudit {
                    id: r.get(0)?,
                    engagement_id: r.get(1)?,
                    resource_id: r.get(2)?,
                    action: r.get(3)?,
                    old_package_sha256: r.get(4)?,
                    new_package_sha256: r.get(5)?,
                    old_settings_sha256: r.get(6)?,
                    new_settings_sha256: r.get(7)?,
                    actor: r.get(8)?,
                    at: r.get(9)?,
                    outcome: r.get(10)?,
                    detail: r.get(11)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn pending_rows(
    tx: &Connection,
    resource_id: &str,
) -> Result<Vec<(String, RuntimePluginIdentity)>, Error> {
    let mut statement = tx.prepare(
        "SELECT u.engagement_id,u.target_runtime FROM engagement_runtime_upgrades u \
         JOIN engagements e ON e.id=u.engagement_id \
         WHERE u.resource_id=?1 AND u.state='upgrade_pending' AND e.state='active' ORDER BY u.engagement_id",
    )?;
    let rows = statement
        .query_map([resource_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(id, target)| Ok((id, serde_json::from_str(&target)?)))
        .collect()
}

/// New rollout cycle: `halted` engagements rejoin the queue, the canary is
/// chosen again by the next step.
fn reset_rollout(tx: &Connection, resource_id: &str, now: u64) -> Result<(), Error> {
    let target: Option<String> = tx
        .query_row(
            "SELECT target_runtime FROM engagement_runtime_upgrades WHERE resource_id=?1 AND state<>'pinned' ORDER BY engagement_id LIMIT 1",
            [resource_id],
            |r| r.get(0),
        )
        .optional()?;
    let target: RuntimePluginIdentity = target
        .map(|v| serde_json::from_str(&v))
        .transpose()?
        .ok_or(Error::NotFound)?;
    // A halted engagement ran on its old package; it targets what the
    // resource names now, which is the row's target of its pending siblings.
    tx.execute(
        "UPDATE engagement_runtime_upgrades SET state='upgrade_pending',target_runtime=?2,updated_at=?3 \
         WHERE resource_id=?1 AND state='halted'",
        params![resource_id, serde_json::to_string(&target)?, now],
    )?;
    write_rollout(
        tx,
        resource_id,
        "running",
        &target,
        None,
        None,
        None,
        None,
        now,
    )
}

fn halt(
    tx: &Connection,
    resource_id: &str,
    rollout: &Rollout,
    canary: &str,
    actor: &str,
    detail: Option<&str>,
    now: u64,
) -> Result<(), Error> {
    write_rollout(
        tx,
        resource_id,
        "halted",
        &rollout.target,
        Some(canary),
        Some("failed"),
        rollout.canary_rebound_at,
        detail,
        now,
    )?;
    let current = read_frozen(tx, canary)?.runtime().cloned();
    write_audit(
        tx,
        Audit {
            engagement_id: canary,
            resource_id,
            action: "rollout",
            old: None,
            new: current.as_ref(),
            actor,
            at: now,
            outcome: "halted".into(),
            detail,
        },
    )?;
    Ok(())
}

fn rollout_step_in(
    tx: &Transaction<'_>,
    resource_id: &str,
    actor: &str,
    now: u64,
    launchable: Launchable<'_>,
) -> Result<RolloutStep, Error> {
    let mut rollout = read_rollout(tx, resource_id)?;
    let candidates = pending_rows(tx, resource_id)?;
    // A different target than the rollout's is a new package nobody probed:
    // begin a new cycle with a new canary.
    let new_target = candidates
        .first()
        .is_some_and(|(_, t)| rollout.as_ref().is_none_or(|r| &r.target != t));
    if rollout.as_ref().is_none_or(|r| r.state == "complete")
        || (new_target && rollout.as_ref().is_some_and(|r| r.state == "running"))
    {
        if candidates.is_empty() {
            return Ok(RolloutStep::Complete);
        }
        reset_rollout(tx, resource_id, now)?;
        rollout = read_rollout(tx, resource_id)?;
    }
    let rollout = rollout.ok_or(Error::State)?;
    if rollout.state == "halted" {
        return Ok(RolloutStep::Halted);
    }
    let mut canary_passed = rollout.canary_result.as_deref() == Some("passed");
    if let (Some(canary), None) = (&rollout.canary, &rollout.canary_result) {
        // One task completed on the new package since the rebind.
        let ok: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runner_attempts a JOIN runner_dispatches d ON d.id=a.dispatch_id \
             JOIN runner_sessions s ON s.id=d.session_id \
             WHERE s.engagement_id=?1 AND d.state='completed' AND a.created_at>=?2)",
            params![canary, rollout.canary_rebound_at.unwrap_or(0)],
            |r| r.get(0),
        )?;
        if !ok {
            return Ok(RolloutStep::AwaitingCanary {
                engagement_id: canary.clone(),
            });
        }
        write_rollout(
            tx,
            resource_id,
            "running",
            &rollout.target,
            Some(canary),
            Some("passed"),
            rollout.canary_rebound_at,
            Some("a task completed on the new package"),
            now,
        )?;
        canary_passed = true;
    }
    if candidates.is_empty() {
        write_rollout(
            tx,
            resource_id,
            "complete",
            &rollout.target,
            rollout.canary.as_deref(),
            rollout
                .canary_result
                .as_deref()
                .or(canary_passed.then_some("passed")),
            rollout.canary_rebound_at,
            rollout.detail.as_deref(),
            now,
        )?;
        return Ok(RolloutStep::Complete);
    }
    let mut blockers = Vec::new();
    for (id, _) in candidates {
        if let Some(reason) = idle_blocker(tx, &id)? {
            blockers.push(RolloutBlocker {
                engagement_id: id,
                reason,
            });
            continue;
        }
        let result = rebind_in(tx, &id, actor, now, true, launchable)?;
        if let Some(reason) = result.refusal {
            debug_assert!(!reason.is_busy());
            blockers.push(RolloutBlocker {
                engagement_id: id,
                reason,
            });
            continue;
        }
        let canary = rollout.canary.is_none();
        if canary {
            write_rollout(
                tx,
                resource_id,
                "running",
                &rollout.target,
                Some(&id),
                None,
                Some(now),
                None,
                now,
            )?;
        }
        return Ok(RolloutStep::Rebound {
            engagement_id: id,
            canary,
        });
    }
    Ok(RolloutStep::Blocked { blockers })
}
