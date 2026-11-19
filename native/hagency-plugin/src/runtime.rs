use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_PENDING_REQUESTS: usize = 64;
/// The capability a runtime advertises in its `initialize` result when it can
/// call host task tools with `tool/call`.
pub const TOOL_CAPABILITY: &str = "tool";

/// One host task tool offered to a runtime in `turn/start`. The same catalog
/// entry (name, description, schema) that the built-in runners read from the
/// task helper; the host executes every call.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub read_only: bool,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RuntimeProtocolError {
    #[error("runtime frame exceeds 1 MiB")]
    Capacity,
    #[error("runtime frame is malformed: {0}")]
    Malformed(String),
    #[error("runtime method is unsupported: {0}")]
    UnsupportedMethod(String),
    #[error("runtime request id must be a string")]
    InvalidId,
    #[error("runtime terminal result is duplicated")]
    DuplicateTerminal,
    #[error("runtime identifiers do not match")]
    MismatchedIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeOutcome {
    Completed,
    Failed,
    Interrupted,
    RateLimited,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeUsage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub reasoning: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "method", content = "params")]
pub enum HostRequest {
    #[serde(rename = "initialize")]
    Initialize {
        protocol: String,
        capabilities: Vec<String>,
    },
    #[serde(rename = "turn/start")]
    TurnStart {
        dispatch_id: String,
        turn_id: String,
        prompt: String,
        workspace: String,
        harness_context: Option<Value>,
        /// Present only when the runtime advertised [`TOOL_CAPABILITY`] and
        /// the host serves task tools for this dispatch.
        tools: Option<Vec<ToolDescriptor>>,
    },
    #[serde(rename = "approval/respond")]
    ApprovalRespond {
        dispatch_id: String,
        turn_id: String,
        approval_id: String,
        decision: String,
    },
    #[serde(rename = "tool/result")]
    ToolResult {
        dispatch_id: String,
        turn_id: String,
        call_id: String,
        result: Value,
    },
    #[serde(rename = "turn/cancel")]
    TurnCancel {
        dispatch_id: String,
        turn_id: String,
    },
    #[serde(rename = "shutdown")]
    Shutdown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PluginFrame {
    Response { id: String, result: Value },
    Error { id: String, error: Value },
    Notification(PluginNotification),
}
#[derive(Debug, Clone, PartialEq)]
pub enum PluginNotification {
    Progress {
        dispatch_id: String,
        turn_id: String,
        text: String,
    },
    ApprovalRequest {
        dispatch_id: String,
        turn_id: String,
        approval_id: String,
        tool: String,
        input: Value,
    },
    ToolCall {
        dispatch_id: String,
        turn_id: String,
        call_id: String,
        tool: String,
        input: Value,
    },
    Usage {
        dispatch_id: String,
        turn_id: String,
        usage: Option<RuntimeUsage>,
    },
    TurnResult {
        dispatch_id: String,
        turn_id: String,
        outcome: RuntimeOutcome,
        final_text: Option<String>,
        error: Option<String>,
        usage: Option<RuntimeUsage>,
    },
}

#[derive(Debug, Clone)]
pub struct RuntimeSession {
    dispatch_id: String,
    turn_id: String,
    terminal_seen: bool,
}
impl RuntimeSession {
    pub fn new(dispatch_id: &str, turn_id: &str) -> Result<Self, RuntimeProtocolError> {
        if dispatch_id.is_empty() || turn_id.is_empty() {
            return Err(RuntimeProtocolError::MismatchedIdentity);
        }
        Ok(Self {
            dispatch_id: dispatch_id.into(),
            turn_id: turn_id.into(),
            terminal_seen: false,
        })
    }
    pub fn accept(
        &mut self,
        frame: PluginFrame,
    ) -> Result<Option<PluginNotification>, RuntimeProtocolError> {
        let PluginFrame::Notification(notification) = frame else {
            return Ok(None);
        };
        let (dispatch_id, turn_id) = notification_identity(&notification);
        if dispatch_id != self.dispatch_id || turn_id != self.turn_id {
            return Err(RuntimeProtocolError::MismatchedIdentity);
        }
        if matches!(notification, PluginNotification::TurnResult { .. }) {
            if self.terminal_seen {
                return Err(RuntimeProtocolError::DuplicateTerminal);
            }
            self.terminal_seen = true;
        }
        Ok(Some(notification))
    }
    pub fn terminal_seen(&self) -> bool {
        self.terminal_seen
    }
}
fn notification_identity(notification: &PluginNotification) -> (&str, &str) {
    match notification {
        PluginNotification::Progress {
            dispatch_id,
            turn_id,
            ..
        }
        | PluginNotification::ApprovalRequest {
            dispatch_id,
            turn_id,
            ..
        }
        | PluginNotification::ToolCall {
            dispatch_id,
            turn_id,
            ..
        }
        | PluginNotification::Usage {
            dispatch_id,
            turn_id,
            ..
        }
        | PluginNotification::TurnResult {
            dispatch_id,
            turn_id,
            ..
        } => (dispatch_id, turn_id),
    }
}

pub fn encode_request(id: &str, request: &HostRequest) -> Result<Vec<u8>, RuntimeProtocolError> {
    if id.is_empty() || id.len() > 128 {
        return Err(RuntimeProtocolError::InvalidId);
    }
    let value = serde_json::json!({"jsonrpc":"2.0","id":id,"method":request_method(request),"params":request_params(request)});
    let mut bytes = serde_json::to_vec(&value)
        .map_err(|error| RuntimeProtocolError::Malformed(error.to_string()))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(RuntimeProtocolError::Capacity);
    }
    Ok(bytes)
}

