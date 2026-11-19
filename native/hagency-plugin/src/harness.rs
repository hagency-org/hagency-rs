use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const MAX_FRAME_BYTES: usize = super::MAX_FRAME_BYTES;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum HarnessProtocolError {
    #[error("harness frame exceeds 1 MiB")]
    Capacity,
    #[error("harness frame is malformed: {0}")]
    Malformed(String),
    #[error("harness request id must be a string")]
    InvalidId,
    #[error("harness agent identity is invalid")]
    InvalidIdentity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessOwnership {
    Existing,
    Managed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessPrepared {
    pub target: String,
    pub ownership: HarnessOwnership,
}

impl HarnessPrepared {
    pub fn validate(self) -> Result<Self, HarnessProtocolError> {
        if self.target.is_empty()
            || self.target.len() > 256
            || self.target.chars().any(char::is_control)
        {
            return Err(HarnessProtocolError::InvalidIdentity);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "camelCase")]
pub enum HarnessRequest {
    #[serde(rename = "initialize")]
    Initialize {
        protocol: String,
        capabilities: Vec<String>,
    },
    #[serde(rename = "agent/prepare")]
    PrepareAgent {
        allocation_id: String,
        dispatch_id: String,
        agent_name: String,
        workspace: String,
        template: crate::HarnessTemplateDocument,
    },
    #[serde(rename = "agent/release")]
    ReleaseAgent {
        allocation_id: String,
        dispatch_id: String,
        target: String,
        outcome: String,
    },
    #[serde(rename = "snapshot")]
    Snapshot { sequence: u64, state: Value },
    #[serde(rename = "event")]
    Event { sequence: u64, event: Value },
    #[serde(rename = "stop_attempt")]
    StopAttempt { attempt_id: String },
    #[serde(rename = "open_console")]
    OpenConsole { pane_id: String },
    #[serde(rename = "shutdown")]
    Shutdown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HarnessFrame {
    Response {
        id: String,
        result: Value,
    },
    Error {
        id: String,
        error: Value,
    },
    /// Boxed: a request is far larger than the other frames.
    Request {
        id: String,
        request: Box<HarnessRequest>,
    },
    Notification(HarnessNotification),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HarnessNotification {
    Event { sequence: u64, event: Value },
}

pub fn encode_request(id: &str, request: &HarnessRequest) -> Result<Vec<u8>, HarnessProtocolError> {
    if id.is_empty() || id.len() > 128 {
        return Err(HarnessProtocolError::InvalidId);
    }
    let value = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": request_method(request),
        "params": request_params(request),
    });
    let mut bytes = serde_json::to_vec(&value)
        .map_err(|error| HarnessProtocolError::Malformed(error.to_string()))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(HarnessProtocolError::Capacity);
    }
    Ok(bytes)
}
pub fn encode_response(id: &str, result: &Value) -> Result<Vec<u8>, HarnessProtocolError> {
    if id.is_empty() || id.len() > 128 {
        return Err(HarnessProtocolError::InvalidId);
    }
    let mut bytes = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
    .map_err(|error| HarnessProtocolError::Malformed(error.to_string()))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(HarnessProtocolError::Capacity);
    }
    Ok(bytes)
}

pub fn decode_frame(line: &[u8]) -> Result<HarnessFrame, HarnessProtocolError> {
    if line.len() > MAX_FRAME_BYTES {
        return Err(HarnessProtocolError::Capacity);
    }
    let value: Value = serde_json::from_slice(line)
        .map_err(|error| HarnessProtocolError::Malformed(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| HarnessProtocolError::Malformed("frame must be an object".into()))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(HarnessProtocolError::Malformed(
            "jsonrpc must be 2.0".into(),
        ));
    }
    if let Some(id) = object.get("id") {
        let id = id
            .as_str()
            .ok_or(HarnessProtocolError::InvalidId)?
            .to_owned();
        if id.is_empty() || id.len() > 128 {
            return Err(HarnessProtocolError::InvalidId);
        }
        if let Some(method) = object.get("method").and_then(Value::as_str) {
            let params = object.get("params").cloned().unwrap_or(Value::Null);
            let request = match method {
                "stop_attempt" => {
                    #[derive(Deserialize)]
                    #[serde(rename_all = "camelCase")]
                    struct Params {
                        attempt_id: String,
                    }
                    let value: Params = serde_json::from_value(params)
                        .map_err(|error| HarnessProtocolError::Malformed(error.to_string()))?;
                    HarnessRequest::StopAttempt {
                        attempt_id: value.attempt_id,
                    }
                }
                "open_console" => {
                    #[derive(Deserialize)]
                    #[serde(rename_all = "camelCase")]
                    struct Params {
                        pane_id: String,
                    }
                    let value: Params = serde_json::from_value(params)
                        .map_err(|error| HarnessProtocolError::Malformed(error.to_string()))?;
                    HarnessRequest::OpenConsole {
                        pane_id: value.pane_id,
                    }
                }
                _ => {
                    return Err(HarnessProtocolError::Malformed(format!(
                        "unsupported request: {method}"
                    )));
                }
            };
            return Ok(HarnessFrame::Request {
                id,
                request: Box::new(request),
            });
        }
        if let Some(result) = object.get("result") {
            return Ok(HarnessFrame::Response {
                id,
                result: result.clone(),
            });
        }
        if let Some(error) = object.get("error") {
            return Ok(HarnessFrame::Error {
                id,
                error: error.clone(),
            });
        }
        return Err(HarnessProtocolError::Malformed(
            "id frame must contain result or error".into(),
        ));
    }
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| HarnessProtocolError::Malformed("notification method is missing".into()))?;
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    let notification = match method {
        "event" => {
            #[derive(Deserialize)]
            struct Params {
                sequence: u64,
                event: Value,
            }
            let value: Params = serde_json::from_value(params)
                .map_err(|error| HarnessProtocolError::Malformed(error.to_string()))?;
            HarnessNotification::Event {
                sequence: value.sequence,
                event: value.event,
            }
        }
        _ => {
            return Err(HarnessProtocolError::Malformed(format!(
                "unsupported notification: {method}"
            )));
        }
    };
    Ok(HarnessFrame::Notification(notification))
}

