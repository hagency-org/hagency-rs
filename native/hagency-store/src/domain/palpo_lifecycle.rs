//! Scoped lifecycle observations; allocation revocation is not cleanup proof.
use super::{DomainRepository, project_grants, read_engagement, serialize};
use crate::Error;
use hagency_core::{
    authority::Registration,
    project::{CleanupState, EngagementState, identifier},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PalpoRetirementTarget {
    pub fleet_id: String,
    pub registration_generation: u64,
    pub engagement_id: String,
    pub request_id: String,
    pub agent_mxid: String,
    pub ended_at: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PalpoAgentLifecycle {
    pub v: u8,
    pub agent_mxid: Option<String>,
    pub local_cleanup: CleanupState,
    pub runtime_stopped: bool,
    pub cleanup_retryable: bool,
    /// Fence of the physical retirement attempt, zero before one is claimed.
    pub cleanup_attempt: u64,
    pub matrix_retired: bool,
    pub ended_at_ms: Option<u64>,
    pub allocated_tokens: u64,
    /// Observed ledger growth is a lower bound, not an exact lifetime bill.
    pub spent_tokens_lower_bound: Option<u64>,
    pub quota_paused: bool,
}

impl DomainRepository {
    pub fn palpo_agent_lifecycle(
        &self,
        issuer: &Registration,
        id: &str,
    ) -> Result<PalpoAgentLifecycle, Error> {
        identifier(id, 128)?;
        if project_grants::registration(&self.db, &issuer.fleet_id)? != *issuer {
            return Err(Error::Generation);
        }
        let belongs: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM engagements WHERE id=?1 AND fleet_id=?2 AND generation=?3)",
            params![id, issuer.fleet_id, issuer.generation], |r| r.get(0))?;
        if !belongs {
            return Err(Error::GrantAuthority);
        }
        let e = read_engagement(&self.db, id)?;
        // Do not synthesize a Matrix ID from an engagement ID or agent name.
        // Retained transport evidence remains readable after the agent ends.
        let agent_mxid: Option<String> = self.db.query_row("SELECT sender_mxid FROM matrix_transports WHERE engagement_id=?1 AND registration_generation=?2 AND server_name=?3",
            params![id, issuer.generation, issuer.server_name], |r| r.get(0)).optional()?;
        let ended_at_ms: Option<u64> = self
            .db
            .query_row(
                "SELECT ended_at FROM engagement_ends WHERE engagement_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let live: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM runner_dispatches d JOIN runner_sessions s ON s.id=d.session_id WHERE s.engagement_id=?1 AND d.state IN ('leased','started','parked'))", [id], |r| r.get(0))?;
        let runtime_stopped = e.state == EngagementState::Revoked
            && matches!(
                e.cleanup,
                CleanupState::Complete | CleanupState::NotRequired
            )
            && !live
            && self.unresolved_dispatches_for_engagement(id)? == 0
            && self.open_agent_fence(id)?.is_none();
        let cleanup_retryable = self.db.query_row("SELECT EXISTS(SELECT 1 FROM effects WHERE engagement_id=?1 AND kind='retire' AND state='failed')", [id], |r| r.get(0))?;
        let cleanup_attempt = self
            .db
            .query_row(
                "SELECT fence FROM effects WHERE engagement_id=?1 AND kind='retire'",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let target =
            agent_mxid
                .as_ref()
                .zip(ended_at_ms)
                .map(|(mxid, ended)| PalpoRetirementTarget {
                    fleet_id: issuer.fleet_id.clone(),
                    registration_generation: issuer.generation,
                    engagement_id: id.into(),
                    request_id: e.request_id.clone(),
                    agent_mxid: mxid.clone(),
                    ended_at: ended,
                });
        let matrix_retired = match target {
            Some(target) => self.db.query_row("SELECT EXISTS(SELECT 1 FROM palpo_agent_retirements WHERE engagement_id=?1 AND generation=?2 AND target=?3)",
                params![id, issuer.generation, serialize(&target)?], |r| r.get(0))?,
            None => false,
        };
        let quota = self.quota_status(id)?;
        Ok(PalpoAgentLifecycle {
            v: 1,
            agent_mxid,
            local_cleanup: e.cleanup,
            runtime_stopped,
            cleanup_retryable,
            cleanup_attempt,
            matrix_retired,
            ended_at_ms,
            allocated_tokens: quota.allocated_tokens,
            spent_tokens_lower_bound: quota.spent_tokens,
            quota_paused: quota.paused,
        })
    }

    pub fn palpo_retirement_target(
        &self,
        issuer: &Registration,
        id: &str,
    ) -> Result<PalpoRetirementTarget, Error> {
        let view = self.palpo_agent_lifecycle(issuer, id)?;
        if !view.runtime_stopped {
            return Err(Error::State);
        }
        let e = read_engagement(&self.db, id)?;
        Ok(PalpoRetirementTarget {
            fleet_id: issuer.fleet_id.clone(),
            registration_generation: issuer.generation,
            engagement_id: id.into(),
            request_id: e.request_id,
            agent_mxid: view.agent_mxid.ok_or(Error::State)?,
            ended_at: view.ended_at_ms.ok_or(Error::State)?,
        })
    }

    /// Host-only: called after the fixed Palpo endpoint confirms every remote
    /// retirement fact. Recheck the original target after the network await.
    pub fn confirm_palpo_retirement(
        &mut self,
        issuer: &Registration,
        target: &PalpoRetirementTarget,
        now: u64,
    ) -> Result<(), Error> {
        hagency_core::tasks::clock(now)?;
        // The target recheck and receipt must see one registration/custody
        // snapshot even if another database connection rotates the fleet.
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if self.palpo_retirement_target(issuer, &target.engagement_id)? != *target {
            return Err(Error::Conflict);
        }
        tx.execute("INSERT INTO palpo_agent_retirements(engagement_id,generation,target,confirmed_at) VALUES(?1,?2,?3,?4) ON CONFLICT(engagement_id) DO NOTHING",
            params![target.engagement_id, issuer.generation, serialize(target)?, now])?;
        let confirmed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM palpo_agent_retirements WHERE engagement_id=?1 AND generation=?2 AND target=?3)",
            params![target.engagement_id, issuer.generation, serialize(target)?], |r| r.get(0))?;
        if !confirmed {
            return Err(Error::Conflict);
        }
        tx.commit()?;
        Ok(())
    }
}