pub fn decode_frame(line: &[u8]) -> Result<PluginFrame, RuntimeProtocolError> {
    if line.len() > MAX_FRAME_BYTES {
        return Err(RuntimeProtocolError::Capacity);
    }
    let value: Value = serde_json::from_slice(line)
        .map_err(|error| RuntimeProtocolError::Malformed(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| RuntimeProtocolError::Malformed("frame must be an object".into()))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(RuntimeProtocolError::Malformed(
            "jsonrpc must be 2.0".into(),
        ));
    }
    if let Some(id) = object.get("id") {
        let id = id
            .as_str()
            .ok_or(RuntimeProtocolError::InvalidId)?
            .to_owned();
        if id.is_empty() || id.len() > 128 {
            return Err(RuntimeProtocolError::InvalidId);
        }
        if let Some(result) = object.get("result") {
            return Ok(PluginFrame::Response {
                id,
                result: result.clone(),
            });
        }
        if let Some(error) = object.get("error") {
            return Ok(PluginFrame::Error {
                id,
                error: error.clone(),
            });
        }
        return Err(RuntimeProtocolError::Malformed(
            "id frame must contain result or error".into(),
        ));
    }
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| RuntimeProtocolError::Malformed("notification method is missing".into()))?;
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    Ok(PluginFrame::Notification(parse_notification(
        method, params,
    )?))
}

fn parse_notification(
    method: &str,
    params: Value,
) -> Result<PluginNotification, RuntimeProtocolError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Progress {
        dispatch_id: String,
        turn_id: String,
        text: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Approval {
        dispatch_id: String,
        turn_id: String,
        approval_id: String,
        tool: String,
        input: Value,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Tool {
        dispatch_id: String,
        turn_id: String,
        call_id: String,
        tool: String,
        input: Value,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Usage {
        dispatch_id: String,
        turn_id: String,
        usage: Option<RuntimeUsage>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ResultFrame {
        dispatch_id: String,
        turn_id: String,
        outcome: RuntimeOutcome,
        final_text: Option<String>,
        error: Option<String>,
        usage: Option<RuntimeUsage>,
    }
    match method {
        "progress" => serde_json::from_value(params)
            .map(|v: Progress| PluginNotification::Progress {
                dispatch_id: v.dispatch_id,
                turn_id: v.turn_id,
                text: v.text,
            })
            .map_err(invalid),
        "approval/request" => serde_json::from_value(params)
            .map(|v: Approval| PluginNotification::ApprovalRequest {
                dispatch_id: v.dispatch_id,
                turn_id: v.turn_id,
                approval_id: v.approval_id,
                tool: v.tool,
                input: v.input,
            })
            .map_err(invalid),
        "tool/call" => serde_json::from_value(params)
            .map(|v: Tool| PluginNotification::ToolCall {
                dispatch_id: v.dispatch_id,
                turn_id: v.turn_id,
                call_id: v.call_id,
                tool: v.tool,
                input: v.input,
            })
            .map_err(invalid),
        "usage" => serde_json::from_value(params)
            .map(|v: Usage| PluginNotification::Usage {
                dispatch_id: v.dispatch_id,
                turn_id: v.turn_id,
                usage: v.usage,
            })
            .map_err(invalid),
        "turn/result" => serde_json::from_value(params)
            .map(|v: ResultFrame| PluginNotification::TurnResult {
                dispatch_id: v.dispatch_id,
                turn_id: v.turn_id,
                outcome: v.outcome,
                final_text: v.final_text,
                error: v.error,
                usage: v.usage,
            })
            .map_err(invalid),
        other => Err(RuntimeProtocolError::UnsupportedMethod(other.into())),
    }
}
fn invalid(error: serde_json::Error) -> RuntimeProtocolError {
    RuntimeProtocolError::Malformed(error.to_string())
}
fn request_method(request: &HostRequest) -> &'static str {
    match request {
        HostRequest::Initialize { .. } => "initialize",
        HostRequest::TurnStart { .. } => "turn/start",
        HostRequest::ApprovalRespond { .. } => "approval/respond",
        HostRequest::ToolResult { .. } => "tool/result",
        HostRequest::TurnCancel { .. } => "turn/cancel",
        HostRequest::Shutdown => "shutdown",
    }
}
fn request_params(request: &HostRequest) -> Value {
    match request {
        HostRequest::Initialize {
            protocol,
            capabilities,
        } => serde_json::json!({"protocol":protocol,"capabilities":capabilities}),
        HostRequest::TurnStart {
            dispatch_id,
            turn_id,
            prompt,
            workspace,
            harness_context,
            tools,
        } => {
            let mut params = serde_json::json!({"dispatchId":dispatch_id,"turnId":turn_id,"prompt":prompt,"workspace":workspace,"harnessContext":harness_context});
            // Omitted entirely for runtimes without the tool capability, so
            // their `turn/start` frame is unchanged.
            if let Some(tools) = tools {
                params["tools"] = serde_json::to_value(tools).unwrap_or(Value::Null);
            }
            params
        }
        HostRequest::ApprovalRespond {
            dispatch_id,
            turn_id,
            approval_id,
            decision,
        } => {
            serde_json::json!({"dispatchId":dispatch_id,"turnId":turn_id,"approvalId":approval_id,"decision":decision})
        }
        HostRequest::ToolResult {
            dispatch_id,
            turn_id,
            call_id,
            result,
        } => {
            serde_json::json!({"dispatchId":dispatch_id,"turnId":turn_id,"callId":call_id,"result":result})
        }
        HostRequest::TurnCancel {
            dispatch_id,
            turn_id,
        } => serde_json::json!({"dispatchId":dispatch_id,"turnId":turn_id}),
        HostRequest::Shutdown => Value::Null,
    }
}