fn request_method(request: &HarnessRequest) -> &'static str {
    match request {
        HarnessRequest::Initialize { .. } => "initialize",
        HarnessRequest::PrepareAgent { .. } => "agent/prepare",
        HarnessRequest::ReleaseAgent { .. } => "agent/release",
        HarnessRequest::Snapshot { .. } => "snapshot",
        HarnessRequest::Event { .. } => "event",
        HarnessRequest::StopAttempt { .. } => "stop_attempt",
        HarnessRequest::OpenConsole { .. } => "open_console",
        HarnessRequest::Shutdown => "shutdown",
    }
}

fn request_params(request: &HarnessRequest) -> Value {
    match request {
        HarnessRequest::Initialize {
            protocol,
            capabilities,
        } => serde_json::json!({
            "protocol": protocol,
            "capabilities": capabilities,
        }),
        HarnessRequest::PrepareAgent {
            allocation_id,
            dispatch_id,
            agent_name,
            workspace,
            template,
        } => serde_json::json!({
            "allocationId": allocation_id,
            "dispatchId": dispatch_id,
            "agentName": agent_name,
            "workspace": workspace,
            "template": template,
        }),
        HarnessRequest::ReleaseAgent {
            allocation_id,
            dispatch_id,
            target,
            outcome,
        } => serde_json::json!({
            "allocationId": allocation_id,
            "dispatchId": dispatch_id,
            "target": target,
            "outcome": outcome,
        }),
        HarnessRequest::Snapshot { sequence, state } => serde_json::json!({
            "sequence": sequence,
            "state": state,
        }),
        HarnessRequest::Event { sequence, event } => serde_json::json!({
            "sequence": sequence,
            "event": event,
        }),
        HarnessRequest::StopAttempt { attempt_id } => serde_json::json!({
            "attemptId": attempt_id,
        }),
        HarnessRequest::OpenConsole { pane_id } => serde_json::json!({
            "paneId": pane_id,
        }),
        HarnessRequest::Shutdown => Value::Null,
    }
}
