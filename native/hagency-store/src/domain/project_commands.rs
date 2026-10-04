//! One canonical transaction for a business command and its immutable result.
use super::{DomainRepository, accounts, project_grants as grants, serialize};
use crate::Error;
use hagency_core::{
    authority::{Registration, VerifiedRequest},
    canonical,
    project::{Engagement, identifier},
    project_commands::*,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

fn receipt(
    tx: &rusqlite::Connection,
    command: &ProjectCommand,
) -> Result<Option<ProjectReceipt>, Error> {
    let old: Option<(String, String)> = tx.query_row(
        "SELECT digest,receipt FROM project_command_receipts WHERE fleet_id=?1 AND generation=?2 AND command_id=?3",
        params![command.fleet_id,command.registration_generation,command.command_id], |r| Ok((r.get(0)?,r.get(1)?)),
    ).optional()?;
    old.map(|(digest, raw)| {
        if digest != command.digest()? {
            return Err(Error::Conflict);
        }
        Ok(serde_json::from_str(&raw)?)
    })
    .transpose()
}
fn agent_result(agent: Engagement) -> Result<ProjectResult, Error> {
    Ok(ProjectResult::Agent {
        engagement_id: agent.id,
        state: serde_json::to_value(agent.state)?
            .as_str()
            .ok_or(Error::Schema)?
            .into(),
        allocated_tokens: u64::from(agent.allocated_tokens.unwrap_or(agent.requested_tokens)),
        cleanup: serde_json::to_value(agent.cleanup)?
            .as_str()
            .ok_or(Error::Schema)?
            .into(),
    })
}
fn refusal(error: &Error) -> Option<ProjectRefusal> {
    use ProjectRefusal as R;
    Some(match error {
        Error::GrantExpired => R::GrantExpired,
        Error::GrantRevoked => R::GrantRevoked,
        Error::GrantAuthority | Error::Generation | Error::RunnerAuthority => R::Authority,
        Error::Conflict => R::Conflict,
        Error::NotFound => R::NotFound,
        Error::InsufficientCapacity | Error::OverCommit { .. } | Error::NoCeiling => {
            R::InsufficientCapacity
        }
        Error::Unqualified => R::ResourceUnavailable,
        Error::Invalid(_) => R::Invalid,
        Error::State => R::State,
        _ => return None, // DB/credential availability or unknown outcomes must be retried, never terminalized.
    })
}
fn execute(
    tx: &Transaction<'_>,
    accounts: &accounts::Registry,
    command: &ProjectCommand,
    issuer: &Registration,
    proof: Option<&VerifiedRequest>,
    now: u64,
) -> Result<ProjectResult, Error> {
    // The transport carries the external command ID; a domain ID is namespaced
    // by fleet/generation so another fleet cannot collide with its decisions.
    let id = format!(
        "workflow_{}",
        canonical::digest(&serde_json::json!([
            command.fleet_id,
            command.registration_generation,
            command.command_id
        ]))?
    );
    let scope = |grant_id: &str, revision: u64, requester: &str| grants::ProjectAgentDecision {
        grant_id: grant_id.into(),
        grant_revision: revision,
        actor_mxid: command.actor_mxid.clone(),
        requester_mxid: requester.into(),
    };
    use ProjectOperation::*;
    let grant_id = match &command.operation {
        ReserveProject { .. } => None,
        AssignProjectAdmins { grant_id, .. }
        | ApproveAgent { grant_id, .. }
        | RejectAgent { grant_id, .. }
        | TopUpAgent { grant_id, .. }
        | RevokeAgent { grant_id, .. }
        | RevokeProject { grant_id, .. } => Some(grant_id),
    };
    if let Some(grant_id) = grant_id {
        let fleet: String = tx
            .query_row(
                "SELECT fleet_id FROM project_grants WHERE id=?1",
                [grant_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        if fleet != issuer.fleet_id {
            return Err(Error::GrantAuthority);
        }
    }
    match &command.operation {
        ReserveProject { grant } => Ok(ProjectResult::Grant {
            grant: grants::reserve(tx, grant, issuer, now)?,
        }),
        AssignProjectAdmins {
            grant_id,
            expected_revision,
            administrators,
            allow_self_approval,
        } => Ok(ProjectResult::Grant {
            grant: grants::assign(
                tx,
                grant_id,
                *expected_revision,
                administrators,
                *allow_self_approval,
                issuer,
                now,
            )?,
        }),
        ApproveAgent {
            grant_id,
            grant_revision,
            request,
            ..
        }
        | RejectAgent {
            grant_id,
            grant_revision,
            request,
        } => {
            let proof = proof.ok_or(Error::GrantAuthority)?;
            if proof.request().digest()? != request.digest()? || proof.registration() != issuer {
                return Err(Error::GrantAuthority);
            }
            let scoped = scope(grant_id, *grant_revision, &request.requester_mxid);
            // No pending console row survives a failed project decision. Fresh
            // admission, grant debit and result are one business transaction.
            grants::validate_approval(tx, &scoped, proof, now)?;
            super::admit_in_transaction(tx, accounts, proof, now)?;
            match &command.operation {
                ApproveAgent {
                    allocated_tokens, ..
                } => agent_result(super::approve_in_transaction(
                    tx,
                    accounts,
                    &id,
                    proof,
                    now,
                    Some(*allocated_tokens),
                    Some(&scoped),
                )?),
                _ => {
                    super::authority(tx, proof, now)?;
                    super::project_authority(tx, proof)?;
                    grants::validate_approval(tx, &scoped, proof, now)?;
                    agent_result(super::end_transaction(
                        tx,
                        &id,
                        &request.engagement_id()?,
                        false,
                        now,
                    )?)
                }
            }
        }
        TopUpAgent {
            grant_id,
            grant_revision,
            engagement_id,
            requester_mxid,
            add_tokens,
        } => agent_result(super::raise_in_transaction(
            tx,
            &id,
            engagement_id,
            *add_tokens,
            now,
            Some(&scope(grant_id, *grant_revision, requester_mxid)),
        )?),
        RevokeAgent {
            grant_id,
            grant_revision,
            engagement_id,
        } => {
            // Cleanup remains authorized after expiry/revocation, but the actor,
            // exact project membership and current registration must still match.
            let (raw, fleet): (String, String) = tx
                .query_row(
                    "SELECT config,fleet_id FROM project_grants WHERE id=?1",
                    [grant_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .ok_or(Error::NotFound)?;
            let grant: hagency_core::project_grants::ProjectGrant = serde_json::from_str(&raw)?;
            let member: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM project_grant_agents WHERE engagement_id=?1 AND grant_id=?2)", params![engagement_id, grant_id], |r| r.get(0))?;
            if fleet != command.fleet_id
                || grant.revision != *grant_revision
                || !member
                || (command.actor_mxid != grant.owner_mxid
                    && !grant.administrator_mxids.contains(&command.actor_mxid))
            {
                return Err(Error::GrantAuthority);
            }
            let agent = super::read_engagement(tx, engagement_id)?;
            if agent.state == hagency_core::project::EngagementState::Revoked {
                return agent_result(agent);
            }
            agent_result(super::end_transaction(tx, &id, engagement_id, true, now)?)
        }
        RevokeProject {
            grant_id,
            expected_revision,
        } => {
            grants::revoke_project(tx, grant_id, *expected_revision, issuer, now)?;
            Ok(ProjectResult::RevokedProject {
                grant_id: grant_id.clone(),
                revision: *expected_revision,
            })
        }
    }
}
impl DomainRepository {
    pub fn palpo_status_page(
        &self,
        issuer: &Registration,
        after: &str,
        limit: usize,
    ) -> Result<Vec<Engagement>, Error> {
        if limit == 0 || limit > 100 {
            return Err(Error::Capacity);
        }
        if grants::registration(&self.db, &issuer.fleet_id)? != *issuer {
            return Err(Error::Generation);
        }
        self.db.prepare("SELECT projection FROM engagements WHERE fleet_id=?1 AND generation=?2 AND id>?3 ORDER BY id LIMIT ?4")?
            .query_map(params![issuer.fleet_id,issuer.generation,after,limit as i64], |r| r.get::<_,String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
    /// Durable evidence that Palpo already knows this request. A process-local
    /// probe file is not the source of truth for project-command status updates.
    pub fn is_project_command_agent(&self, issuer: &Registration, id: &str) -> Result<bool, Error> {
        if grants::registration(&self.db, &issuer.fleet_id)? != *issuer {
            return Err(Error::Generation);
        }
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM project_command_receipts WHERE fleet_id=?1 AND generation=?2 AND json_extract(receipt,'$.outcome.result.engagementId')=?3)",
            params![issuer.fleet_id,issuer.generation,id], |r| r.get(0),
        )?)
    }
    pub fn project_command_receipt(
        &self,
        command: &ProjectCommand,
        issuer: &Registration,
    ) -> Result<Option<ProjectReceipt>, Error> {
        command.validate(issuer)?;
        if grants::registration(&self.db, &issuer.fleet_id)? != *issuer {
            return Err(Error::Generation);
        }
        receipt(&self.db, command)
    }
    pub fn apply_project_command(
        &mut self,
        command: &ProjectCommand,
        issuer: &Registration,
        authorization: &ProjectAuthorization,
        proof: Option<&VerifiedRequest>,
        now: u64,
    ) -> Result<ProjectReceipt, Error> {
        self.apply_project_command_clock(command, issuer, authorization, proof, || Ok(now))
    }
    pub(crate) fn apply_project_command_clock(
        &mut self,
        command: &ProjectCommand,
        issuer: &Registration,
        authorization: &ProjectAuthorization,
        proof: Option<&VerifiedRequest>,
        clock: impl FnOnce() -> Result<u64, Error>,
    ) -> Result<ProjectReceipt, Error> {
        command.validate(issuer)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = clock()?;
        if grants::registration(&tx, &issuer.fleet_id)? != *issuer {
            return Err(Error::Generation);
        }
        if let Some(receipt) = receipt(&tx, command)? {
            return Ok(receipt);
        }
        let count: u64 =
            tx.query_row("SELECT COUNT(*) FROM project_command_receipts", [], |r| {
                r.get(0)
            })?;
        if count >= 100_000 {
            return Err(Error::Capacity);
        }
        let outcome = if command.expires_at_ms <= now {
            ProjectOutcome::Refused {
                code: ProjectRefusal::Expired,
            }
        } else {
            // A stale execution lease is retriable with a fresh server check;
            // the business command itself is not yet definitively refused.
            authorization
                .validate(command, now)
                .map_err(|_| Error::OutcomeUnknown)?;
            if !authorization.allowed {
                ProjectOutcome::Refused {
                    code: ProjectRefusal::Authority,
                }
            } else {
                tx.execute_batch("SAVEPOINT project_operation")?;
                match execute(&tx, &self.accounts, command, issuer, proof, now) {
                    Ok(result) => {
                        tx.execute_batch("RELEASE project_operation")?;
                        ProjectOutcome::Applied { result }
                    }
                    Err(error) => {
                        let Some(code) = refusal(&error) else {
                            return Err(error);
                        };
                        tx.execute_batch(
                            "ROLLBACK TO project_operation; RELEASE project_operation",
                        )?;
                        ProjectOutcome::Refused { code }
                    }
                }
            }
        };
        let value = ProjectReceipt {
            v: 1,
            fleet_id: command.fleet_id.clone(),
            registration_generation: command.registration_generation,
            command_id: command.command_id.clone(),
            command_digest: command.digest()?,
            completed_at_ms: now,
            outcome,
        };
        tx.execute("INSERT INTO project_command_receipts(fleet_id,generation,command_id,digest,receipt) VALUES(?1,?2,?3,?4,?5)",
            params![value.fleet_id,value.registration_generation,value.command_id,value.command_digest,serialize(&value)?])?;
        tx.commit()?;
        Ok(value)
    }
    pub fn pending_project_receipts(
        &self,
        issuer: &Registration,
    ) -> Result<Vec<ProjectReceipt>, Error> {
        if grants::registration(&self.db, &issuer.fleet_id)? != *issuer {
            return Err(Error::Generation);
        }
        self.db.prepare("SELECT receipt FROM project_command_receipts WHERE fleet_id=?1 AND generation=?2 AND published=0 ORDER BY rowid LIMIT 8")?
            .query_map(params![issuer.fleet_id,issuer.generation], |r| r.get::<_,String>(0))?
            .map(|raw| Ok(serde_json::from_str(&raw?)?)).collect()
    }
    pub fn mark_project_receipts_published(
        &mut self,
        issuer: &Registration,
        receipts: &[ProjectReceipt],
    ) -> Result<(), Error> {
        if receipts.len() > 8 {
            return Err(Error::Capacity);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if grants::registration(&tx, &issuer.fleet_id)? != *issuer {
            return Err(Error::Generation);
        }
        for receipt in receipts {
            identifier(&receipt.command_id, 128)?;
            if receipt.fleet_id != issuer.fleet_id
                || receipt.registration_generation != issuer.generation
            {
                return Err(Error::Generation);
            }
            tx.execute("UPDATE project_command_receipts SET published=1 WHERE fleet_id=?1 AND generation=?2 AND command_id=?3 AND digest=?4 AND receipt=?5",params![issuer.fleet_id,issuer.generation,receipt.command_id,receipt.command_digest,serialize(receipt)?])?;
        }
        tx.commit()?;
        Ok(())
    }
}
