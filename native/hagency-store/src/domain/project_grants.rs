//! Provider reservations and project grants share the engagement writer and
//! transaction. Revoked/expired grants retain their reservation until an
//! explicit, verified release; a timeout cannot manufacture free capacity.
use super::{DomainRepository, bounded_row, check_grant, read_resource, serialize};
use crate::Error;
use hagency_core::{
    JSON_SAFE_MAX,
    authority::{Registration, VerifiedRequest},
    project_grants::{ProjectGrant, ResourceDelegation},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProjectAgentDecision {
    pub grant_id: String,
    pub grant_revision: u64,
    pub actor_mxid: String,
    pub requester_mxid: String,
}

pub(super) fn registration(db: &Connection, fleet: &str) -> Result<Registration, Error> {
    let config: String = db
        .query_row(
            "SELECT config FROM registrations WHERE fleet_id=?1",
            [fleet],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    Ok(serde_json::from_str(&config)?)
}
fn delegation(db: &Connection, id: &str, now: u64) -> Result<ResourceDelegation, Error> {
    let (config, revoked): (String, Option<u64>) = db
        .query_row(
            "SELECT config,revoked_at FROM resource_delegations WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    if revoked.is_some() {
        return Err(Error::GrantRevoked);
    }
    let grant: ResourceDelegation = serde_json::from_str(&config)?;
    if grant.expires_at_ms <= now {
        return Err(Error::GrantExpired);
    }
    let current = registration(db, &grant.fleet_id)?;
    if grant.registration_generation != current.generation || grant.issuer != current.server_name {
        return Err(Error::GrantAuthority);
    }
    grant.validate(&current, now)?;
    Ok(grant)
}
pub(super) fn project(
    db: &Connection,
    id: &str,
    now: u64,
) -> Result<(ProjectGrant, ResourceDelegation), Error> {
    let (config, revoked): (String, Option<u64>) = db
        .query_row(
            "SELECT config,revoked_at FROM project_grants WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    if revoked.is_some() {
        return Err(Error::GrantRevoked);
    }
    let grant: ProjectGrant = serde_json::from_str(&config)?;
    if grant.expires_at_ms <= now {
        return Err(Error::GrantExpired);
    }
    let parent = delegation(db, &grant.delegation_id, now)?;
    grant.validate(&parent, now)?;
    Ok((grant, parent))
}

/// The unassigned part of each provider reservation. Assigned agents already
/// appear in the existing engagement accounting, so they are subtracted once.
/// Ended agents do not implicitly refund their budget or their unknown spend.
pub(super) fn unassigned(db: &Connection, resource: &str) -> Result<u64, Error> {
    let value: u64 = db.query_row("SELECT COALESCE(SUM(json_extract(d.config,'$.limits.tokens') - COALESCE((SELECT SUM(COALESCE(e.allocated_tokens,e.tokens)) FROM project_grant_agents a JOIN project_grants g ON g.id=a.grant_id JOIN engagements e ON e.id=a.engagement_id WHERE g.delegation_id=d.id AND e.state IN ('reserved','active')),0)),0) FROM resource_delegations d WHERE d.resource_id=?1", [resource], |r| r.get(0))?;
    if value > JSON_SAFE_MAX {
        return Err(Error::Capacity);
    }
    Ok(value)
}

pub(super) fn validate_approval(
    db: &Connection,
    scope: &ProjectAgentDecision,
    proof: &VerifiedRequest,
    now: u64,
) -> Result<(ProjectGrant, ResourceDelegation), Error> {
    let (grant, parent) = project(db, &scope.grant_id, now)?;
    let req = proof.request();
    if grant.revision != scope.grant_revision
        || parent.fleet_id != req.fleet_id
        || parent.registration_generation != proof.registration().generation
        || parent.resource_id != req.agent_definition.resource_id
        || grant.project_id != req.target_project_id
        || grant.room_id != req.target_room_id
        || grant.owner_mxid != req.owner_mxid
        || scope.requester_mxid != req.requester_mxid
        || !grant.permits_decision(&scope.actor_mxid, &scope.requester_mxid)
    {
        return Err(Error::GrantAuthority);
    }
    Ok((grant, parent))
}

/// Called INSIDE the existing approval transaction. The insert, engagement
/// reservation, provision effect and idempotent decision receipt commit together.
pub(super) fn debit_approval(
    db: &Connection,
    scope: &ProjectAgentDecision,
    proof: &VerifiedRequest,
    granted: u64,
    now: u64,
) -> Result<(), Error> {
    let (grant, parent) = validate_approval(db, scope, proof, now)?;
    let req = proof.request();
    let rate = req
        .rate_per_day
        .map(u64::from)
        .filter(|v| *v > 0)
        .ok_or(Error::GrantAuthority)?;
    // Debits are lifetime allocations. Releasing a running agent is not proof
    // that its tokens were unused; safe refunds need a separate receipt.
    let debit: u64 = db.query_row(
        "SELECT COALESCE(SUM(debited_tokens),0) FROM project_grant_agents WHERE grant_id=?1",
        [&grant.id],
        |r| r.get(0),
    )?;
    let (agents, rates): (u64, u64) = db.query_row("SELECT COUNT(*),COALESCE(SUM(json_extract(e.context,'$.ratePerDay')),0) FROM project_grant_agents a JOIN engagements e ON e.id=a.engagement_id WHERE a.grant_id=?1 AND e.state IN ('reserved','active')", [&grant.id], |r| Ok((r.get(0)?,r.get(1)?)))?;
    if granted == 0
        || debit
            .checked_add(granted)
            .is_none_or(|v| v > grant.limits.tokens)
        || agents >= grant.limits.max_agents
        || rates
            .checked_add(rate)
            .is_none_or(|v| v > grant.limits.max_rate_per_day)
    {
        return Err(Error::InsufficientCapacity);
    }
    let resource = read_resource(db, &parent.resource_id)?;
    // A later provider policy change must still be respected. This consumes
    // existing reserved capacity, so do not charge granted tokens again.
    let report = super::usage::ceiling_report(db, &parent.resource_id, now)?;
    let budget = super::budget(db, &resource, None, false)?;
    if report.ceiling_tokens.is_some_and(|n| report.drawn > n)
        || budget
            .pool
            .ceiling
            .is_some_and(|n| u64::from(budget.pool.committed) > u64::from(n))
        || budget
            .seat
            .quota
            .is_some_and(|n| u64::from(budget.seat.committed) > u64::from(n))
    {
        return Err(Error::InsufficientCapacity);
    }
    check_grant(db, &resource, req.agent_definition.name.as_str(), 0, now)?;
    bounded_row(
        db,
        "project_grant_agents",
        "engagement_id",
        &req.engagement_id()?,
        10_000,
    )?;
    db.execute("INSERT INTO project_grant_agents(engagement_id,grant_id,debited_tokens,decision_actor,grant_revision,created_at) VALUES(?1,?2,?3,?4,?5,?6)", params![req.engagement_id()?, grant.id, granted, scope.actor_mxid, scope.grant_revision, now])?;
    Ok(())
}

pub(super) fn validate_top_up(
    db: &Connection,
    scope: &ProjectAgentDecision,
    id: &str,
    now: u64,
) -> Result<(ProjectGrant, ResourceDelegation), Error> {
    let (grant, parent) = project(db, &scope.grant_id, now)?;
    let member: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_grant_agents WHERE engagement_id=?1 AND grant_id=?2)",
        params![id, grant.id],
        |r| r.get(0),
    )?;
    if !member
        || scope.grant_revision != grant.revision
        || scope.requester_mxid != grant.owner_mxid
        || !grant.permits_decision(&scope.actor_mxid, &scope.requester_mxid)
    {
        return Err(Error::GrantAuthority);
    }
    Ok((grant, parent))
}
pub(super) fn debit_top_up(
    db: &Connection,
    scope: &ProjectAgentDecision,
    id: &str,
    add: u64,
    now: u64,
) -> Result<(), Error> {
    let (grant, parent) = validate_top_up(db, scope, id, now)?;
    let debit: u64 = db.query_row(
        "SELECT COALESCE(SUM(debited_tokens),0) FROM project_grant_agents WHERE grant_id=?1",
        [&grant.id],
        |r| r.get(0),
    )?;
    if add == 0
        || debit
            .checked_add(add)
            .is_none_or(|v| v > grant.limits.tokens)
    {
        return Err(Error::InsufficientCapacity);
    }
    let resource = read_resource(db, &parent.resource_id)?;
    let report = super::usage::ceiling_report(db, &parent.resource_id, now)?;
    let budget = super::budget(db, &resource, None, false)?;
    if report.ceiling_tokens.is_some_and(|n| report.drawn > n)
        || budget
            .pool
            .ceiling
            .is_some_and(|n| budget.pool.committed > n)
        || budget.seat.quota.is_some_and(|n| budget.seat.committed > n)
    {
        return Err(Error::InsufficientCapacity);
    }
    check_grant(db, &resource, "Project top-up", 0, now)?;
    db.execute(
        "UPDATE project_grant_agents SET debited_tokens=debited_tokens+?2 WHERE engagement_id=?1",
        params![id, add],
    )?;
    Ok(())
}
pub(super) fn is_delegated(db: &Connection, id: &str) -> Result<bool, Error> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_grant_agents WHERE engagement_id=?1)",
        [id],
        |r| r.get(0),
    )?)
}

