use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectSetupCommand {
    pub context: contract::CommandContext,
    pub project_id: contract::ProjectId,
    pub project_revision: contract::Revision,
    pub approval_command_id: contract::CommandId,
}
#[derive(Debug, Clone, Serialize)]
pub struct ProjectSetupWork {
    pub definition: ProjectDefinition,
    pub completed: Option<Value>,
}
fn authorized(
    db: &Connection,
    command: &ProjectSetupCommand,
    now: u64,
) -> Result<(ProjectGrant, ProjectDefinition), Error> {
    let c = &command.context;
    let authority = current(db, c.server_engagement_id.as_str(), now)?;
    if c.version != 1
        || c.registration_generation != authority.registration_generation
        || c.delegation_revision != authority.delegation_revision
        || c.issued_at_ms > now
        || c.expires_at_ms <= now
        || c.expires_at_ms > contract::MAX_EXACT_JSON_INTEGER
    {
        return Err(Error::Generation);
    }
    let (raw, definition): (String, String) = db
        .query_row(
            "SELECT grant,definition FROM coordinator_projects WHERE engagement_id=?1 AND id=?2",
            params![authority.id.as_str(), command.project_id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    let grant: ProjectGrant = serde_json::from_str(&raw)?;
    if grant.revision != command.project_revision
        || !matches!(
            grant.state,
            contract::ProjectState::Approved | contract::ProjectState::Ready
        )
    {
        return Err(Error::State);
    }
    if c.actor != grant.owner && c.actor != authority.owner && c.actor != authority.coordinator {
        return Err(Error::LocalAuthority);
    }
    let source: String = db
        .query_row(
            "SELECT receipt FROM coordinator_commands WHERE id=?1 AND engagement_id=?2",
            params![command.approval_command_id.as_str(), authority.id.as_str()],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    let mut approved: ProjectGrant = serde_json::from_str(&source)?;
    approved.state = grant.state;
    if approved != grant {
        return Err(Error::Conflict);
    }
    Ok((grant, serde_json::from_str(&definition)?))
}
fn fingerprint(command: &ProjectSetupCommand) -> Result<String, Error> {
    Ok(canonical::digest(
        &json!({"operation":"coordinator_project_setup","command":command}),
    )?)
}
fn publish_status(
    db: &Connection,
    command: &ProjectSetupCommand,
    state: &str,
    reason: Option<&str>,
    now: u64,
) -> Result<Value, Error> {
    let fleet = command.context.server_engagement_id.as_str();
    let project = command.project_id.as_str();
    let previous:Option<String>=db.query_row("SELECT observation FROM coordinator_project_setup WHERE engagement_id=?1 AND project_id=?2",params![fleet,project],|r|r.get(0)).optional()?;
    let revision = previous
        .map(|raw| serde_json::from_str::<Value>(&raw))
        .transpose()?
        .map_or(Ok(1), |p| {
            p["revision"]
                .as_u64()
                .filter(|r| *r < contract::MAX_EXACT_JSON_INTEGER)
                .map(|r| r + 1)
                .ok_or(Error::Capacity)
        })?;
    let authority = binding(db, fleet)?.ok_or(Error::LocalAuthority)?;
    let value = json!({"kind":"project_setup","projectId":project,"projectRevision":command.project_revision,
        "approvalCommandId":command.approval_command_id,"attemptId":command.context.command_id,"revision":revision,"state":state,"reason":reason,"observedAtMs":now,
        "registrationGeneration":authority.registration_generation,"delegationRevision":authority.delegation_revision});
    db.execute("INSERT INTO coordinator_project_setup(engagement_id,project_id,attempt_id,observation) VALUES(?1,?2,?3,?4) ON CONFLICT(engagement_id,project_id) DO UPDATE SET attempt_id=excluded.attempt_id,observation=excluded.observation",params![fleet,project,command.context.command_id.as_str(),serialize(&value)?])?;
    publish(
        db,
        fleet,
        &format!("project_setup_{project}"),
        value.clone(),
    )?;
    Ok(value)
}
impl DomainRepository {
    /// Recovery operates on an existing grant. It cannot create a grant, change
    /// its rooms/resources or substitute the actor who originally approved it.
    pub fn begin_project_setup(
        &mut self,
        command: &ProjectSetupCommand,
        now: u64,
    ) -> Result<ProjectSetupWork, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let digest = fingerprint(command)?;
        let id = command.context.command_id.as_str();
        if id != command.approval_command_id.as_str() {
            refusals::result(&tx, id, &digest)?;
        }
        let prior: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT digest,result FROM coordinator_project_setup_attempts WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if prior.as_ref().is_some_and(|(old, _)| old != &digest) {
            return Err(Error::Conflict);
        }
        // A completed replay performs no Matrix operation, including after expiry.
        if let Some((_, Some(result))) = prior {
            return Ok(ProjectSetupWork {
                definition: ProjectDefinition {
                    name: String::new(),
                    reason: String::new(),
                    room_id: String::new(),
                    owner_dm_room_id: String::new(),
                },
                completed: Some(serde_json::from_str(&result)?),
            });
        }
        let (grant, definition) = authorized(&tx, command, now)?;
        if prior.is_none() {
            let occupied: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM coordinator_commands WHERE id=?1)",
                [id],
                |r| r.get(0),
            )?;
            if occupied && id != command.approval_command_id.as_str() {
                return Err(Error::Conflict);
            }
            bounded_row(&tx, "coordinator_project_setup_attempts", "id", id, 10000)?;
            tx.execute("INSERT INTO coordinator_project_setup_attempts(id,engagement_id,project_id,digest,command) VALUES(?1,?2,?3,?4,?5)",params![id,grant.server_engagement_id.as_str(),grant.project_id.as_str(),digest,serialize(command)?])?;
            publish_status(&tx, command, "pending", None, now)?;
        }
        let active:String=tx.query_row("SELECT attempt_id FROM coordinator_project_setup WHERE engagement_id=?1 AND project_id=?2",params![grant.server_engagement_id.as_str(),grant.project_id.as_str()],|r|r.get(0))?;
        if active != id {
            return Err(Error::Generation);
        }
        let completed = if grant.state == contract::ProjectState::Ready {
            let result = publish_status(&tx, command, "ready", None, now)?;
            tx.execute(
                "UPDATE coordinator_project_setup_attempts SET result=?2 WHERE id=?1",
                params![id, serialize(&result)?],
            )?;
            Some(result)
        } else {
            None
        };
        tx.commit()?;
        Ok(ProjectSetupWork {
            definition,
            completed,
        })
    }
    /// Called before every Matrix operation and before accepting readiness.
    pub fn validate_project_setup(
        &self,
        command: &ProjectSetupCommand,
        now: u64,
    ) -> Result<(), Error> {
        authorized(&self.db, command, now)?;
        let expected = fingerprint(command)?;
        let valid:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM coordinator_project_setup_attempts a JOIN coordinator_project_setup p ON p.attempt_id=a.id WHERE a.id=?1 AND a.digest=?2 AND a.result IS NULL)",params![command.context.command_id.as_str(),expected],|r|r.get(0))?;
        if !valid {
            return Err(Error::Generation);
        }
        Ok(())
    }
    pub fn finish_project_setup(
        &mut self,
        command: &ProjectSetupCommand,
        reason: Option<&str>,
        now: u64,
    ) -> Result<Value, Error> {
        if reason.is_some_and(|r| {
            !matches!(
                r,
                "project_membership_pending"
                    | "private_membership_pending"
                    | "project_room_unreadable"
                    | "private_room_unreadable"
                    | "room_authority_changed"
                    | "authority_changed"
                    | "setup_unavailable"
            )
        }) {
            return Err(InvalidInput("invalid project setup reason").into());
        }
        if reason.is_none() {
            self.validate_project_setup(command, now)?;
            if authorized(&self.db, command, now)?.0.state != contract::ProjectState::Ready {
                return Err(Error::State);
            }
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = command.context.command_id.as_str();
        let (digest, result): (String, Option<String>) = tx
            .query_row(
                "SELECT digest,result FROM coordinator_project_setup_attempts WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        if digest != fingerprint(command)? {
            return Err(Error::Conflict);
        }
        if let Some(result) = result {
            return Ok(serde_json::from_str(&result)?);
        }
        let active:String=tx.query_row("SELECT attempt_id FROM coordinator_project_setup WHERE engagement_id=?1 AND project_id=?2",params![command.context.server_engagement_id.as_str(),command.project_id.as_str()],|r|r.get(0))?;
        if active != id {
            return Err(Error::Generation);
        }
        let result = publish_status(
            &tx,
            command,
            if reason.is_some() { "failed" } else { "ready" },
            reason,
            now,
        )?;
        tx.execute(
            "UPDATE coordinator_project_setup_attempts SET result=?2 WHERE id=?1",
            params![id, serialize(&result)?],
        )?;
        tx.commit()?;
        Ok(result)
    }
}
