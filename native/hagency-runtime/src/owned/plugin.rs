use super::{Cleanup, StartError};
use hagency_platform::{Launch, SupervisedProcess};
use hagency_plugin::{
    HostRequest, PluginFrame, PluginNotification, RuntimeLaunch, RuntimeOutcome, RuntimeSession,
    TOOL_CAPABILITY, ToolDescriptor, decode_frame, encode_request,
};
use std::{
    collections::BTreeSet,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;

#[cfg(unix)]
type Receiver = tokio::net::unix::pipe::Receiver;
#[cfg(unix)]
type Sender = tokio::net::unix::pipe::Sender;
#[cfg(windows)]
type Receiver = hagency_platform::WindowsPipe;
#[cfg(windows)]
type Sender = hagency_platform::WindowsPipe;

const STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// How long the host waits on a runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginTimeouts {
    /// For the answer to a host request (`initialize`, `turn/start`,
    /// `shutdown`, ...): a runtime must acknowledge promptly.
    pub response: Duration,
    /// For the next frame while a turn runs. Runtimes should send `progress`
    /// heartbeats; this bounds a runtime that has stopped responding, while
    /// the dispatch budget still bounds the whole turn.
    pub turn_idle: Duration,
}

/// Boxed future returned by [`PluginToolHost`] methods.
pub type ToolFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Host task tools for one dispatch, executed by the host with its own
/// authority. The runtime only names a tool and its input; it never receives
/// a runner capability.
pub trait PluginToolHost: Send {
    /// Starts the tools and returns the catalog offered to the runtime.
    fn prepare(&mut self) -> ToolFuture<'_, Result<Vec<ToolDescriptor>, ()>>;
    /// Runs one call: `Ok` is the tool output, `Err` its failure message.
    fn call<'a>(
        &'a mut self,
        tool: &'a str,
        input: serde_json::Value,
    ) -> ToolFuture<'a, Result<serde_json::Value, String>>;
}

/// The `tool/result` payload for one call.
fn tool_result(result: Result<serde_json::Value, String>) -> serde_json::Value {
    match result {
        Ok(output) => serde_json::json!({"ok": true, "output": output}),
        Err(error) => serde_json::json!({"ok": false, "error": error}),
    }
}

#[derive(Debug, Error)]
pub enum PluginSessionError {
    #[error("plugin runtime IO failed")]
    Io(#[from] io::Error),
    #[error("plugin runtime protocol failed: {0}")]
    Protocol(String),
    #[error("plugin runtime response timed out")]
    Timeout,
    #[error("plugin runtime cancellation requested")]
    Cancelled,
    #[error("plugin runtime refused host authority")]
    Refused,
}

#[derive(Debug, Clone)]
pub struct PluginTurn {
    pub outcome: RuntimeOutcome,
    pub final_text: Option<String>,
    pub error: Option<String>,
    pub usage: Option<hagency_plugin::RuntimeUsage>,
}

pub struct OwnedPluginSession {
    owner: SupervisedProcess,
    stdin: Option<Sender>,
    stdout: Option<BufReader<Receiver>>,
    stderr_tail: Arc<std::sync::Mutex<Vec<u8>>>,
    cleanup: Cleanup,
    dispatch_id: String,
    turn_id: String,
}
impl OwnedPluginSession {
    pub fn spawn(
        guardian: &Path,
        launch: &RuntimeLaunch,
        workspace: PathBuf,
        environment: std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>,
    ) -> Result<Self, StartError> {
        if tokio::runtime::Handle::try_current().is_err()
            || launch.executable.as_os_str().is_empty()
        {
            return Err(StartError::Settings);
        }
        let process_launch = Launch {
            executable: launch.executable.clone(),
            arguments: launch.arguments.clone(),
            directory: workspace,
            environment,
            require_crash_containment: false,
        };
        let (owner, pipes) =
            SupervisedProcess::spawn_piped(guardian, &process_launch).map_err(|error| {
                if error.kind() == io::ErrorKind::Unsupported {
                    StartError::Unsupported
                } else {
                    StartError::Uncertain {
                        kind: error.kind(),
                        cleanup: Cleanup::Unknown { kind: error.kind() },
                    }
                }
            })?;
        let (stdin, stdout, stderr) = match Self::pipes(pipes) {
            Ok(value) => value,
            Err(error) => {
                let mut owner = owner;
                let cleanup = observe_stop(&mut owner);
                return Err(StartError::Uncertain {
                    kind: error.kind(),
                    cleanup,
                });
            }
        };
        let stderr_tail = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stderr_capture = Arc::clone(&stderr_tail);
        tokio::spawn(async move {
            let mut stderr = stderr;
            let mut chunk = [0u8; 4096];
            while let Ok(size) = stderr.read(&mut chunk).await {
                if size == 0 {
                    break;
                }
                if let Ok(mut tail) = stderr_capture.lock() {
                    tail.extend_from_slice(&chunk[..size]);
                    if tail.len() > 64 * 1024 {
                        let drop = tail.len() - 64 * 1024;
                        tail.drain(..drop);
                    }
                }
            }
        });
        Ok(Self {
            owner,
            stdin: Some(stdin),
            stdout: Some(BufReader::new(stdout)),
            stderr_tail,
            cleanup: Cleanup::Pending,
            dispatch_id: String::new(),
            turn_id: String::new(),
        })
    }

