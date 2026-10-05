//! Local owner adoption preserves existing allocations, decisions and usage.
//! It never constructs a historical coordinator approval or provisions an agent.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyProject {
    pub grant: ProjectGrant,
    pub definition: ProjectDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyAdoption {
    pub version: u8,
    pub id: contract::CommandId,
    pub source_digest: contract::DefinitionDigest,
    pub server_engagement_id: contract::ServerEngagementId,
    pub registration_generation: contract::Revision,
    pub delegation_revision: contract::Revision,
    pub resource_owner: contract::MatrixUserId,
    pub resources: Vec<ResourceGrant>,
    pub projects: Vec<LegacyProject>,
    /// Original native agent allocation ID -> explicit child resource grant.
    pub agents: BTreeMap<String, contract::ResourceAllocationId>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MigratedAuthority {
    legacy_adoption: String,
    source_digest: String,
    request: contract::AgentRequest,
    original_decisions: Vec<Value>,
}

pub(super) fn approved_request(raw: &str) -> Result<contract::AgentRequest, Error> {
    let value: Value = serde_json::from_str(raw)?;
    if value.get("legacyAdoption").is_some() {
        Ok(serde_json::from_value::<MigratedAuthority>(value)?.request)
    } else {
        Ok(serde_json::from_value::<AgentApproval>(value)?.request)
    }
}

fn inventory(db: &Connection) -> Result<Value, Error> {
    let mut tables = Vec::new();
    let mut schema =
        db.prepare("SELECT name,sql FROM sqlite_master WHERE type='table' ORDER BY name")?;
    for table in schema.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })? {
        let (name, sql) = table?;
        if !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            return Err(Error::Schema);
        }
        let mut query = db.prepare(&format!("SELECT * FROM \"{name}\" ORDER BY rowid"))?;
        let columns = query.column_count();
        let mut rows = query.query([])?;
        let mut hash = Sha256::new();
        let mut count = 0u64;
        while let Some(row) = rows.next()? {
            count = count.checked_add(1).ok_or(Error::Capacity)?;
            for column in 0..columns {
                use rusqlite::types::ValueRef;
                let (kind, bytes) = match row.get_ref(column)? {
                    ValueRef::Null => (0, Vec::new()),
                    ValueRef::Integer(value) => (1, value.to_le_bytes().to_vec()),
                    ValueRef::Real(value) => (2, value.to_bits().to_le_bytes().to_vec()),
                    ValueRef::Text(value) => (3, value.to_vec()),
                    ValueRef::Blob(value) => (4, value.to_vec()),
                };
                hash.update([kind]);
                hash.update((bytes.len() as u64).to_le_bytes());
                hash.update(bytes);
            }
        }
        tables.push(json!({"table":name,"rows":count,"columns":columns,"schemaDigest":canonical::digest(&json!(sql))?,"digest":format!("{:x}",hash.finalize())}));
    }
    Ok(
        json!({"version":1,"schemaVersion":DOMAIN_SCHEMA_VERSION,"sourceDigest":canonical::digest(&json!(tables))?,"tables":tables}),
    )
}

impl DomainRepository {
    /// Offline CLI only. The domain owner lock excludes an active runtime.
    /// Only counts/digests leave the store, never resource secrets or contexts.
    pub fn coordinator_migration_inventory(&self) -> Result<Value, Error> {
        inventory(&self.db)
    }

