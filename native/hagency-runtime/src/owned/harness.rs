use super::{Cleanup, StartError};
use hagency_platform::{Launch, SupervisedProcess};
use hagency_plugin::{
    HarnessFrame, HarnessLaunch, HarnessPrepared, HarnessRequest, decode_harness_frame,
    encode_harness_request,
};
use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;

#[cfg(unix)]
type Receiver = tokio::net::unix::pipe::Receiver;
#[cfg(unix)]
type Sender = tokio::net::unix::pipe::Sender;
#[cfg(windows)]
type Receiver = hagency_platform::WindowsPipe;
#[cfg(windows)]
type Sender = hagency_platform::WindowsPipe;

pub struct OwnedHarnessSession {
    owner: SupervisedProcess,
    stdin: Option<Sender>,
    stdout: Option<BufReader<Receiver>>,
    cleanup: Cleanup,
    next_request_id: u64,
}
impl OwnedHarnessSession {
    pub fn spawn(
        guardian: &Path,
        launch: &HarnessLaunch,
        workspace: PathBuf,
        environment: std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>,
    ) -> Result<Self, StartError> {
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
        let (stdin, stdout, _) = match pipes_to_async(pipes) {
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
        Ok(Self {
            owner,
            stdin: Some(stdin),
            stdout: Some(BufReader::new(stdout)),
            cleanup: Cleanup::Pending,
            next_request_id: 0,
        })
    }
    fn next_request_id(&mut self, prefix: &str) -> String {
        self.next_request_id = self.next_request_id.saturating_add(1);
        format!("{prefix}-{}", self.next_request_id)
    }
    async fn write_frame(&mut self, bytes: &[u8], wait: Duration) -> Result<(), io::Error> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "harness stdin closed"))?;
        timeout(wait, async {
            stdin.write_all(bytes).await?;
            stdin.flush().await
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "harness write timed out"))??;
        Ok(())
    }
    pub async fn request_value(
        &mut self,
        id: &str,
        request: &HarnessRequest,
        wait: Duration,
    ) -> Result<serde_json::Value, io::Error> {
        self.request_value_with(id, request, wait, |_| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "harness control is not admitted",
            ))
        })
        .await
    }
    /// Send one host request and keep handling harness control requests while
    /// waiting for its exact response. The callback is the host authority;
    /// harness output alone never authorizes a control.
    pub async fn request_value_with<F>(
        &mut self,
        id: &str,
        request: &HarnessRequest,
        wait: Duration,
        mut control: F,
    ) -> Result<serde_json::Value, io::Error>
    where
        F: FnMut(&HarnessRequest) -> Result<serde_json::Value, io::Error>,
    {
        let bytes = encode_harness_request(id, request)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid harness frame"))?;
        self.write_frame(&bytes, wait).await?;
        loop {
            let stdout = self.stdout.as_mut().ok_or_else(|| {
                io::Error::new(io::ErrorKind::BrokenPipe, "harness stdout closed")
            })?;
            let mut line = Vec::new();
            let size = timeout(wait, stdout.read_until(b'\n', &mut line))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "harness read timed out"))??;
            if size == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "harness exited",
                ));
            }
            match decode_harness_frame(&line)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid harness frame"))?
            {
                HarnessFrame::Response {
                    id: response_id,
                    result,
                } if response_id == id => return Ok(result),
                HarnessFrame::Error {
                    id: response_id, ..
                } if response_id == id => {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "harness refused request",
                    ));
                }
                HarnessFrame::Request {
                    id: request_id,
                    request,
                } => {
                    let result = control(&request)?;
                    let response = hagency_plugin::encode_harness_response(&request_id, &result)
                        .map_err(|_| {
                            io::Error::new(io::ErrorKind::InvalidInput, "invalid harness response")
                        })?;
                    self.write_frame(&response, wait).await?;
                }
                HarnessFrame::Response { .. } | HarnessFrame::Error { .. } => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unexpected harness response identity",
                    ));
                }
                HarnessFrame::Notification(_) => {}
            }
        }
    }
    pub async fn request_unit(
        &mut self,
        id: &str,
        request: &HarnessRequest,
        wait: Duration,
    ) -> Result<(), io::Error> {
        self.request_value(id, request, wait).await.map(|_| ())
    }
    pub async fn prepare_agent(
        &mut self,
        request: &HarnessRequest,
        wait: Duration,
    ) -> Result<HarnessPrepared, io::Error> {
        self.prepare_agent_with(request, wait, |_| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "harness control is not admitted",
            ))
        })
        .await
    }
    pub async fn prepare_agent_with<F>(
        &mut self,
        request: &HarnessRequest,
        wait: Duration,
        control: F,
    ) -> Result<HarnessPrepared, io::Error>
    where
        F: FnMut(&HarnessRequest) -> Result<serde_json::Value, io::Error>,
    {
        if !matches!(request, HarnessRequest::PrepareAgent { .. }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not an agent prepare request",
            ));
        }
        let id = self.next_request_id("agent-prepare");
        let value = self.request_value_with(&id, request, wait, control).await?;
        serde_json::from_value::<HarnessPrepared>(value)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid harness prepare response",
                )
            })?
            .validate()
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid harness prepare response",
                )
            })
    }
    pub async fn release_agent(
        &mut self,
        request: &HarnessRequest,
        wait: Duration,
    ) -> Result<(), io::Error> {
        self.release_agent_with(request, wait, |_| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "harness control is not admitted",
            ))
        })
        .await
    }
    pub async fn release_agent_with<F>(
        &mut self,
        request: &HarnessRequest,
        wait: Duration,
        control: F,
    ) -> Result<(), io::Error>
    where
        F: FnMut(&HarnessRequest) -> Result<serde_json::Value, io::Error>,
    {
        if !matches!(request, HarnessRequest::ReleaseAgent { .. }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not an agent release request",
            ));
        }
        let id = self.next_request_id("agent-release");
        self.request_value_with(&id, request, wait, control)
            .await
            .map(|_| ())
    }
    pub async fn shutdown(&mut self, wait: Duration) -> Result<(), io::Error> {
        let id = self.next_request_id("shutdown");
        self.request_unit(&id, &HarnessRequest::Shutdown, wait)
            .await
    }
    pub fn stop(&mut self) -> Cleanup {
        self.stdin.take();
        self.stdout.take();
        if !matches!(self.cleanup, Cleanup::Observed(report) if report.scope.whole_tree_stopped) {
            self.cleanup = observe_stop(&mut self.owner);
        }
        self.cleanup
    }
}
impl Drop for OwnedHarnessSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(unix)]
fn pipes_to_async(pipes: hagency_platform::StdioPipes) -> io::Result<(Sender, Receiver, Receiver)> {
    let (stdin, stdout, stderr) = pipes.into_parts();
    Ok((
        Sender::from_owned_fd(stdin)?,
        Receiver::from_owned_fd(stdout)?,
        Receiver::from_owned_fd(stderr)?,
    ))
}
#[cfg(windows)]
fn pipes_to_async(pipes: hagency_platform::StdioPipes) -> io::Result<(Sender, Receiver, Receiver)> {
    pipes.into_async_parts()
}
fn observe_stop(owner: &mut SupervisedProcess) -> Cleanup {
    match owner.stop(Duration::from_secs(3)) {
        Ok(report) => Cleanup::Observed(report),
        Err(error) => Cleanup::Unknown { kind: error.kind() },
    }
}
