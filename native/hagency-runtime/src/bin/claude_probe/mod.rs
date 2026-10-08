//! Explicit offline peer mode; never invokes Claude or another provider.
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufRead, Write},
    path::Path,
};

fn emit(value: Value) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}
pub(super) fn run(mode: &str, marker: &Path) -> io::Result<()> {
    if !matches!(
        mode,
        "normal"
            | "malformed"
            | "stall"
            | "permission-allow"
            | "permission-deny"
            | "permission-cancel"
            | "permission-hold"
            | "usage"
            | "usage-conflict"
    ) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    fs::write(marker.with_extension("entered"), b"offline")?;
    let mut stdin = io::stdin().lock();
    let mut line = String::new();
    stdin.read_line(&mut line)?;
    let request: Value = serde_json::from_str(&line)?;
    if request["type"] != "control_request" || request["request"]["subtype"] != "initialize" {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    if mode == "stall" {
        drop(stdin);
        return super::pulse(marker);
    }
    if mode == "malformed" {
        io::stdout().write_all(b"{\"type\":\"unknown\"}\n")?;
        io::stdout().flush()?;
        drop(stdin);
        return super::pulse(marker);
    }
    emit(
        json!({"type":"control_response","response":{"subtype":"success","request_id":request["request_id"],"response":{}}}),
    )?;
    line.clear();
    stdin.read_line(&mut line)?;
    let prompt: Value = serde_json::from_str(&line)?;
    if prompt["type"] != "user" || prompt["message"]["role"] != "user" || prompt["session_id"] != ""
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    fs::write(
        marker.with_extension("prompt"),
        serde_json::to_vec(&prompt)?,
    )?;
    emit(json!({"type":"system","subtype":"init","session_id":"owned-claude"}))?;
    if mode.starts_with("permission-") {
        let input = json!({"command":"offline literal $(no shell) 中文"});
        emit(
            json!({"type":"control_request","request_id":"owned-permission","request":{
            "subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"owned-tool","input":input}}),
        )?;
        if mode == "permission-cancel" {
            emit(json!({"type":"control_cancel_request","request_id":"owned-permission"}))?;
            fs::write(marker.with_extension("cancelled"), b"offline")?;
            drop(stdin);
            return super::pulse(marker);
        }
        if mode == "permission-hold" {
            drop(stdin);
            return super::pulse(marker);
        }
        line.clear();
        stdin.read_line(&mut line)?;
        let response: Value = serde_json::from_str(&line)?;
        let expected = if mode == "permission-allow" {
            json!({"behavior":"allow","updatedInput":input})
        } else {
            json!({"behavior":"deny","message":"Permission denied by Hagency.","interrupt":true})
        };
        if response
            != json!({"type":"control_response","response":{
            "subtype":"success","request_id":"owned-permission","response":expected}})
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        fs::write(
            marker.with_extension("response"),
            serde_json::to_vec(&response)?,
        )?;
    }
    if mode.starts_with("usage") {
        let mut message = json!({"type":"assistant","session_id":"owned-claude","uuid":"one","parent_tool_use_id":null,
            "message":{"id":"step-one","usage":{"input_tokens":10,"output_tokens":999,"cache_read_input_tokens":20,"cache_creation_input_tokens":30}}});
        emit(message.clone())?;
        message["uuid"] = json!("two");
        if mode == "usage-conflict" {
            message["message"]["usage"]["input_tokens"] = json!(11);
        }
        emit(message.clone())?;
        message["parent_tool_use_id"] = json!("parent");
        emit(message.clone())?;
        message["parent_tool_use_id"] = Value::Null;
        message["message"]["id"] = json!("step-two");
        emit(message)?;
        emit(
            json!({"type":"result","subtype":"success","session_id":"owned-claude","is_error":true,
            "modelUsage":{"private-model-one":{"inputTokens":100,"outputTokens":200,"cacheReadInputTokens":300,"cacheCreationInputTokens":400},
                "private-model-two":{"inputTokens":1,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4}}}),
        )?;
    } else {
        emit(
            json!({"type":"assistant","session_id":"owned-claude","message":{"content":[{"type":"text","text":"offline"}]}}),
        )?;
        emit(
            json!({"type":"result","subtype":"success","session_id":"owned-claude","is_error":false,"result":"offline"}),
        )?;
    }
    // Deliberately remains alive after result AND after stdin closes. Only the
    // original guardian's stop or the bounded fixture fuse ends the pulse.
    drop(stdin);
    super::pulse(marker)
}

fn read(stdin: &mut io::StdinLock<'_>) -> io::Result<Value> {
    let mut line = String::new();
    stdin.read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}
fn control(stdin: &mut io::StdinLock<'_>, subtype: &str) -> io::Result<Value> {
    let request = read(stdin)?;
    if request["type"] != "control_request" || request["request"]["subtype"] != subtype {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(request)
}
fn respond(request: &Value, body: Value) -> io::Result<()> {
    emit(json!({"type":"control_response","response":{
        "subtype":"success","request_id":request["request_id"],"response":body}}))
}
/// The scoped helper binding of ADR-158: status, set, status. The tool list
/// follows the helper's own file flags; the helper itself is never started.
fn bind_helper(stdin: &mut io::StdinLock<'_>, before: &Value) -> io::Result<()> {
    respond(before, json!({"mcpServers":[]}))?;
    let set = control(stdin, "mcp_set_servers")?;
    let server = &set["request"]["servers"]["hagency_task_writer"];
    if server["command"].as_str().is_none()
        || server["args"] != json!(["mcp", "--owned-task-profile"])
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut tools = vec![
        "get_task",
        "list_tasks",
        "update_task_execution",
        "transition_task",
        "complete_task_with_reply",
        "read_conversation",
        "schedule_reminder",
    ];
    if server["env"]["HAGENCY_FILE_TOOLS"] == "1" {
        tools.extend(["send_file", "get_file_delivery"]);
    }
    if server["env"]["HAGENCY_RECEIVE_FILE_TOOLS"] == "1" {
        tools.extend(["list_received_files", "receive_file"]);
    }
    respond(
        &set,
        json!({"added":["hagency_task_writer"],"removed":[],"errors":{}}),
    )?;
    let after = control(stdin, "mcp_status")?;
    respond(
        &after,
        json!({"mcpServers":[{"name":"hagency_task_writer","status":"connected",
            "tools":tools.iter().map(|name| json!({"name":name})).collect::<Vec<_>>()}]}),
    )?;
    Ok(())
}
/// One `can_use_tool` request of the approval modes, always the same tool and
/// input, so a task or always grant on the first one matches the next.
fn permission(index: usize) -> io::Result<()> {
    emit(
        json!({"type":"control_request","request_id":format!("owned-permission-{index}"),
        "request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":format!("owned-tool-{index}"),
            "input":{"command":"offline literal","description":"Offline step"}}}),
    )
}
/// Every answer Hagency wrote, one JSON line each, for the test to compare.
fn record(marker: &Path, response: &Value) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(marker.with_extension("responses"))?;
    serde_json::to_writer(&mut file, response)?;
    file.write_all(b"\n")
}
fn error_result() -> io::Result<()> {
    emit(json!({"type":"result","subtype":"error_during_execution",
        "session_id":"owned-claude","is_error":true,
        "modelUsage":{"claude-fixture":{"inputTokens":10,"outputTokens":7,"cacheReadInputTokens":20,"cacheCreationInputTokens":30}}}))
}
/// A Claude launched by the execution Host with the fixed task profile
/// (ADR-158): initialize, then the scoped helper binding, then one prompt.
/// Never starts the helper or a provider; the reply is the result text.
/// The approval modes ask for one tool use (twice in `task-approval-twice`)
/// and continue as the installed CLI does: an allow runs the tool and the
/// turn succeeds; a deny interrupts it into an error result (ADR-156).
pub(super) fn run_task(mode: &str, marker: &Path) -> io::Result<()> {
    if !matches!(
        mode,
        "task"
            | "task-error"
            | "task-permission"
            | "task-approval"
            | "task-approval-twice"
            | "task-approval-cancel"
    ) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    fs::write(marker.with_extension("entered"), b"offline")?;
    let mut stdin = io::stdin().lock();
    let initialize = control(&mut stdin, "initialize")?;
    respond(&initialize, json!({}))?;
    // The helper binding comes only from a host with a task helper.
    let mut next = read(&mut stdin)?;
    if next["type"] == "control_request" && next["request"]["subtype"] == "mcp_status" {
        bind_helper(&mut stdin, &next)?;
        next = read(&mut stdin)?;
    }
    let prompt = next;
    if prompt["type"] != "user" || prompt["message"]["role"] != "user" {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    fs::write(
        marker.with_extension("prompt"),
        serde_json::to_vec(&prompt)?,
    )?;
    emit(json!({"type":"system","subtype":"init","session_id":"owned-claude"}))?;
    emit(
        json!({"type":"assistant","session_id":"owned-claude","uuid":"one","parent_tool_use_id":null,
        "message":{"id":"step-one","content":[{"type":"text","text":"working"}],
            "usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":20,"cache_creation_input_tokens":30}}}),
    )?;
    let success = || {
        emit(
            json!({"type":"result","subtype":"success","session_id":"owned-claude",
            "is_error":false,"result":"claude fixture reply",
            "modelUsage":{"claude-fixture":{"inputTokens":10,"outputTokens":7,"cacheReadInputTokens":20,"cacheCreationInputTokens":30}}}),
        )
    };
    match mode {
        "task" => success()?,
        "task-error" => error_result()?,
        "task-permission" => emit(
            json!({"type":"control_request","request_id":"owned-permission","request":{
            "subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"owned-tool",
            "input":{"command":"offline literal"}}}),
        )?,
        "task-approval-cancel" => {
            permission(1)?;
            emit(json!({"type":"control_cancel_request","request_id":"owned-permission-1"}))?;
            fs::write(marker.with_extension("cancelled"), b"offline")?;
        }
        _ => {
            let count = if mode == "task-approval-twice" { 2 } else { 1 };
            let mut allowed = true;
            for index in 1..=count {
                permission(index)?;
                let response = read(&mut stdin)?;
                record(marker, &response)?;
                if response["response"]["response"]["behavior"] != "allow" {
                    allowed = false;
                    break;
                }
            }
            if allowed { success()? } else { error_result()? }
        }
    }
    // Alive after its result: only the original guardian's stop or the bounded
    // fixture fuse ends it, as for the single-prompt modes above.
    drop(stdin);
    super::pulse(marker)
}
