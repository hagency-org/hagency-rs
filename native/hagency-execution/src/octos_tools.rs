//! ADR-193 decision 5: an Octos agent's task tools are Hagency's host tools.
//! Each dispatch runs the same scoped task helper Codex and Claude Code reach
//! (`hagency mcp --owned-task-profile`), here as Hagency's own child on the
//! dispatch's capability, and relays Octos's `peer/tool/call`s to it. The
//! helper and the loopback API behind it check every call exactly as they do
//! for the other two agents; this module only translates frames.
use hagency_runtime::octos::session::HostTool;
use serde_json::{Value, json};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

/// The helper's own bounds (`hagency::mcp`): one request frame, one answer.
const REQUEST_LIMIT: usize = 32 * 1024;
const ANSWER_LIMIT: usize = 256 * 1024;
/// Starting the helper and listing its tools.
const START_MS: u64 = 10_000;
/// One tool call: inside Octos's own wait (`HOST_TOOL_TIMEOUT_MS`).
const CALL_MS: u64 = 45_000;

/// What the host prepared for one Octos dispatch: the helper and the exact
/// environment it inherits. No Debug: the environment holds the capability.
pub(crate) struct Spec {
    pub(crate) executable: PathBuf,
    pub(crate) environment: BTreeMap<OsString, OsString>,
    /// The helper tool names this dispatch may call.
    pub(crate) tools: Vec<&'static str>,
}

/// One running helper. Dropping it kills the child.
pub(crate) struct HostTools {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
    names: Vec<&'static str>,
    broken: bool,
}
impl HostTools {
    /// Start the helper, finish the MCP handshake and read its catalog. The
    /// returned tools are the catalog's entries this dispatch may call, as
    /// Octos registers them.
    pub(crate) async fn start(spec: &Spec) -> Result<(Self, Vec<HostTool>), ()> {
        let mut command = Command::new(&spec.executable);
        command
            .args(["mcp", "--owned-task-profile"])
            .env_clear()
            .envs(&spec.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| ())?;
        let stdin = child.stdin.take().ok_or(())?;
        let stdout = BufReader::new(child.stdout.take().ok_or(())?);
        let mut tools = Self {
            child,
            stdin,
            stdout,
            next: 0,
            names: spec.tools.clone(),
            broken: false,
        };
        let catalog = tokio::time::timeout(Duration::from_millis(START_MS), tools.handshake())
            .await
            .map_err(|_| ())??;
        let mut registered = Vec::new();
        for entry in catalog["tools"].as_array().ok_or(())? {
            let name = entry["name"].as_str().ok_or(())?;
            if !tools.names.contains(&name) {
                continue;
            }
            registered.push(HostTool {
                name: format!("{}.{name}", hagency_runtime::octos::task_tools::APP),
                description: entry["description"].as_str().ok_or(())?.to_owned(),
                input_schema: entry["inputSchema"].clone(),
                read_only: entry["annotations"]["readOnlyHint"] == true,
            });
        }
        // Every tool the profile offers must be served.
        if registered.len() != tools.names.len() {
            return Err(());
        }
        Ok((tools, registered))
    }
    async fn handshake(&mut self) -> Result<Value, ()> {
        self.request(
            "initialize",
            json!({"protocolVersion":"2025-11-25","capabilities":{},
                "clientInfo":{"name":"hagency-octos","version":env!("CARGO_PKG_VERSION")}}),
        )
        .await?
        .map_err(|_| ())?;
        self.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await?;
        self.request("tools/list", Value::Null)
            .await?
            .map_err(|_| ())
    }
    /// One Octos call, by its registered (dotted) name: `Ok` is the tool's
    /// output for the model, `Err` its failure message. A helper that failed
    /// once is not used again; every later call fails in the same words.
    pub(crate) async fn call(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let unavailable = || "Hagency task tools are unavailable for this task".to_owned();
        if self.broken {
            return Err(unavailable());
        }
        let Some(tool) = name
            .strip_prefix(hagency_runtime::octos::task_tools::APP)
            .and_then(|rest| rest.strip_prefix('.'))
            .filter(|tool| self.names.contains(tool))
        else {
            return Err(format!("{name} is not a Hagency task tool of this task"));
        };
        let params = json!({"name":tool,"arguments":args});
        // The helper ends itself on a frame over its bound: refuse it here.
        if params.to_string().len() + 128 > REQUEST_LIMIT {
            return Err(format!(
                "{name} arguments exceed the {REQUEST_LIMIT}-byte request limit"
            ));
        }
        let answer = tokio::time::timeout(
            Duration::from_millis(CALL_MS),
            self.request("tools/call", params),
        )
        .await;
        let result = match answer {
            Ok(Ok(Ok(result))) => result,
            Ok(Ok(Err(error))) => {
                return Err(error["message"]
                    .as_str()
                    .unwrap_or("the task tool refused the call")
                    .to_owned());
            }
            // A lost answer is uncertain: the call may have been applied.
            Ok(Err(())) | Err(_) => {
                self.broken = true;
                return Err(format!(
                    "{name} outcome unknown: the task helper stopped answering. \
                     Inspect with the read tools before repeating the same call_id."
                ));
            }
        };
        let text = result["content"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if result["isError"] == true {
            return Err(text);
        }
        Ok(match result.get("structuredContent") {
            Some(structured) if !structured.is_null() => structured.clone(),
            _ => Value::String(text),
        })
    }
    async fn request(&mut self, method: &str, params: Value) -> Result<Result<Value, Value>, ()> {
        self.next += 1;
        let id = self.next;
        let mut frame = json!({"jsonrpc":"2.0","id":id,"method":method});
        if !params.is_null() {
            frame["params"] = params;
        }
        self.send(&frame).await?;
        let mut line = Vec::new();
        let read = (&mut self.stdout)
            .take(ANSWER_LIMIT as u64 + 1)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|_| ())?;
        if read == 0 || line.len() > ANSWER_LIMIT || line.last() != Some(&b'\n') {
            return Err(());
        }
        let answer: Value = serde_json::from_slice(&line).map_err(|_| ())?;
        // The helper answers in order and sends no notifications.
        if answer["id"] != id {
            return Err(());
        }
        Ok(match answer.get("error") {
            Some(error) => Err(error.clone()),
            None => Ok(answer.get("result").cloned().ok_or(())?),
        })
    }
    async fn send(&mut self, frame: &Value) -> Result<(), ()> {
        let mut bytes = serde_json::to_vec(frame).map_err(|_| ())?;
        if bytes.len() > REQUEST_LIMIT {
            return Err(());
        }
        bytes.push(b'\n');
        self.stdin.write_all(&bytes).await.map_err(|_| ())?;
        self.stdin.flush().await.map_err(|_| ())
    }
}
impl Drop for HostTools {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
