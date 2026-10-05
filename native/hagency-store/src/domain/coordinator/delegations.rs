use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegationChange {
    pub server_engagement_id: contract::ServerEngagementId,
    pub expected_revision: contract::Revision,
    pub coordinator_mxid: contract::MatrixUserId,
    pub delegation_expires_at_ms: u64,
    pub allow_self_approval: bool,
    pub state: DelegationState,
    pub export_mxids: Vec<contract::MatrixUserId>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationState {
    Active,
    Suspended,
    Revoked,
}

pub struct DelegationCommand {
    pub(crate) change: DelegationChange,
    pub(crate) gate: crate::ResourcePublicationCommand,
}
impl DelegationCommand {
    pub(crate) fn weight(&self) -> Result<u32, Error> {
        u32::try_from(serialize(&self.change)?.len() + 256).map_err(|_| Error::Capacity)
    }
}
impl DomainRepository {
    pub fn coordinator_authority(&self, fleet: &str) -> Result<Option<ServerEngagement>, Error> {
        binding(&self.db, fleet)
    }
    pub fn change_coordinator(
        &mut self,
        command: DelegationCommand,
        now: u64,
    ) -> Result<ServerEngagement, Error> {
        let change = &command.change;
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
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let fleet = change.server_engagement_id.as_str();
        let digest = canonical::digest(&serde_json::to_value(change)?)?;
        let revision = u64::from(change.expected_revision)
            .checked_add(1)
            .ok_or(Error::Capacity)?;
        let previous: Option<(String,String)> = tx.query_row("SELECT digest,authority FROM coordinator_delegations WHERE engagement_id=?1 AND revision=?2",params![fleet,revision],|r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((old, receipt)) = previous {
            if old != digest {
                return Err(Error::Conflict);
            }
            return Ok(serde_json::from_str(&receipt)?);
        }
        let mut value = binding(&tx, fleet)?.ok_or(Error::NotFound)?;
        if value.delegation_revision != change.expected_revision
            || value.state == contract::EngagementState::Revoked
        {
            return Err(Error::Generation);
        }
        if !change.coordinator_mxid.belongs_to(&value.server)
            || change.export_mxids.len() > 2
            || change
                .export_mxids
                .iter()
                .any(|u| u != &value.owner && u != &change.coordinator_mxid)
            || change
                .export_mxids
                .iter()
                .map(|u| u.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != change.export_mxids.len()
            || change.delegation_expires_at_ms > JSON_SAFE_MAX
            || matches!(change.state, DelegationState::Active)
                && (change.delegation_expires_at_ms <= now
                    || change.delegation_expires_at_ms > now.saturating_add(366 * 86400000))
        {
            return Err(Error::Invalid(InvalidInput("invalid delegation")));
        }
        if !matches!(change.state, DelegationState::Revoked)
            && !matches!(
                value.state,
                contract::EngagementState::Verified | contract::EngagementState::Suspended
            )
        {
            return Err(Error::State);
        }
        value.coordinator = change.coordinator_mxid.clone();
        value.delegation_revision = revision.try_into().map_err(policy)?;
        value.delegation_expires_at_ms = change.delegation_expires_at_ms;
        value.allow_self_approval = change.allow_self_approval;
        value.state = match change.state {
            DelegationState::Active => contract::EngagementState::Verified,
            DelegationState::Suspended => contract::EngagementState::Suspended,
            DelegationState::Revoked => contract::EngagementState::Revoked,
        };
        Self::configure_coordinator_transaction(&tx, &value)?;
        tx.execute("INSERT INTO coordinator_delegations(engagement_id,revision,digest,change,authority,accepted_at) VALUES(?1,?2,?3,?4,?5,?6)",params![fleet,revision,digest,serialize(change)?,serialize(&value)?,now])?;
        publish(
            &tx,
            fleet,
            "engagement_authority",
            json!({"kind":"engagement","engagement":value,"exportMxids":change.export_mxids,"observedAtMs":now}),
        )?;
        check()?;
        tx.commit()?;
        check().map_err(|_| Error::OutcomeUnknown)?;
        Ok(value)
    }
}
