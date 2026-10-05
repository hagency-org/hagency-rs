//! Explicit resource-owner reconciliation of final account usage. Host usage
//! observations remain lower bounds and cannot silently authorize a refund.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinalUsage {
    pub command_id: contract::CommandId,
    pub agent_allocation_id: contract::AgentAllocationId,
    pub resource_allocation_id: contract::ResourceAllocationId,
    pub expected_allocated_tokens: contract::Tokens,
    pub consumed_tokens: contract::Tokens,
    pub period: String,
    pub period_key: String,
    /// Bounded non-secret accounting reference, retained in the private audit.
    pub evidence_reference: String,
}
pub struct SettlementCommand {
    pub(crate) usage: FinalUsage,
    pub(crate) gate: crate::ResourcePublicationCommand,
}
impl SettlementCommand {
    pub(crate) fn weight(&self) -> Result<u32, Error> {
        u32::try_from(serialize(&self.usage)?.len() + 256).map_err(|_| Error::Capacity)
    }
}
impl DomainRepository {
    pub fn settle_coordinator_agent(
        &mut self,
        command: SettlementCommand,
        now: u64,
    ) -> Result<Value, Error> {
        let gate = &command.gate;
        let revoked = gate
            .access
            .revoked
            .try_lock()
            .map_err(resource_publication::lock_error)?;
        let check = || {
            gate.access
                .check_at(*revoked, gate.deadline, std::time::Instant::now())
        };
        check()?;
        let input = &command.usage;
        if input.evidence_reference.trim().is_empty()
            || input.evidence_reference.len() > 256
            || input.evidence_reference.chars().any(char::is_control)
        {
            return Err(Error::Invalid(InvalidInput(
                "final accounting reference required",
            )));
        }
        let digest = canonical::digest(&serde_json::to_value(input)?)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = input.agent_allocation_id.as_str();
        let old:Option<(String,String)>=tx.query_row("SELECT digest,receipt FROM coordinator_settlements WHERE agent_id=?1 OR command_id=?2",params![id,input.command_id.as_str()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((prior, receipt)) = old {
            return if prior == digest {
                Ok(serde_json::from_str(&receipt)?)
            } else {
                Err(Error::Conflict)
            };
        }
        let agent = read_engagement(&tx, id)?;
        if agent.state != EngagementState::Revoked
            || !matches!(
                agent.cleanup,
                CleanupState::Complete | CleanupState::NotRequired
            )
        {
            return Err(Error::State);
        }
        let unsettled:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runner_dispatches d JOIN runner_sessions s ON s.id=d.session_id WHERE s.engagement_id=?1 AND d.state IN ('queued','leased','started','parked','outcome_unknown')) OR EXISTS(SELECT 1 FROM runner_attempts a JOIN runner_dispatches d ON d.id=a.dispatch_id JOIN runner_sessions s ON s.id=d.session_id WHERE s.engagement_id=?1 AND a.outcome NOT IN ('completed','spawn_failed','unstarted_requeued','cancelled_unstarted') AND NOT EXISTS(SELECT 1 FROM dispatch_stops z WHERE z.dispatch_id=a.dispatch_id AND z.fence=a.fence AND z.settled_at IS NOT NULL)) OR EXISTS(SELECT 1 FROM agent_fences WHERE engagement_id=?1 AND cleared_at IS NULL) OR EXISTS(SELECT 1 FROM dispatch_stops p JOIN runner_dispatches d ON d.id=p.dispatch_id JOIN runner_sessions s ON s.id=d.session_id WHERE s.engagement_id=?1 AND p.settled_at IS NULL)",[id],|r|r.get(0))?;
        if unsettled {
            return Err(Error::State);
        }
        let (grant,allocated):(String,u64)=tx.query_row("SELECT resource_allocation_id,allocated_tokens FROM coordinator_agents WHERE agent_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or(Error::NotFound)?;
        let (period, key): (String, String) = tx.query_row(
            "SELECT period,period_key FROM coordinator_resources WHERE id=?1",
            [&grant],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let consumed = u64::from(input.consumed_tokens);
        if grant != input.resource_allocation_id.as_str()
            || allocated != u64::from(input.expected_allocated_tokens)
            || period != input.period
            || key != input.period_key
            || consumed < quota_holds::spend(&tx, id)?.unwrap_or(0)
        {
            return Err(Error::Conflict);
        }
        let receipt = json!({"agentAllocationId":id,"resourceAllocationId":grant,"allocatedTokens":allocated,"consumedTokens":consumed,
            "releasedTokens":allocated.saturating_sub(consumed),"period":period,"periodKey":key,
            "state":"settled","evidence":"owner_account_reconciliation","observedAtMs":now});
        let mut private_receipt = receipt.clone();
        private_receipt["evidenceReference"] = json!(input.evidence_reference);
        tx.execute("INSERT INTO coordinator_settlements(agent_id,command_id,digest,receipt,accepted_at) VALUES(?1,?2,?3,?4,?5)",params![id,input.command_id.as_str(),digest,serialize(&private_receipt)?,now])?;
        tx.execute(
            "UPDATE coordinator_agents SET retained_tokens=?2 WHERE agent_id=?1",
            params![id, consumed],
        )?;
        check()?;
        tx.commit()?;
        check().map_err(|_| Error::OutcomeUnknown)?;
        Ok(private_receipt)
    }
    pub fn coordinator_settlement(&self, id: &str) -> Result<Value, Error> {
        let raw: Option<String> = self
            .db
            .query_row(
                "SELECT receipt FROM coordinator_settlements WHERE agent_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(raw) = raw {
            let mut value: Value = serde_json::from_str(&raw)?;
            value.as_object_mut().unwrap().remove("evidenceReference");
            let observed = quota_holds::spend(&self.db, id)?.unwrap_or(0);
            let final_tokens = value["consumedTokens"].as_u64().ok_or(Error::Schema)?;
            value["lateUsageTokens"] = json!(observed.saturating_sub(final_tokens));
            if observed > final_tokens {
                value["state"] = json!("late_usage_charged");
            }
            return Ok(value);
        }
        let (grant,allocated,period,key):(String,u64,String,String)=self.db.query_row("SELECT c.resource_allocation_id,c.allocated_tokens,r.period,r.period_key FROM coordinator_agents c JOIN coordinator_resources r ON r.id=c.resource_allocation_id WHERE c.agent_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?.ok_or(Error::NotFound)?;
        let agent = read_engagement(&self.db, id)?;
        Ok(
            json!({"state":"awaiting_final_usage","agentAllocationId":id,"resourceAllocationId":grant,"allocatedTokens":allocated,
            "period":period,"periodKey":key,"observedLowerBound":quota_holds::spend(&self.db,id)?,
            "canSettle":agent.state==EngagementState::Revoked && matches!(agent.cleanup,CleanupState::Complete|CleanupState::NotRequired)}),
        )
    }
}