pub(super) fn replay(
    db: &Connection,
    id: &str,
    digest: &str,
) -> Result<Option<hagency_core::project::Engagement>, Error> {
    let old: Option<(String, String)> = db
        .query_row(
            "SELECT digest,result FROM project_grant_decisions WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    old.map(|(saved, result)| {
        if saved != digest {
            return Err(Error::Conflict);
        }
        Ok(serde_json::from_str(&result)?)
    })
    .transpose()
}
pub(super) fn record(
    db: &Connection,
    id: &str,
    digest: &str,
    result: &hagency_core::project::Engagement,
    now: u64,
) -> Result<(), Error> {
    hagency_core::project::identifier(id, 128)?;
    bounded_row(db, "project_grant_decisions", "id", id, 100_000)?;
    db.execute(
        "INSERT INTO project_grant_decisions(id,digest,result,created_at) VALUES(?1,?2,?3,?4)",
        params![id, digest, serialize(result)?, now],
    )?;
    Ok(())
}

/// Read-time authority check; legacy engagements have no delegated grant.
pub(super) fn check_engagement(db: &Connection, id: &str, now: u64) -> Result<(), Error> {
    let grant: Option<String> = db
        .query_row(
            "SELECT grant_id FROM project_grant_agents WHERE engagement_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(grant) = grant {
        project(db, &grant, now)?;
    }
    Ok(())
}