    #[cfg(unix)]
    fn pipes(pipes: hagency_platform::StdioPipes) -> io::Result<(Sender, Receiver, Receiver)> {
        let (stdin, stdout, stderr) = pipes.into_parts();
        Ok((
            Sender::from_owned_fd(stdin)?,
            Receiver::from_owned_fd(stdout)?,
            Receiver::from_owned_fd(stderr)?,
        ))
    }
    #[cfg(windows)]
    fn pipes(pipes: hagency_platform::StdioPipes) -> io::Result<(Sender, Receiver, Receiver)> {
        pipes.into_async_parts()
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run_turn(
        &mut self,
        dispatch_id: String,
        turn_id: String,
        prompt: String,
        workspace: String,
        harness_context: Option<serde_json::Value>,
        timeouts: PluginTimeouts,
        cancel: Arc<AtomicBool>,
        mut tools: Option<&mut dyn PluginToolHost>,
    ) -> Result<PluginTurn, PluginSessionError> {
        self.dispatch_id = dispatch_id.clone();
        self.turn_id = turn_id.clone();
        let initialized = self
            .request(
                "initialize",
                HostRequest::Initialize {
                    protocol: "hagency.runtime/v1".into(),
                    capabilities: vec![
                        "cancel".into(),
                        "approval".into(),
                        "tool".into(),
                        "usage".into(),
                    ],
                },
                timeouts.response,
            )
            .await?;
        // Tools are offered only to a runtime that asked for them, and only
        // when the host serves task tools for this dispatch.
        let wants_tools = initialized
            .get("capabilities")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|capabilities| {
                capabilities
                    .iter()
                    .any(|capability| capability.as_str() == Some(TOOL_CAPABILITY))
            });
        let offered = match tools.as_deref_mut() {
            Some(host) if wants_tools => Some(
                host.prepare()
                    .await
                    .map_err(|()| PluginSessionError::Refused)?,
            ),
            _ => None,
        };
        let catalog: BTreeSet<String> = offered
            .iter()
            .flatten()
            .map(|tool| tool.name.clone())
            .collect();
        self.request(
            "turn-start",
            HostRequest::TurnStart {
                dispatch_id: dispatch_id.clone(),
                turn_id: turn_id.clone(),
                prompt,
                workspace,
                harness_context,
                tools: offered,
            },
            timeouts.response,
        )
        .await?;
        let mut protocol = RuntimeSession::new(&dispatch_id, &turn_id)
            .map_err(|error| PluginSessionError::Protocol(error.to_string()))?;
        let mut last_usage = None;
        let mut calls: BTreeSet<String> = BTreeSet::new();
        loop {
            if cancel.load(Ordering::Acquire) {
                self.send_only(
                    "turn-cancel",
                    &HostRequest::TurnCancel {
                        dispatch_id: dispatch_id.clone(),
                        turn_id: turn_id.clone(),
                    },
                )
                .await?;
                self.request("shutdown", HostRequest::Shutdown, timeouts.response)
                    .await?;
                return Err(PluginSessionError::Cancelled);
            }
            match self.read_frame(timeouts.turn_idle).await? {
                PluginFrame::Response { .. } => {}
                PluginFrame::Error { error, .. } => {
                    return Err(PluginSessionError::Protocol(error.to_string()));
                }
                PluginFrame::Notification(notification) => {
                    let notification = protocol
                        .accept(PluginFrame::Notification(notification))
                        .map_err(|error| PluginSessionError::Protocol(error.to_string()))?
                        .ok_or_else(|| {
                            PluginSessionError::Protocol("missing plugin notification".into())
                        })?;
                    match notification {
                        PluginNotification::ApprovalRequest {
                            approval_id,
                            tool,
                            input,
                            ..
                        } => {
                            // Runtime-side approvals are not routed to the
                            // owner yet: refuse rather than approve.
                            let _ = (tool, input);
                            self.send_only(
                                &format!("approval-{approval_id}"),
                                &HostRequest::ApprovalRespond {
                                    dispatch_id: dispatch_id.clone(),
                                    turn_id: turn_id.clone(),
                                    approval_id,
                                    decision: "deny".into(),
                                },
                            )
                            .await?;
                        }
                        PluginNotification::ToolCall {
                            call_id,
                            tool,
                            input,
                            ..
                        } => {
                            let result = if !calls.insert(call_id.clone()) {
                                // A repeated call_id is never executed twice.
                                Err(format!("duplicate call_id {call_id}"))
                            } else if !catalog.contains(&tool) {
                                Err(format!("{tool} is not a Hagency task tool of this task"))
                            } else if let Some(host) = tools.as_deref_mut() {
                                host.call(&tool, input).await
                            } else {
                                Err("tool execution remains with Hagency host".into())
                            };
                            self.send_only(
                                &format!("tool-{call_id}"),
                                &HostRequest::ToolResult {
                                    dispatch_id: dispatch_id.clone(),
                                    turn_id: turn_id.clone(),
                                    call_id,
                                    result: tool_result(result),
                                },
                            )
                            .await?;
                        }
                        PluginNotification::Progress { .. } => {}
                        PluginNotification::Usage { usage, .. } => last_usage = usage,
                        PluginNotification::TurnResult {
                            outcome,
                            final_text,
                            error,
                            usage,
                            ..
                        } => {
                            let turn = PluginTurn {
                                outcome,
                                final_text,
                                error,
                                usage: usage.or(last_usage),
                            };
                            self.request("shutdown", HostRequest::Shutdown, timeouts.response)
                                .await?;
                            return Ok(turn);
                        }
                    }
                }
            }
        }
    }

