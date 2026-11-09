use crate::{MAX_JSON_BYTES, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use thiserror::Error;

pub const MAX_EVENT_QUEUE: usize = 64;
pub const EVENT_KINDS: [&str; 10] = [
    "agent.provisioned",
    "agent.retired",
    "dispatch.started",
    "dispatch.blocked",
    "dispatch.completed",
    "dispatch.failed",
    "approval.requested",
    "approval.resolved",
    "plugin.enabled",
    "plugin.disabled",
];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RedactedEvent {
    pub kind: String,
    pub opaque_id: String,
    pub state: String,
    pub at_ms: u64,
}
impl RedactedEvent {
    pub fn new(kind: &str, opaque_id: &str, state: &str, at_ms: u64) -> Result<Self, PluginError> {
        if !EVENT_KINDS.contains(&kind)
            || opaque_id.is_empty()
            || opaque_id.len() > 128
            || state.len() > 64
        {
            return Err(PluginError::Incompatible);
        }
        Ok(Self {
            kind: kind.into(),
            opaque_id: opaque_id.into(),
            state: state.into(),
            at_ms,
        })
    }
}

#[derive(Debug, Default)]
pub struct EventQueue {
    events: VecDeque<RedactedEvent>,
    events_lost: u64,
}
impl EventQueue {
    pub fn push(&mut self, event: RedactedEvent) {
        if self.events.len() >= MAX_EVENT_QUEUE {
            self.events.pop_front();
            self.events_lost = self.events_lost.saturating_add(1);
        }
        self.events.push_back(event);
    }
    pub fn drain(&mut self) -> (Vec<RedactedEvent>, u64) {
        (
            self.events.drain(..).collect(),
            std::mem::take(&mut self.events_lost),
        )
    }
    pub fn len(&self) -> usize {
        self.events.len()
    }
    pub fn events_lost(&self) -> u64 {
        self.events_lost
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    ApproveOnce,
    Deny,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginToolApproval {
    pub framework: String,
    pub plugin_id: String,
    pub tool_id: String,
    pub package_sha256: String,
    pub input_sha256: String,
}
impl PluginToolApproval {
    pub fn new(
        framework: &str,
        plugin_id: &str,
        tool_id: &str,
        package_sha256: &str,
        input: &Value,
    ) -> Result<Self, PluginError> {
        if serde_json::to_vec(input)?.len() > MAX_JSON_BYTES {
            return Err(PluginError::Capacity);
        }
        Ok(Self {
            framework: framework.into(),
            plugin_id: plugin_id.into(),
            tool_id: tool_id.into(),
            package_sha256: package_sha256.into(),
            input_sha256: sha256_json(input)?,
        })
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ToolResultError {
    #[error("plugin tool result is malformed")]
    Malformed,
    #[error("plugin tool result exceeds its bound")]
    Capacity,
}
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginToolResult {
    pub content: Vec<PluginToolContent>,
    pub is_error: bool,
}
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PluginToolContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}
pub fn parse_tool_result(bytes: &[u8]) -> Result<PluginToolResult, ToolResultError> {
    if bytes.len() > MAX_JSON_BYTES {
        return Err(ToolResultError::Capacity);
    }
    let result: PluginToolResult =
        serde_json::from_slice(bytes).map_err(|_| ToolResultError::Malformed)?;
    if result
        .content
        .iter()
        .any(|part| part.content_type != "text" || part.text.len() > MAX_JSON_BYTES)
    {
        return Err(ToolResultError::Malformed);
    }
    Ok(result)
}


pub fn sha256_json(value: &Value) -> Result<String, PluginError> {
    let bytes = serde_json::to_vec(value)?;
    Ok(crate::digest_bytes(&bytes))
}