/// Fence queued/started provisioning and execution using the ordinary retirement
/// kernel. Reservation debits are retained while cleanup is pending or unknown.
pub(super) fn reconcile(tx: &rusqlite::Transaction<'_>, now: u64) -> Result<(), Error> {
    let ids = tx.prepare("SELECT e.id FROM project_grant_agents a JOIN engagements e ON e.id=a.engagement_id WHERE e.state IN ('reserved','active') ORDER BY e.id")?
        .query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    for id in ids {
        match check_engagement(tx, &id, now) {
            Ok(()) => {}
            Err(Error::GrantExpired | Error::GrantRevoked | Error::GrantAuthority) => {
                super::end_transaction(tx, &format!("grant_retire_{id}"), &id, true, now)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

impl DomainRepository {
    /// Operator-only caller. Publishing a resource never calls this method.
    pub fn delegate_resource(
        &mut self,
        grant: &ResourceDelegation,
        now: u64,
    ) -> Result<ResourceDelegation, Error> {
        self.delegate_resource_clock(grant, || Ok(now))
    }
    pub(crate) fn delegate_resource_clock(
        &mut self,
        grant: &ResourceDelegation,
        clock: impl FnOnce() -> Result<u64, Error>,
    ) -> Result<ResourceDelegation, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = clock()?;
        if grant.expires_at_ms <= now {
            return Err(Error::GrantExpired);
        }
        grant.validate(&registration(&tx, &grant.fleet_id)?, now)?;
        let old: Option<String> = tx
            .query_row(
                "SELECT config FROM resource_delegations WHERE id=?1",
                [&grant.id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            if serde_json::from_str::<ResourceDelegation>(&old)? != *grant {
                return Err(Error::Conflict);
            }
            return delegation(&tx, &grant.id, now);
        }
        bounded_row(&tx, "resource_delegations", "id", &grant.id, 10_000)?;
        let resource = read_resource(&tx, &grant.resource_id)?;
        self.accounts.check_resource(&tx, &resource)?;
        check_grant(
            &tx,
            &resource,
            "Project delegation",
            grant.limits.tokens,
            now,
        )?;
        tx.execute("INSERT INTO resource_delegations(id,fleet_id,resource_id,config,created_at) VALUES(?1,?2,?3,?4,?5)", params![grant.id, grant.fleet_id, grant.resource_id, serialize(grant)?, now])?;
        tx.commit()?;
        Ok(grant.clone())
    }

    /// The transport supplies its authenticated current registration. Palpo
    /// cannot use one fleet's machine channel to reserve another fleet's grant.
    pub fn reserve_project_grant(
        &mut self,
        grant: &ProjectGrant,
        issuer: &Registration,
        now: u64,
    ) -> Result<ProjectGrant, Error> {
        self.reserve_project_grant_clock(grant, issuer, || Ok(now))
    }
    pub(crate) fn reserve_project_grant_clock(
        &mut self,
        grant: &ProjectGrant,
        issuer: &Registration,
        clock: impl FnOnce() -> Result<u64, Error>,
    ) -> Result<ProjectGrant, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = clock()?;
        let result = reserve(&tx, grant, issuer, now)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn project_grant(&self, id: &str, now: u64) -> Result<ProjectGrant, Error> {
        Ok(project(&self.db, id, now)?.0)
    }

    /// Only Palpo's authenticated registration may change a project's explicit
    /// approval policy. Ownership and budget never move with this operation.
    pub fn update_project_administrators(
        &mut self,
        id: &str,
        expected_revision: u64,
        administrators: &[String],
        allow_self_approval: bool,
        issuer: &Registration,
        now: u64,
    ) -> Result<ProjectGrant, Error> {
        self.update_project_administrators_clock(
            id,
            expected_revision,
            administrators,
            allow_self_approval,
            issuer,
            || Ok(now),
        )
    }
    pub(crate) fn update_project_administrators_clock(
        &mut self,
        id: &str,
        expected_revision: u64,
        administrators: &[String],
        allow_self_approval: bool,
        issuer: &Registration,
        clock: impl FnOnce() -> Result<u64, Error>,
    ) -> Result<ProjectGrant, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = clock()?;
        let result = assign(
            &tx,
            id,
            expected_revision,
            administrators,
            allow_self_approval,
            issuer,
            now,
        )?;
        tx.commit()?;
        Ok(result)
    }

    pub fn revoke_project_grant(
        &mut self,
        id: &str,
        expected_revision: u64,
        issuer: &Registration,
        now: u64,
    ) -> Result<(), Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = revoke_project(&tx, id, expected_revision, issuer, now)?;
        tx.commit()?;
        Ok(result)
    }

    /// Revocation fences new work immediately; held capacity is NOT recycled
    /// while runtime cleanup or final usage may still be outstanding.
    pub fn revoke_resource_delegation(
        &mut self,
        id: &str,
        expected_revision: u64,
        now: u64,
    ) -> Result<(), Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw: String = tx
            .query_row(
                "SELECT config FROM resource_delegations WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        let grant: ResourceDelegation = serde_json::from_str(&raw)?;
        if grant.revision != expected_revision {
            return Err(Error::Conflict);
        }
        tx.execute(
            "UPDATE resource_delegations SET revoked_at=COALESCE(revoked_at,?2) WHERE id=?1",
            params![id, now],
        )?;
        reconcile(&tx, now)?;
        tx.commit()?;
        Ok(())
    }
    pub fn reconcile_project_grants(&mut self, now: u64) -> Result<(), Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        reconcile(&tx, now)?;
        tx.commit()?;
        Ok(())
    }
}

pub(super) fn reserve(
    tx: &rusqlite::Transaction<'_>,
    grant: &ProjectGrant,
    issuer: &Registration,
    now: u64,
) -> Result<ProjectGrant, Error> {
    let parent = delegation(tx, &grant.delegation_id, now)?;
    if issuer != &registration(tx, &parent.fleet_id)? {
        return Err(Error::GrantAuthority);
    }
    if grant.expires_at_ms <= now {
        return Err(Error::GrantExpired);
    }
    grant.validate(&parent, now)?;
    let old: Option<String> = tx
        .query_row(
            "SELECT config FROM project_grants WHERE id=?1",
            [&grant.id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(old) = old {
        if serde_json::from_str::<ProjectGrant>(&old)? != *grant {
            return Err(Error::Conflict);
        }
        return Ok(project(tx, &grant.id, now)?.0);
    }
    bounded_row(tx, "project_grants", "id", &grant.id, 10_000)?;
    let (tokens, agents, rate): (u64, u64, u64) = tx.query_row("SELECT COALESCE(SUM(json_extract(config,'$.limits.tokens')),0),COALESCE(SUM(json_extract(config,'$.limits.maxAgents')),0),COALESCE(SUM(json_extract(config,'$.limits.maxRatePerDay')),0) FROM project_grants WHERE delegation_id=?1", [&parent.id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if tokens
        .checked_add(grant.limits.tokens)
        .is_none_or(|n| n > parent.limits.tokens)
        || agents
            .checked_add(grant.limits.max_agents)
            .is_none_or(|n| n > parent.limits.max_agents)
        || rate
            .checked_add(grant.limits.max_rate_per_day)
            .is_none_or(|n| n > parent.limits.max_rate_per_day)
    {
        return Err(Error::InsufficientCapacity);
    }
    let existing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM project_grants WHERE fleet_id=?1 AND project_id=?2 AND resource_id=?3)", params![parent.fleet_id, grant.project_id, parent.resource_id], |r| r.get(0))?;
    if existing {
        return Err(Error::Conflict);
    }
    tx.execute("INSERT INTO project_grants(id,delegation_id,fleet_id,project_id,resource_id,config,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![grant.id,parent.id,parent.fleet_id,grant.project_id,parent.resource_id,serialize(grant)?,now])?;
    Ok(grant.clone())
}

pub(super) fn assign(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    expected_revision: u64,
    administrators: &[String],
    allow_self_approval: bool,
    issuer: &Registration,
    now: u64,
) -> Result<ProjectGrant, Error> {
    let (mut grant, parent) = project(tx, id, now)?;
    if issuer != &registration(tx, &parent.fleet_id)? {
        return Err(Error::GrantAuthority);
    }
    let next = expected_revision
        .checked_add(1)
        .filter(|n| *n <= JSON_SAFE_MAX)
        .ok_or(Error::Capacity)?;
    if grant.revision == next
        && grant.administrator_mxids == administrators
        && grant.allow_self_approval == allow_self_approval
    {
        return Ok(grant);
    }
    if grant.revision != expected_revision {
        return Err(Error::Conflict);
    }
    grant.revision = next;
    grant.administrator_mxids = administrators.to_vec();
    grant.allow_self_approval = allow_self_approval;
    grant.validate(&parent, now)?;
    tx.execute(
        "UPDATE project_grants SET config=?2 WHERE id=?1",
        params![id, serialize(&grant)?],
    )?;
    Ok(grant)
}

pub(super) fn revoke_project(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    expected_revision: u64,
    issuer: &Registration,
    now: u64,
) -> Result<(), Error> {
    let (raw, fleet): (String, String) = tx
        .query_row(
            "SELECT config,fleet_id FROM project_grants WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    if issuer != &registration(tx, &fleet)? {
        return Err(Error::GrantAuthority);
    }
    let grant: ProjectGrant = serde_json::from_str(&raw)?;
    if grant.revision != expected_revision {
        return Err(Error::Conflict);
    }
    tx.execute(
        "UPDATE project_grants SET revoked_at=COALESCE(revoked_at,?2) WHERE id=?1",
        params![id, now],
    )?;
    reconcile(tx, now)?;
    Ok(())
}
