//! Closed Palpo business commands. Transport custody is not an execution receipt.
use crate::{
    InvalidInput, JSON_SAFE_MAX,
    authority::{ProjectRequest, Registration},
    canonical,
    project::identifier,
    project_grants::ProjectGrant,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProjectCommand {
    pub v: u8,
    pub command_id: String,
    pub fleet_id: String,
    pub registration_generation: u64,
    pub issuer: String,
    pub actor_mxid: String,
    pub expires_at_ms: u64,
    pub operation: ProjectOperation,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProjectOperation {
    ReserveProject {
        grant: ProjectGrant,
    },
    AssignProjectAdmins {
        grant_id: String,
        expected_revision: u64,
        administrators: Vec<String>,
        allow_self_approval: bool,
    },
    ApproveAgent {
        grant_id: String,
        grant_revision: u64,
        request: ProjectRequest,
        allocated_tokens: u64,
    },
    RejectAgent {
        grant_id: String,
        grant_revision: u64,
        request: ProjectRequest,
    },
    TopUpAgent {
        grant_id: String,
        grant_revision: u64,
        engagement_id: String,
        requester_mxid: String,
        add_tokens: u64,
    },
    RevokeAgent {
        grant_id: String,
        grant_revision: u64,
        engagement_id: String,
    },
    RevokeProject {
        grant_id: String,
        expected_revision: u64,
    },
}
fn positive(n: u64) -> Result<(), InvalidInput> {
    if n == 0 || n > JSON_SAFE_MAX {
        return Err(InvalidInput("invalid workflow integer"));
    }
    Ok(())
}
fn user(id: &str, issuer: &str) -> Result<(), InvalidInput> {
    if !ruma_common::UserId::parse(id).is_ok_and(|u| u.server_name().as_str() == issuer) {
        return Err(InvalidInput("workflow actor is outside its issuer"));
    }
    Ok(())
}
impl ProjectCommand {
    /// Reject extensions also inside legacy request/agent DTOs, whose older
    /// decoders intentionally accept extra fields outside this closed protocol.
    pub fn decode(value: serde_json::Value) -> Result<Self, InvalidInput> {
        let command: Self = serde_json::from_value(value.clone())
            .map_err(|_| InvalidInput("invalid workflow command"))?;
        if serde_json::to_value(&command).map_err(|_| InvalidInput("invalid workflow command"))?
            != value
        {
            return Err(InvalidInput(
                "workflow command contains unknown or noncanonical fields",
            ));
        }
        Ok(command)
    }
    /// Shape and authenticated registration, deliberately independent of time:
    /// an already committed receipt remains readable after its command expires.
    pub fn validate(&self, registration: &Registration) -> Result<(), InvalidInput> {
        registration.validate()?;
        identifier(&self.command_id, 128)?;
        positive(self.expires_at_ms)?;
        if self.v != 1
            || self.fleet_id != registration.fleet_id
            || self.registration_generation != registration.generation
            || self.issuer != registration.server_name
        {
            return Err(InvalidInput("workflow registration mismatch"));
        }
        user(&self.actor_mxid, &self.issuer)?;
        use ProjectOperation::*;
        match &self.operation {
            ReserveProject { grant } => {
                identifier(&grant.id, 128)?;
                positive(grant.revision)?;
                grant.limits.validate()?;
            }
            AssignProjectAdmins {
                grant_id,
                expected_revision,
                administrators,
                ..
            } => {
                identifier(grant_id, 128)?;
                positive(*expected_revision)?;
                if administrators.is_empty() || administrators.len() > 16 {
                    return Err(InvalidInput("invalid project administrators"));
                }
                for actor in administrators {
                    user(actor, &self.issuer)?;
                }
            }
            ApproveAgent {
                grant_id,
                grant_revision,
                request,
                allocated_tokens,
            } => {
                identifier(grant_id, 128)?;
                positive(*grant_revision)?;
                positive(*allocated_tokens)?;
                request.validate(registration)?;
            }
            RejectAgent {
                grant_id,
                grant_revision,
                request,
            } => {
                identifier(grant_id, 128)?;
                positive(*grant_revision)?;
                request.validate(registration)?;
            }
            TopUpAgent {
                grant_id,
                grant_revision,
                engagement_id,
                requester_mxid,
                add_tokens,
            } => {
                identifier(grant_id, 128)?;
                positive(*grant_revision)?;
                identifier(engagement_id, 128)?;
                user(requester_mxid, &self.issuer)?;
                positive(*add_tokens)?;
            }
            RevokeAgent {
                grant_id,
                grant_revision,
                engagement_id,
            } => {
                identifier(grant_id, 128)?;
                positive(*grant_revision)?;
                identifier(engagement_id, 128)?;
            }
            RevokeProject {
                grant_id,
                expected_revision,
            } => {
                identifier(grant_id, 128)?;
                positive(*expected_revision)?;
            }
        }
        if serde_json::to_vec(self)
            .map_err(|_| InvalidInput("invalid workflow command"))?
            .len()
            > 48 * 1024
        {
            return Err(InvalidInput("workflow command exceeds limit"));
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String, InvalidInput> {
        canonical::digest(
            &serde_json::to_value(self).map_err(|_| InvalidInput("invalid workflow command"))?,
        )
    }
    pub fn request(&self) -> Option<&ProjectRequest> {
        match &self.operation {
            ProjectOperation::ApproveAgent { request, .. }
            | ProjectOperation::RejectAgent { request, .. } => Some(request),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRefusal {
    Expired,
    GrantExpired,
    GrantRevoked,
    Authority,
    Conflict,
    NotFound,
    InsufficientCapacity,
    ResourceUnavailable,
    Invalid,
    State,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProjectResult {
    Grant {
        grant: ProjectGrant,
    },
    Agent {
        engagement_id: String,
        state: String,
        allocated_tokens: u64,
        cleanup: String,
    },
    RevokedProject {
        grant_id: String,
        revision: u64,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectOutcome {
    Applied { result: ProjectResult },
    Refused { code: ProjectRefusal },
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProjectReceipt {
    pub v: u8,
    pub fleet_id: String,
    pub registration_generation: u64,
    pub command_id: String,
    pub command_digest: String,
    pub completed_at_ms: u64,
    pub outcome: ProjectOutcome,
}

/// Fresh answer from the authenticated Palpo machine endpoint. The transport
/// validates its binding; the writer checks its deadline again after contention.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProjectAuthorization {
    pub v: u8,
    pub command_id: String,
    pub command_digest: String,
    pub allowed: bool,
    pub valid_until_ms: u64,
}
impl ProjectAuthorization {
    pub fn validate(&self, command: &ProjectCommand, now: u64) -> Result<(), InvalidInput> {
        if self.v != 1
            || self.command_id != command.command_id
            || self.command_digest != command.digest()?
            || self.valid_until_ms <= now
            || self.valid_until_ms > now.saturating_add(30_000)
        {
            return Err(InvalidInput(
                "workflow authorization is stale or mismatched",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_commands_match_palpo_wire_corpus() {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../../fixtures/project-commands.json")).unwrap();
        let registration: Registration =
            serde_json::from_value(corpus["registration"].clone()).unwrap();
        for vector in corpus["vectors"].as_array().unwrap() {
            let command = ProjectCommand::decode(vector["command"].clone()).unwrap();
            command.validate(&registration).unwrap();
            assert_eq!(serde_json::to_value(&command).unwrap(), vector["command"]);
            assert_eq!(
                canonical::encode(&serde_json::to_value(&command).unwrap()).unwrap(),
                vector["canonical"]
            );
            assert_eq!(command.digest().unwrap(), vector["sha256"]);
            let receipt: ProjectReceipt =
                serde_json::from_value(vector["receipt"].clone()).unwrap();
            assert_eq!(serde_json::to_value(receipt).unwrap(), vector["receipt"]);
        }
        let mut unknown = corpus["vectors"][2]["command"].clone();
        unknown["operation"]["request"]["administratorOverride"] = serde_json::json!(true);
        assert!(ProjectCommand::decode(unknown).is_err());
        let mut unknown = corpus["vectors"][2]["command"].clone();
        unknown["operation"]["request"]["agentDefinition"]["administratorOverride"] =
            serde_json::json!(true);
        assert!(ProjectCommand::decode(unknown).is_err());
    }
}