    pub fn adopt_legacy_allocations(
        &mut self,
        plan: &LegacyAdoption,
        now: u64,
    ) -> Result<Value, Error> {
        if plan.version != 1
            || plan.resources.is_empty()
            || plan.resources.len() > 64
            || plan.projects.len() > 1000
            || plan.agents.len() > 1000
        {
            return Err(Error::Capacity);
        }
        let fingerprint = canonical::digest(&serde_json::to_value(plan)?)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old: Option<(String, String)> = tx
            .query_row(
                "SELECT digest,receipt FROM coordinator_migrations WHERE id=?1",
                [plan.id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((digest, receipt)) = old {
            return if digest == fingerprint {
                Ok(serde_json::from_str(&receipt)?)
            } else {
                Err(Error::Conflict)
            };
        }
        let fleet = plan.server_engagement_id.as_str();
        let authority = current(&tx, fleet, now)?;
        if authority.owner != plan.resource_owner
            || authority.registration_generation != plan.registration_generation
            || authority.delegation_revision != plan.delegation_revision
            || inventory(&tx)?["sourceDigest"] != String::from(plan.source_digest.clone())
        {
            return Err(Error::Generation);
        }
        let mut grant_ids = BTreeSet::new();
        let mut resource_rows = BTreeMap::new();
        for grant in &plan.resources {
            if grant.server_engagement_id != plan.server_engagement_id
                || !grant_ids.insert(grant.id.as_str())
                || grant.eligible_managers.len() > 64
                || grant
                    .eligible_managers
                    .iter()
                    .map(|m| m.as_str())
                    .collect::<BTreeSet<_>>()
                    .len()
                    != grant.eligible_managers.len()
                || grant
                    .eligible_managers
                    .iter()
                    .any(|m| !m.belongs_to(&authority.server))
            {
                return Err(Error::LocalAuthority);
            }
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM coordinator_resources WHERE id=?1)",
                [grant.id.as_str()],
                |r| r.get(0),
            )?;
            if exists {
                return Err(Error::Conflict);
            }
            let resource = read_resource(&tx, &grant.resource_id)?;
            self.accounts.check_resource(&tx, &resource)?;
            let (period, key) = period(&resource, now)?;
            bounded_row(&tx, "coordinator_resources", "id", grant.id.as_str(), 2000)?;
            tx.execute("INSERT INTO coordinator_resources(id,engagement_id,resource_id,preset_id,seat_id,revision,allocated_tokens,period,period_key,managers) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![grant.id.as_str(),fleet,grant.resource_id,resource.preset_id,resource.seat_id,u64::from(grant.revision),u64::from(grant.allocated_tokens),period,key,serialize(&grant.eligible_managers)?])?;
            resource_rows.insert(grant.id.as_str(), (resource, period, key));
        }
        let mut projects = BTreeMap::new();
        for project in &plan.projects {
            let grant = &project.grant;
            if grant.server_engagement_id != plan.server_engagement_id
                || !grant.owner.belongs_to(&authority.server)
                || grant.resource_allocations.is_empty()
                || grant
                    .resource_allocations
                    .iter()
                    .any(|id| !grant_ids.contains(id.as_str()))
                || projects
                    .insert(grant.project_id.as_str(), project)
                    .is_some()
            {
                return Err(Error::LocalAuthority);
            }
            // A migrated Ready project needs a pre-existing native, verified
            // owner/room binding. No room, owner or grant is guessed from an ID.
            let (generation, room, owner, private): (u64,String,String,String) = tx.query_row("SELECT generation,room_id,owner_mxid,owner_room_id FROM projects WHERE fleet_id=?1 AND id=?2",params![fleet,grant.project_id.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?.ok_or(Error::NotFound)?;
            if generation != u64::from(plan.registration_generation)
                || owner != grant.owner.as_str()
                || room != project.definition.room_id
                || private != project.definition.owner_dm_room_id
                || grant.state != contract::ProjectState::Ready
            {
                return Err(Error::Generation);
            }
            for id in &grant.resource_allocations {
                if !plan
                    .resources
                    .iter()
                    .find(|r| r.id == *id)
                    .unwrap()
                    .eligible_managers
                    .contains(&grant.owner)
                {
                    return Err(Error::LocalAuthority);
                }
            }
            let old: Option<(String,String)> = tx.query_row("SELECT grant,definition FROM coordinator_projects WHERE engagement_id=?1 AND id=?2",params![fleet,grant.project_id.as_str()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if old.is_some() {
                return Err(Error::Conflict);
            }
            tx.execute("INSERT INTO coordinator_projects(engagement_id,id,grant,definition) VALUES(?1,?2,?3,?4)", params![fleet,grant.project_id.as_str(),serialize(grant)?,serialize(&project.definition)?])?;
        }
        let mut adopted = Vec::new();
        for (agent_id, grant_id) in &plan.agents {
            let agent = read_engagement(&tx, agent_id)?;
            let (original_fleet, generation, context): (String, u64, String) = tx.query_row(
                "SELECT fleet_id,generation,context FROM engagements WHERE id=?1",
                [agent_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let context: Value = serde_json::from_str(&context)?;
            let project = projects
                .get(agent.project_id.as_str())
                .ok_or(Error::NotFound)?;
            let resource = resource_rows
                .get(grant_id.as_str())
                .ok_or(Error::NotFound)?;
            if original_fleet != fleet
                || generation != u64::from(plan.registration_generation)
                || agent.resource_id != resource.0.id()
                || !project.grant.resource_allocations.contains(grant_id)
                || !matches!(
                    agent.state,
                    EngagementState::Reserved
                        | EngagementState::Active
                        | EngagementState::Revoked
                        | EngagementState::Failed
                )
                || context["ownerMxid"] != project.grant.owner.as_str()
            {
                return Err(Error::Conflict);
            }
            let mut query = tx.prepare("SELECT id,digest,result,kind,at FROM decisions WHERE json_extract(result,'$.id')=?1 ORDER BY id")?;
            let originals = query.query_map([agent_id], |r|Ok(json!({"id":r.get::<_,String>(0)?,"digest":r.get::<_,String>(1)?,"result":r.get::<_,String>(2)?,"kind":r.get::<_,Option<String>>(3)?,"at":r.get::<_,Option<u64>>(4)?})))?.collect::<Result<Vec<_>,_>>()?;
            let was_allocated = originals.iter().any(|row| {
                serde_json::from_str::<Value>(row["result"].as_str().unwrap_or_default())
                    .is_ok_and(|v| matches!(v["state"].as_str(), Some("reserved" | "active")))
            });
            if !was_allocated {
                return Err(Error::LocalAuthority);
            }
            let request: contract::AgentRequest = serde_json::from_value(
                json!({"id":agent.request_id,"revision":1,"serverEngagementId":fleet,
                "projectId":agent.project_id,"projectRevision":project.grant.revision,"resourceAllocationId":grant_id,
                "projectOwner":project.grant.owner,"requester":context["requesterMxid"],"definitionDigest":canonical::digest(&context)?,"requestedTokens":agent.requested_tokens}),
            )?;
            let decision_references = originals.iter().map(|row| json!({"id":row["id"],"digest":row["digest"],"kind":row["kind"],"at":row["at"]})).collect::<Vec<_>>();
            let stored = MigratedAuthority {
                legacy_adoption: plan.id.as_str().into(),
                source_digest: plan.source_digest.clone().into(),
                request: request.clone(),
                original_decisions: originals,
            };
            let command_id = format!(
                "legacy_{}",
                &canonical::digest(&json!([plan.id, agent_id]))?[..48]
            );
            let retained =
                u64::from(agent.allocation()).max(quota_holds::spend(&tx, agent_id)?.unwrap_or(0));
            tx.execute("INSERT INTO coordinator_agents(agent_id,resource_allocation_id,allocated_tokens,retained_tokens,command_id,decision) VALUES(?1,?2,?3,?4,?5,?6)",params![agent_id,grant_id.as_str(),u64::from(agent.allocation()),retained,command_id,serialize(&stored)?])?;
            adopted.push(json!({"agentAllocationId":agent_id,"request":request,"definition":context,"allocatedTokens":agent.allocation(),"retainedTokens":retained,
                "consumedTokens":quota_holds::spend(&tx,agent_id)?,"decisionSource":"legacy_native_receipts","originalDecisions":decision_references}));
        }
        // Now that the existing children are mapped, reserve each parent only
        // once. A full legacy allocation can move with zero free parent tokens.
        // Unknown/ended allocations retain their full hold until final metering.
        for grant in &plan.resources {
            if held(&tx, grant.id.as_str())?.0 > u64::from(grant.allocated_tokens) {
                return Err(Error::InsufficientCapacity);
            }
            let (resource, period, key) = &resource_rows[grant.id.as_str()];
            let budget = budget(&tx, resource, None, false)?;
            let ceiling = usage::ceiling_report(&tx, &resource.id(), now)?;
            if budget
                .pool
                .ceiling
                .is_none_or(|limit| u64::from(budget.pool.committed) > u64::from(limit))
                || budget.seat.status == allocation::SeatStatus::PeriodMismatch
                || budget
                    .seat
                    .quota
                    .is_some_and(|limit| u64::from(budget.seat.committed) > u64::from(limit))
                || ceiling
                    .ceiling_tokens
                    .is_none_or(|limit| ceiling.drawn > limit)
            {
                return Err(Error::InsufficientCapacity);
            }
            publish(
                &tx,
                fleet,
                &format!("resource_{}", grant.id.as_str()),
                json!({"kind":"resource","resource":{"id":grant.id,"serverEngagementId":fleet,"revision":grant.revision,"allocatedTokens":grant.allocated_tokens,"eligibleManagers":grant.eligible_managers},"resourceId":grant.resource_id,"period":period,"periodKey":key}),
            )?;
        }
        let resources = plan
            .resources
            .iter()
            .map(|grant| {
                let (_, period, key) = &resource_rows[grant.id.as_str()];
                let mut row = serde_json::to_value(grant)?;
                row["period"] = json!(period);
                row["periodKey"] = json!(key);
                Ok(row)
            })
            .collect::<Result<Vec<Value>, Error>>()?;
        let receipt = json!({"version":1,"id":plan.id,"sourceDigest":plan.source_digest,"serverEngagementId":fleet,
            "registrationGeneration":plan.registration_generation,"delegationRevision":plan.delegation_revision,
            "resourceOwner":plan.resource_owner,"resources":resources,"projects":plan.projects,"agents":adopted,"acceptedAtMs":now});
        bounded_row(&tx, "coordinator_migrations", "id", plan.id.as_str(), 1000)?;
        tx.execute(
            "INSERT INTO coordinator_migrations VALUES(?1,?2,?3,?4,?5)",
            params![
                plan.id.as_str(),
                fleet,
                fingerprint,
                serialize(&receipt)?,
                now
            ],
        )?;
        tx.commit()?;
        Ok(receipt)
    }
}