    /// Ask the runtime to cancel before the host tears down custody. This is
    /// best-effort and remains bounded; cleanup still owns the final verdict.
    pub async fn cancel_turn(&mut self, wait: Duration) -> Result<(), PluginSessionError> {
        if self.dispatch_id.is_empty() || self.turn_id.is_empty() {
            return Err(PluginSessionError::Protocol(
                "turn identity is unset".into(),
            ));
        }
        self.request(
            "turn-cancel",
            HostRequest::TurnCancel {
                dispatch_id: self.dispatch_id.clone(),
                turn_id: self.turn_id.clone(),
            },
            wait,
        )
        .await?;
        self.request("shutdown", HostRequest::Shutdown, wait)
            .await
            .map(|_| ())
    }

    async fn request(
        &mut self,
        id: &str,
        request: HostRequest,
        response_timeout: Duration,
    ) -> Result<serde_json::Value, PluginSessionError> {
        let bytes = encode_request(id, &request)
            .map_err(|error| PluginSessionError::Protocol(error.to_string()))?;
        self.write(&bytes, response_timeout).await?;
        loop {
            match self.read_frame(response_timeout).await? {
                PluginFrame::Response {
                    id: response_id,
                    result,
                } if response_id == id => return Ok(result),
                PluginFrame::Error {
                    id: response_id,
                    error,
                } if response_id == id => {
                    return Err(PluginSessionError::Protocol(error.to_string()));
                }
                PluginFrame::Notification(_)
                | PluginFrame::Response { .. }
                | PluginFrame::Error { .. } => {}
            }
        }
    }

