use super::*;

/// The transport worker has authenticated this engagement, but Matrix evidence
/// and resource reservation may still be unavailable. This is not admission.
impl DomainRepository {
    pub fn receive_coordinator_agent(
        &mut self,
        fleet: &str,
        payload: &Value,
        now: u64,
    ) -> Result<Value, Error> {
        let command: AgentApproval =
            serde_json::from_value(payload["coordinatorApproval"].clone())?;
        let definition: hagency_core::authority::ProjectRequest =
            serde_json::from_value(payload.clone())?;
        if command.context.server_engagement_id.as_str() != fleet
            || command.request.server_engagement_id.as_str() != fleet
            || definition.fleet_id != fleet
            || command.request.id.as_str() != definition.request_id
            || command.request.project_id.as_str() != definition.target_project_id
            || command.request.project_owner.as_str() != definition.owner_mxid
            || command.request.requester.as_str() != definition.requester_mxid
            || String::from(command.request.definition_digest.clone())
                != canonical::digest(&serde_json::to_value(&definition)?)?
            || u64::from(command.request.requested_tokens) != u64::from(definition.requested_tokens)
        {
            return Err(Error::Conflict);
        }
        let id = command.context.command_id.as_str();
        let fingerprint = command_digest(&command)?;
        let tx = self.db.transaction()?;
        binding(&tx, fleet)?.ok_or(Error::NotFound)?;
        let previous: Option<(String, String)> = tx
            .query_row(
                "SELECT digest,state FROM coordinator_deliveries WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((digest, state)) = previous {
            if digest != fingerprint {
                return Err(Error::Conflict);
            }
            if state != "pending" {
                tx.commit()?;
                return self.coordinator_delivery(id);
            }
        } else {
            bounded_row(&tx, "coordinator_deliveries", "id", id, 10000)?;
            tx.execute("INSERT INTO coordinator_deliveries(id,engagement_id,digest,command,definition,state,received_at,updated_at) VALUES(?1,?2,?3,?4,?5,'pending',?6,?6)",
                params![id,fleet,fingerprint,serialize(&command)?,serialize(&definition)?,now])?;
        }
        // An approval may have committed before the worker lost its result.
        // Its durable receipt wins over a now-expired command on replay.
        if let Some(applied) = replay(&tx, id, &fingerprint)? {
            tx.execute("UPDATE coordinator_deliveries SET state='applied',agent_id=?2,updated_at=?3 WHERE id=?1",params![id,applied.id,now])?;
        } else {
            let checked = (|| {
                let authority = current(&tx, fleet, now)?;
                let raw: String = tx
                    .query_row(
                        "SELECT grant FROM coordinator_projects WHERE engagement_id=?1 AND id=?2",
                        params![fleet, command.request.project_id.as_str()],
                        |r| r.get(0),
                    )
                    .optional()?
                    .ok_or(Error::NotFound)?;
                let project: ProjectGrant = serde_json::from_str(&raw)?;
                contract::authorize_agent_approval(
                    &command,
                    &command.request,
                    &authority,
                    &project,
                    &command.context.actor,
                    now,
                )
                .map_err(policy)?;
                Ok::<(), Error>(())
            })();
            if let Err(error) = checked {
                let Some(reason) = terminal_reason(&error) else {
                    return Err(error);
                };
                refuse(&tx, id, reason, now)?;
            }
        }
        tx.commit()?;
        self.coordinator_delivery(id)
    }

    pub fn refuse_coordinator_agent(
        &mut self,
        id: &str,
        reason: &str,
        now: u64,
    ) -> Result<Value, Error> {
        if ![
            "insufficient_capacity",
            "authority_changed",
            "invalid_request",
            "project_unavailable",
            "resource_unavailable",
            "request_conflict",
        ]
        .contains(&reason)
        {
            return Err(Error::Invalid(InvalidInput("invalid refusal")));
        }
        let tx = self.db.transaction()?;
        refuse(&tx, id, reason, now)?;
        tx.commit()?;
        self.coordinator_delivery(id)
    }
    fn coordinator_delivery(&self, id: &str) -> Result<Value, Error> {
        self.db.query_row("SELECT command,definition,state,reason,agent_id,received_at,updated_at FROM coordinator_deliveries WHERE id=?1",[id],row)
            .map_err(Into::into)
    }
    pub fn coordinator_deliveries(
        &self,
        fleet: &str,
        after: &str,
        limit: usize,
    ) -> Result<Vec<Value>, Error> {
        if limit == 0 || limit > 50 {
            return Err(Error::Capacity);
        }
        let mut q=self.db.prepare("SELECT command,definition,state,reason,agent_id,received_at,updated_at FROM coordinator_deliveries WHERE engagement_id=?1 AND id>?2 ORDER BY id LIMIT ?3")?;
        q.query_map(params![fleet, after, limit], row)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let command: Value =
        serde_json::from_str(&r.get::<_, String>(0)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let definition: Value =
        serde_json::from_str(&r.get::<_, String>(1)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(
        json!({"id":command["context"]["commandId"],"serverEngagementId":command["context"]["serverEngagementId"],"requestId":command["request"]["id"],
        "agentAllocationId":r.get::<_,Option<String>>(4)?,"agentName":definition["agentDefinition"]["name"],"projectId":command["request"]["projectId"],
        "projectOwner":command["request"]["projectOwner"],"decidedBy":command["context"]["actor"],"approvedAtMs":command["context"]["issuedAtMs"],
        "resourceAllocationId":command["request"]["resourceAllocationId"],"resourceId":definition["agentDefinition"]["resourceId"],
        "requestedTokens":command["request"]["requestedTokens"],"approvedTokens":command["allocatedTokens"],"state":r.get::<_,String>(2)?,
        "reason":r.get::<_,Option<String>>(3)?,"receivedAtMs":r.get::<_,u64>(5)?,"updatedAtMs":r.get::<_,u64>(6)?}),
    )
}
fn refuse(db: &Connection, id: &str, reason: &str, now: u64) -> Result<(), Error> {
    let (fleet, digest, state): (String, String, String) = db.query_row(
        "SELECT engagement_id,digest,state FROM coordinator_deliveries WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if state == "applied" || state == "refused" {
        return Ok(());
    }
    db.execute(
        "UPDATE coordinator_deliveries SET state='refused',reason=?2,updated_at=?3 WHERE id=?1",
        params![id, reason, now],
    )?;
    publish(
        db,
        &fleet,
        &format!("command_{id}"),
        json!({"kind":"receipt","commandId":id,"commandDigest":digest,"state":"refused","reason":reason}),
    )
}

pub fn terminal_reason(error: &Error) -> Option<&'static str> {
    match error {
        Error::InsufficientCapacity | Error::OverCommit { .. } | Error::NoCeiling => {
            Some("insufficient_capacity")
        }
        Error::LocalAuthority | Error::Generation => Some("authority_changed"),
        Error::Conflict => Some("request_conflict"),
        Error::NotFound | Error::State => Some("project_unavailable"),
        Error::Unqualified => Some("resource_unavailable"),
        Error::Invalid(_) => Some("invalid_request"),
        _ => None,
    }
}
