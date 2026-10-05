use super::*;

fn describe(fleet: &str, payload: &Value) -> Result<(String, String, Value), Error> {
    let (context, target, canonical) = match payload["operation"].as_str() {
        Some("coordinator_project_approval") => {
            let command: ProjectApproval = serde_json::from_value(payload["command"].clone())?;
            if command.request.server_engagement_id.as_str() != fleet {
                return Err(Error::Conflict);
            }
            let expected: String = command.request.definition_digest.clone().into();
            if canonical::digest(&payload["definition"])? != expected {
                return Err(Error::Conflict);
            }
            let canonical = json!({"operation":"coordinator_project_approval","command":command,"definition":payload["definition"]});
            (
                command.context,
                json!({"projectId":command.request.project_id}),
                canonical,
            )
        }
        Some("coordinator_token_top_up") => {
            let command: TokenTopUpApproval = serde_json::from_value(payload["command"].clone())?;
            if command.request.server_engagement_id.as_str() != fleet {
                return Err(Error::Conflict);
            }
            let canonical = json!({"operation":"coordinator_token_top_up","command":command});
            (
                command.context,
                json!({"agentId":command.request.agent_allocation_id}),
                canonical,
            )
        }
        Some("coordinator_agent_control") => {
            let command: AgentControl = serde_json::from_value(payload["command"].clone())?;
            let canonical = json!({"operation":"coordinator_agent_control","command":command});
            (
                command.context,
                json!({"agentId":command.agent_allocation_id,"operation":command.operation}),
                canonical,
            )
        }
        _ => {
            return Err(Error::Invalid(InvalidInput(
                "invalid coordinator operation",
            )));
        }
    };
    if context.server_engagement_id.as_str() != fleet {
        return Err(Error::Conflict);
    }
    Ok((
        context.command_id.as_str().into(),
        canonical::digest(&canonical)?,
        target,
    ))
}

/// Terminal receipts precede freshness checks: replay observes a past outcome;
/// it cannot consume capacity again or revive a refused command.
pub(super) fn result(db: &Connection, id: &str, digest: &str) -> Result<Option<Value>, Error> {
    let refused: Option<(String, String)> = db
        .query_row(
            "SELECT digest,receipt FROM coordinator_refusals WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((old, _)) = refused {
        return Err(if old == digest {
            Error::State
        } else {
            Error::Conflict
        });
    }
    let applied: Option<(String, String)> = db
        .query_row(
            "SELECT digest,receipt FROM coordinator_commands WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    applied
        .map(|(old, receipt)| {
            if old == digest {
                Ok(serde_json::from_str(&receipt)?)
            } else {
                Err(Error::Conflict)
            }
        })
        .transpose()
}
impl DomainRepository {
    pub fn receive_coordinator_command(
        &mut self,
        fleet: &str,
        payload: &Value,
        now: u64,
    ) -> Result<Option<Value>, Error> {
        if let Some(outcome) = self.coordinator_command_outcome(fleet, payload)? {
            return Ok(Some(outcome));
        }
        let checked = (|| match payload["operation"].as_str() {
            Some("coordinator_agent_control") => {
                let command: AgentControl = serde_json::from_value(payload["command"].clone())?;
                lifecycle::authorize(&self.db, &command, now)
            }
            Some("coordinator_project_approval") => {
                let authority = current(&self.db, fleet, now)?;
                let command: ProjectApproval = serde_json::from_value(payload["command"].clone())?;
                contract::authorize_project_approval(
                    &command,
                    &command.request,
                    &authority,
                    &command.context.actor,
                    now,
                )
                .map_err(policy)
            }
            Some("coordinator_token_top_up") => {
                let authority = current(&self.db, fleet, now)?;
                let command: TokenTopUpApproval =
                    serde_json::from_value(payload["command"].clone())?;
                let raw: String = self
                    .db
                    .query_row(
                        "SELECT grant FROM coordinator_projects WHERE engagement_id=?1 AND id=?2",
                        params![fleet, command.request.project_id.as_str()],
                        |r| r.get(0),
                    )
                    .optional()?
                    .ok_or(Error::NotFound)?;
                let grant: ProjectGrant = serde_json::from_str(&raw)?;
                contract::authorize_token_top_up(
                    &command,
                    &command.request,
                    &authority,
                    &grant,
                    &command.context.actor,
                    now,
                )
                .map_err(policy)
            }
            _ => Err(Error::Invalid(InvalidInput(
                "invalid coordinator operation",
            ))),
        })();
        if let Err(error) = checked {
            let Some(reason) = coordinator_refusal_reason(&error) else {
                return Err(error);
            };
            return self
                .refuse_coordinator_command(fleet, payload, reason, now)
                .map(Some);
        }
        Ok(None)
    }
    pub fn coordinator_command_outcome(
        &self,
        fleet: &str,
        payload: &Value,
    ) -> Result<Option<Value>, Error> {
        let (id, digest, mut receipt) = describe(fleet, payload)?;
        let refused: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT digest,receipt FROM coordinator_refusals WHERE id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((old, raw)) = refused {
            if old != digest {
                return Err(Error::Conflict);
            }
            return Ok(Some(serde_json::from_str(&raw)?));
        }
        if let Some(value) = result(&self.db, &id, &digest)? {
            receipt["state"] = json!("applied");
            receipt["commandId"] = json!(id);
            receipt["result"] = value;
            return Ok(Some(receipt));
        }
        Ok(None)
    }
    pub fn refuse_coordinator_command(
        &mut self,
        fleet: &str,
        payload: &Value,
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
        if let Some(outcome) = self.coordinator_command_outcome(fleet, payload)? {
            return Ok(outcome);
        }
        let (id, digest, mut receipt) = describe(fleet, payload)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        binding(&tx, fleet)?.ok_or(Error::NotFound)?;
        bounded_row(&tx, "coordinator_refusals", "id", &id, 10000)?;
        receipt["kind"] = json!("receipt");
        receipt["commandId"] = json!(id);
        receipt["commandDigest"] = json!(digest);
        receipt["state"] = json!("refused");
        receipt["reason"] = json!(reason);
        tx.execute("INSERT INTO coordinator_refusals(id,engagement_id,digest,receipt,refused_at) VALUES(?1,?2,?3,?4,?5)",params![id,fleet,digest,serialize(&receipt)?,now])?;
        publish(&tx, fleet, &format!("command_{id}"), receipt.clone())?;
        tx.commit()?;
        Ok(receipt)
    }
}