    async fn send_only(
        &mut self,
        id: &str,
        request: &HostRequest,
    ) -> Result<(), PluginSessionError> {
        let bytes = encode_request(id, request)
            .map_err(|error| PluginSessionError::Protocol(error.to_string()))?;
        self.write(&bytes, Duration::from_secs(2)).await
    }

    async fn write(&mut self, bytes: &[u8], wait: Duration) -> Result<(), PluginSessionError> {
        let stdin = self.stdin.as_mut().ok_or(PluginSessionError::Refused)?;
        timeout(wait, async {
            stdin.write_all(bytes).await?;
            stdin.flush().await
        })
        .await
        .map_err(|_| PluginSessionError::Timeout)??;
        Ok(())
    }

    async fn read_frame(&mut self, wait: Duration) -> Result<PluginFrame, PluginSessionError> {
        let stdout = self.stdout.as_mut().ok_or(PluginSessionError::Refused)?;
        let mut line = Vec::new();
        let read = timeout(wait, stdout.read_until(b'\n', &mut line))
            .await
            .map_err(|_| PluginSessionError::Timeout)??;
        if read == 0 || line.len() > hagency_plugin::MAX_FRAME_BYTES {
            return Err(PluginSessionError::Protocol(
                "plugin EOF or oversized frame".into(),
            ));
        }
        decode_frame(&line).map_err(|error| PluginSessionError::Protocol(error.to_string()))
    }

    pub fn stop(&mut self) -> Cleanup {
        self.stdin.take();
        self.stdout.take();
        if !matches!(self.cleanup, Cleanup::Observed(report) if report.scope.whole_tree_stopped) {
            self.cleanup = observe_stop(&mut self.owner);
        }
        self.cleanup
    }
    pub fn cleanup(&self) -> Cleanup {
        self.cleanup
    }
    pub fn exit_identity(&self) -> Option<String> {
        match self.cleanup {
            Cleanup::Observed(report) => Some(format!(
                "leader_status={:?};guardian_exit={:?};live_count={};cause={:?}",
                report.leader_status, report.guardian_exit, report.live_count, report.cause
            )),
            Cleanup::Pending | Cleanup::Unknown { .. } => None,
        }
    }
    pub fn stderr_tail(&self, max: usize) -> String {
        let tail = self.stderr_tail.lock().ok();
        let bytes = tail.as_ref().map_or(&[][..], |guard| guard.as_slice());
        let start = bytes.len().saturating_sub(max);
        String::from_utf8_lossy(&bytes[start..]).into_owned()
    }
    pub fn guardian_stderr_tail(&self) -> String {
        self.owner.guardian_stderr_tail()
    }
}
impl Drop for OwnedPluginSession {
    fn drop(&mut self) {
        self.stop();
    }
}
fn observe_stop(owner: &mut SupervisedProcess) -> Cleanup {
    match owner.stop(STOP_TIMEOUT) {
        Ok(report) => Cleanup::Observed(report),
        Err(error) => Cleanup::Unknown { kind: error.kind() },
    }
}
