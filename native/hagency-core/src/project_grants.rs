//! Bounded Palpo delegation. Catalog publication is not delegation authority.
//!
//! A Hagency operator delegates a finite resource budget to one registration.
//! Palpo reserves projects within it and explicitly assigns their approvers.
//! These closed wire types validate scope; transport authentication and durable
//! capacity accounting remain responsibilities of their owning boundaries.
use crate::{InvalidInput, JSON_SAFE_MAX, authority::Registration};
use ruma_common::{RoomId, UserId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

fn id(value: &str) -> Result<(), InvalidInput> {
    if value.is_empty() || value.len() > 255 || value.chars().any(char::is_control) {
        return Err(InvalidInput("invalid grant identity"));
    }
    Ok(())
}
fn positive(value: u64) -> Result<(), InvalidInput> {
    if value == 0 || value > JSON_SAFE_MAX {
        return Err(InvalidInput(
            "grant limits must be positive JSON-safe integers",
        ));
    }
    Ok(())
}
fn user(value: &str, server: &str) -> Result<(), InvalidInput> {
    if !UserId::parse(value).is_ok_and(|u| u.server_name().as_str() == server) {
        return Err(InvalidInput(
            "grant actor must belong to the authorized Palpo server",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GrantLimits {
    pub tokens: u64,
    pub max_agents: u64,
    pub max_rate_per_day: u64,
}
impl GrantLimits {
    pub fn validate(&self) -> Result<(), InvalidInput> {
        positive(self.tokens)?;
        positive(self.max_agents)?;
        positive(self.max_rate_per_day)?;
        if self.max_agents > 10_000 {
            return Err(InvalidInput("grant agent limit exceeds the service bound"));
        }
        Ok(())
    }
    pub fn contains(&self, child: &Self) -> bool {
        child.tokens <= self.tokens
            && child.max_agents <= self.max_agents
            && child.max_rate_per_day <= self.max_rate_per_day
    }
}

/// Created only by Hagency's authenticated local operator. No absent limit
/// means unlimited. Rotation, expiry and revocation require fresh authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResourceDelegation {
    pub v: u8,
    pub id: String,
    pub revision: u64,
    pub fleet_id: String,
    pub registration_generation: u64,
    pub issuer: String,
    pub resource_id: String,
    pub limits: GrantLimits,
    pub expires_at_ms: u64,
}
impl ResourceDelegation {
    pub fn validate(&self, registration: &Registration, now: u64) -> Result<(), InvalidInput> {
        registration.validate()?;
        crate::project::identifier(&self.id, 128)?;
        id(&self.resource_id)?;
        positive(self.revision)?;
        self.limits.validate()?;
        if self.v != 1
            || self.fleet_id != registration.fleet_id
            || self.registration_generation != registration.generation
            || self.issuer != registration.server_name
            || self.expires_at_ms <= now
            || self.expires_at_ms > JSON_SAFE_MAX
        {
            return Err(InvalidInput(
                "delegation scope, generation or expiry is invalid",
            ));
        }
        Ok(())
    }
}

/// One project and resource, under an explicit contribution. Revision is an
/// optimistic-concurrency fence, not permission inferred from a room role.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProjectGrant {
    pub v: u8,
    pub id: String,
    pub revision: u64,
    pub delegation_id: String,
    pub delegation_revision: u64,
    pub project_id: String,
    pub room_id: String,
    pub owner_mxid: String,
    pub administrator_mxids: Vec<String>,
    pub allow_self_approval: bool,
    pub limits: GrantLimits,
    pub expires_at_ms: u64,
}
impl ProjectGrant {
    pub fn validate(&self, parent: &ResourceDelegation, now: u64) -> Result<(), InvalidInput> {
        id(&self.id)?;
        id(&self.project_id)?;
        positive(self.revision)?;
        self.limits.validate()?;
        user(&self.owner_mxid, &parent.issuer)?;
        if self.v != 1
            || self.delegation_id != parent.id
            || self.delegation_revision != parent.revision
            || self.expires_at_ms <= now
            || self.expires_at_ms > parent.expires_at_ms
            || !parent.limits.contains(&self.limits)
            || self.administrator_mxids.is_empty()
            || self.administrator_mxids.len() > 16
            || self
                .administrator_mxids
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != self.administrator_mxids.len()
            || RoomId::parse(&self.room_id).is_err()
            || !self
                .room_id
                .split_once(':')
                .is_some_and(|(_, s)| s == parent.issuer)
        {
            return Err(InvalidInput("project grant scope or limits are invalid"));
        }
        for actor in &self.administrator_mxids {
            user(actor, &parent.issuer)?;
        }
        Ok(())
    }
    pub fn permits_decision(&self, actor: &str, requester: &str) -> bool {
        self.administrator_mxids.iter().any(|a| a == actor)
            && (self.allow_self_approval || actor != requester)
    }
}
