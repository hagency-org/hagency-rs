//! Operator-owned resource contributions. Browser data cannot construct the
//! session gate; creation/revocation use the existing capacity transaction.
use super::{DomainRepository, project_grants, read_resource, resource_publication::lock_error};
use crate::{
    Error, ResourcePublicationAccess, ResourcePublicationCommand, ResourcePublicationRetirement,
    resource_publication_revision,
};
use hagency_core::{
    authority::Registration,
    project_grants::{GrantLimits, ResourceDelegation},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::time::Instant;

pub struct ResourceContributionAccess(ResourcePublicationAccess);
impl ResourceContributionAccess {
    pub fn new(expires: Instant, retirement: ResourcePublicationRetirement) -> Self {
        Self(ResourcePublicationAccess::new(expires, retirement))
    }
    pub fn revoke(&self) -> Result<(), Error> {
        self.0.revoke()
    }
    pub fn prepare(
        &self,
        resource: String,
        revision: String,
        mutation: ContributionMutation,
        deadline: Instant,
    ) -> Result<ResourceContributionCommand, Error> {
        // Bound retained queue data before acquiring a session command slot.
        if serde_json::to_vec(&mutation)?.len() > 4096 {
            return Err(Error::Capacity);
        }
        let gate = self.0.prepare(resource, revision, false, deadline)?;
        Ok(ResourceContributionCommand { gate, mutation })
    }
}
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ContributionMutation {
    Contribute { grant: ResourceDelegation },
    Revoke { id: String, expected_revision: u64 },
}
pub struct ResourceContributionCommand {
    gate: ResourcePublicationCommand,
    mutation: ContributionMutation,
}
impl ResourceContributionCommand {
    pub(crate) fn weight(&self) -> u32 {
        4608
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContributionState {
    Active,
    Expired,
    Revoked,
    RegistrationChanged,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContributionStatus {
    pub grant: ResourceDelegation,
    pub state: ContributionState,
    /// Includes retired project reservations. Cleanup is not a budget refund.
    pub reserved: GrantLimits,
}
/// A bounded page of authority observations for one current registration.
/// Missing records on a page are never a withdrawal or a budget refund.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContributionPage {
    pub v: u8,
    pub registration_generation: u64,
    pub observed_at_ms: u64,
    pub after: String,
    pub contributions: Vec<ContributionStatus>,
    pub next_after: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContributionTarget {
    pub fleet_id: String,
    pub registration_generation: u64,
    pub issuer: String,
    pub reception_bound: bool,
}
pub(super) fn status(db: &Connection, id: &str, now: u64) -> Result<ContributionStatus, Error> {
    let (raw, revoked): (String, Option<u64>) = db
        .query_row(
            "SELECT config,revoked_at FROM resource_delegations WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    let grant: ResourceDelegation = serde_json::from_str(&raw)?;
    let registration = project_grants::registration(db, &grant.fleet_id)?;
    let state = if revoked.is_some() {
        ContributionState::Revoked
    } else if grant.registration_generation != registration.generation
        || grant.issuer != registration.server_name
    {
        ContributionState::RegistrationChanged
    } else if grant.expires_at_ms <= now {
        ContributionState::Expired
    } else {
        ContributionState::Active
    };
    let reserved = db.query_row("SELECT COALESCE(SUM(json_extract(config,'$.limits.tokens')),0),COALESCE(SUM(json_extract(config,'$.limits.maxAgents')),0),COALESCE(SUM(json_extract(config,'$.limits.maxRatePerDay')),0) FROM project_grants WHERE delegation_id=?1", [id], |r| Ok(GrantLimits { tokens:r.get(0)?, max_agents:r.get(1)?, max_rate_per_day:r.get(2)? }))?;
    if !grant.limits.contains(&reserved) {
        return Err(Error::Schema);
    }
    Ok(ContributionStatus {
        grant,
        state,
        reserved,
    })
}
impl DomainRepository {
    pub fn contribution_page(
        &self,
        identity: &crate::outbound::RegistrationIdentity,
        after: &str,
        now: u64,
    ) -> Result<ContributionPage, Error> {
        self.check_publication_registration(identity)?;
        if !after.is_empty() {
            hagency_core::project::identifier(after, 128)?;
        }
        let mut ids = self.db.prepare("SELECT id FROM resource_delegations WHERE fleet_id=?1 AND json_extract(config,'$.registrationGeneration')=?2 AND id>?3 ORDER BY id LIMIT 17")?
            .query_map(params![identity.fleet_id, identity.registration_generation, after], |r| r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        let next_after = (ids.len() > 16).then(|| ids[15].clone());
        ids.truncate(16);
        Ok(ContributionPage {
            v: 1,
            registration_generation: identity.registration_generation,
            observed_at_ms: now,
            after: after.into(),
            contributions: ids
                .iter()
                .map(|id| status(&self.db, id, now))
                .collect::<Result<_, _>>()?,
            next_after,
        })
    }
    pub fn contribution_targets(
        &self,
        after: &str,
        limit: usize,
    ) -> Result<Vec<ContributionTarget>, Error> {
        if limit == 0 || limit > 100 {
            return Err(Error::Capacity);
        }
        self.db
            .prepare(
                "SELECT config FROM registrations WHERE fleet_id>?1 ORDER BY fleet_id LIMIT ?2",
            )?
            .query_map(params![after, limit as i64], |r| r.get::<_, String>(0))?
            .map(|raw| {
                let r: Registration = serde_json::from_str(&raw?)?;
                r.validate()?;
                Ok(ContributionTarget {
                    fleet_id: r.fleet_id,
                    registration_generation: r.generation,
                    issuer: r.server_name,
                    reception_bound: !r.reception_room_id.is_empty(),
                })
            })
            .collect()
    }
    pub fn resource_contributions(
        &self,
        resource: &str,
        after: &str,
        limit: usize,
        now: u64,
    ) -> Result<Vec<ContributionStatus>, Error> {
        if limit == 0 || limit > 100 {
            return Err(Error::Capacity);
        }
        read_resource(&self.db, resource)?;
        let ids = self.db.prepare("SELECT id FROM resource_delegations WHERE resource_id=?1 AND id>?2 ORDER BY id LIMIT ?3")?
            .query_map(params![resource,after,limit as i64], |r| r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        ids.iter().map(|id| status(&self.db, id, now)).collect()
    }
    pub fn contribute_resource(
        &mut self,
        command: ResourceContributionCommand,
        now: u64,
    ) -> Result<ContributionStatus, Error> {
        self.contribute_resource_clock(command, || Ok(now), Instant::now)
    }
    pub(crate) fn contribute_resource_clock(
        &mut self,
        command: ResourceContributionCommand,
        wall_clock: impl FnOnce() -> Result<u64, Error>,
        mut clock: impl FnMut() -> Instant,
    ) -> Result<ContributionStatus, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = wall_clock()?;
        let gate = &command.gate;
        let revoked = gate.access.revoked.try_lock().map_err(lock_error)?;
        let check = |at| gate.access.check_at(*revoked, gate.deadline, at);
        check(clock())?;
        let resource = read_resource(&tx, &gate.resource)?;
        if resource_publication_revision(&resource)? != gate.revision {
            return Err(Error::Conflict);
        }
        let id = match &command.mutation {
            ContributionMutation::Contribute { grant } => {
                if grant.resource_id != gate.resource || grant.revision != 1 {
                    return Err(Error::Conflict);
                }
                // Connection proof binds the imported Palpo identity before
                // its administrator is allowed to allocate this contribution.
                let registration: Registration =
                    project_grants::registration(&tx, &grant.fleet_id)?;
                if registration.reception_room_id.is_empty() {
                    return Err(Error::GrantAuthority);
                }
                self.accounts.check_resource(&tx, &resource)?;
                project_grants::delegate(&tx, &self.accounts, grant, now)?;
                &grant.id
            }
            ContributionMutation::Revoke {
                id,
                expected_revision,
            } => {
                if status(&tx, id, now)?.grant.resource_id != gate.resource {
                    return Err(Error::Conflict);
                }
                project_grants::revoke_delegation(&tx, id, *expected_revision, now)?;
                id
            }
        };
        let result = status(&tx, id, now)?;
        if matches!(command.mutation, ContributionMutation::Contribute { .. }) {
            self.accounts.check_resource(&tx, &resource)?;
        }
        check(clock())?;
        tx.commit()?;
        if matches!(command.mutation, ContributionMutation::Contribute { .. }) {
            self.accounts
                .check_resource(&self.db, &resource)
                .map_err(|_| Error::OutcomeUnknown)?;
        }
        check(clock()).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
