use hagency_platform::{Launch, SupervisedProcess};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;

pub const MAX_GLOBAL_COMMANDS: usize = 16;
pub const MAX_PLUGIN_COMMANDS: usize = 2;
pub const MAX_LOG_BYTES: usize = 64 * 1024;
pub const MAX_LOG_RECORDS: usize = 200;
pub const STARTUP_EVENT_DEADLINE: Duration = Duration::from_secs(30);
pub const ACTION_TOOL_DEADLINE: Duration = Duration::from_secs(300);
pub const BUILD_DEADLINE: Duration = Duration::from_secs(1200);

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("plugin scheduler is busy")]
    Busy,
    #[error("plugin command failed to start: {0}")]
    Spawn(#[from] io::Error),
    #[error("plugin command timed out; cleanup is unproven")]
    OutcomeUnknown,
    #[error("plugin command exited with status {0}")]
    Exit(i32),
    #[error("plugin command output is not valid UTF-8")]
    InvalidOutput,
}

#[derive(Debug, Clone)]
pub struct CommandEnvironment {
    pub plugin_id: String,
    pub plugin_root: PathBuf,
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    pub entrypoint_id: String,
    pub path: Option<OsString>,
    pub workspace: Option<PathBuf>,
}
impl CommandEnvironment {
    pub fn build(&self) -> BTreeMap<OsString, OsString> {
        let mut env = BTreeMap::new();
        if let Some(path) = &self.path {
            env.insert(OsString::from("PATH"), path.clone());
        }
        env.insert(
            OsString::from("HAGENCY_PLUGIN_ID"),
            self.plugin_id.clone().into(),
        );
        env.insert(
            OsString::from("HAGENCY_PLUGIN_ROOT"),
            self.plugin_root.as_os_str().to_owned(),
        );
        env.insert(
            OsString::from("HAGENCY_PLUGIN_CONFIG_DIR"),
            self.config_dir.as_os_str().to_owned(),
        );
        env.insert(
            OsString::from("HAGENCY_PLUGIN_STATE_DIR"),
            self.state_dir.as_os_str().to_owned(),
        );
        env.insert(
            OsString::from("HAGENCY_PLUGIN_ENTRYPOINT_ID"),
            self.entrypoint_id.clone().into(),
        );
        env
    }
}

#[derive(Debug, Clone)]
pub struct CommandRequest {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub stdin: Vec<u8>,
    pub environment: CommandEnvironment,
    pub deadline: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub whole_tree_stopped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerSnapshot {
    pub global_active: usize,
    pub plugin_active: usize,
}

#[derive(Clone)]
pub struct Scheduler {
    state: Arc<(Mutex<SchedulerState>, Condvar)>,
}
#[derive(Debug, Default)]
struct SchedulerState {
    global_active: usize,
    plugin_active: BTreeMap<String, usize>,
}
pub struct Slot {
    state: Arc<(Mutex<SchedulerState>, Condvar)>,
    plugin_id: String,
}
impl Drop for Slot {
    fn drop(&mut self) {
        let (lock, wake) = &*self.state;
        if let Ok(mut state) = lock.lock() {
            state.global_active = state.global_active.saturating_sub(1);
            if let Some(active) = state.plugin_active.get_mut(&self.plugin_id) {
                *active = active.saturating_sub(1);
                if *active == 0 {
                    state.plugin_active.remove(&self.plugin_id);
                }
            }
            wake.notify_one();
        }
    }
}
impl Scheduler {
    pub fn new() -> Self {
        Self {
            state: Arc::new((Mutex::new(SchedulerState::default()), Condvar::new())),
        }
    }
    pub fn try_acquire(&self, plugin_id: &str) -> Result<Slot, CommandError> {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().map_err(|_| CommandError::Busy)?;
        let plugin_active = state.plugin_active.get(plugin_id).copied().unwrap_or(0);
        if state.global_active >= MAX_GLOBAL_COMMANDS || plugin_active >= MAX_PLUGIN_COMMANDS {
            return Err(CommandError::Busy);
        }
        state.global_active += 1;
        *state.plugin_active.entry(plugin_id.into()).or_default() += 1;
        Ok(Slot {
            state: Arc::clone(&self.state),
            plugin_id: plugin_id.into(),
        })
    }
    pub fn snapshot(&self, plugin_id: &str) -> SchedulerSnapshot {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("scheduler mutex poisoned");
        SchedulerSnapshot {
            global_active: state.global_active,
            plugin_active: state.plugin_active.get(plugin_id).copied().unwrap_or(0),
        }
    }
}
impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

pub fn run_command(request: CommandRequest) -> Result<CommandResult, CommandError> {
    if request.argv.is_empty()
        || request.argv.len() > 64
        || request.stdin.len() > 1024 * 1024
        || !request.cwd.is_dir()
    {
        return Err(CommandError::Spawn(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid plugin command request",
        )));
    }
    let mut command = Command::new(&request.argv[0]);
    command
        .args(&request.argv[1..])
        .current_dir(&request.cwd)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in request.environment.build() {
        command.env(key, value);
    }
    let mut child = command.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let input = request.stdin;
        thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let stdout = child.stdout.take().ok_or_else(|| {
        CommandError::Spawn(io::Error::new(io::ErrorKind::Other, "missing stdout"))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        CommandError::Spawn(io::Error::new(io::ErrorKind::Other, "missing stderr"))
    })?;
    let stdout_thread = thread::spawn(|| read_bounded(stdout));
    let stderr_thread = thread::spawn(|| read_bounded(stderr));
    let deadline = Instant::now() + request.deadline;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            return Err(CommandError::OutcomeUnknown);
        }
        thread::sleep(Duration::from_millis(5));
    };
    let stdout = stdout_thread
        .join()
        .map_err(|_| CommandError::InvalidOutput)?
        .map_err(|_| CommandError::InvalidOutput)?;
    let stderr = stderr_thread
        .join()
        .map_err(|_| CommandError::InvalidOutput)?
        .map_err(|_| CommandError::InvalidOutput)?;
    let status_code = status.code().unwrap_or(-1);
    if !status.success() {
        return Err(CommandError::Exit(status_code));
    }
    Ok(CommandResult {
        status: status_code,
        stdout,
        stderr,
        whole_tree_stopped: true,
    })
}

/// Run one plugin command under the host guardian. Service-owned execution
/// uses this entry point instead of the unowned compatibility helper above.
pub fn run_supervised_command(
    guardian: &Path,
    request: CommandRequest,
) -> Result<CommandResult, CommandError> {
    validate_request(&request)?;
    let launch = Launch {
        executable: PathBuf::from(&request.argv[0]),
        arguments: request.argv[1..].iter().map(OsString::from).collect(),
        directory: request.cwd.clone(),
        environment: request.environment.build(),
        require_crash_containment: false,
    };
    let (mut owner, pipes) =
        SupervisedProcess::spawn_piped(guardian, &launch).map_err(CommandError::Spawn)?;
    #[cfg(unix)]
    {
        use std::fs::File;
        let (stdin, stdout, stderr) = pipes.into_parts();
        let mut stdin = File::from(stdin);
        let stdout_thread = thread::spawn(|| read_bounded(File::from(stdout)));
        let stderr_thread = thread::spawn(|| read_bounded(File::from(stderr)));
        let input = request.stdin;
        let stdin_thread = thread::spawn(move || stdin.write_all(&input));
        let deadline = Instant::now() + request.deadline;
        let report = loop {
            if let Some(report) = owner
                .wait(Duration::from_millis(50))
                .map_err(CommandError::Spawn)?
            {
                break report;
            }
            if Instant::now() >= deadline {
                let _ = owner.stop(Duration::from_secs(5));
                let _ = stdin_thread.join();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(CommandError::OutcomeUnknown);
            }
        };
        let _ = stdin_thread.join();
        let stdout = stdout_thread
            .join()
            .map_err(|_| CommandError::InvalidOutput)?
            .map_err(|_| CommandError::InvalidOutput)?;
        let stderr = stderr_thread
            .join()
            .map_err(|_| CommandError::InvalidOutput)?
            .map_err(|_| CommandError::InvalidOutput)?;
        if !report.scope.whole_tree_stopped {
            return Err(CommandError::OutcomeUnknown);
        }
        return Ok(CommandResult {
            status: 0,
            stdout,
            stderr,
            whole_tree_stopped: true,
        });
    }
    #[cfg(windows)]
    {
        return run_supervised_windows(owner, pipes, request);
    }
}

fn validate_request(request: &CommandRequest) -> Result<(), CommandError> {
    if request.argv.is_empty()
        || request.argv.len() > 64
        || request.stdin.len() > 1024 * 1024
        || !request.cwd.is_dir()
    {
        return Err(CommandError::Spawn(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid plugin command request",
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn run_supervised_windows(
    owner: SupervisedProcess,
    pipes: hagency_platform::StdioPipes,
    request: CommandRequest,
) -> Result<CommandResult, CommandError> {
    thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| CommandError::Spawn(io::Error::other(error)))?
            .block_on(run_supervised_windows_async(owner, pipes, request))
    })
    .join()
    .map_err(|_| CommandError::InvalidOutput)?
}

#[cfg(windows)]
async fn run_supervised_windows_async(
    mut owner: SupervisedProcess,
    pipes: hagency_platform::StdioPipes,
    request: CommandRequest,
) -> Result<CommandResult, CommandError> {
    use tokio::io::AsyncWriteExt;
    let (mut stdin, stdout, stderr) = pipes.into_async_parts().map_err(CommandError::Spawn)?;
    let (report_tx, report_rx) =
        std::sync::mpsc::channel::<Result<hagency_platform::SupervisedReport, CommandError>>();
    let deadline = Instant::now() + request.deadline;
    thread::spawn(move || {
        loop {
            match owner.wait(Duration::from_millis(50)) {
                Ok(Some(report)) => {
                    let _ = report_tx.send(Ok(report));
                    break;
                }
                Ok(None) if Instant::now() < deadline => {}
                Ok(None) => {
                    let _ = owner.stop(Duration::from_secs(5));
                    let _ = report_tx.send(Err(CommandError::OutcomeUnknown));
                    break;
                }
                Err(error) => {
                    let _ = report_tx.send(Err(CommandError::Spawn(error)));
                    break;
                }
            }
        }
    });
    let input = request.stdin;
    let writer = async move {
        stdin
            .write_all(&input)
            .await
            .map_err(|_| CommandError::InvalidOutput)
    };
    let stdout_reader = read_bounded_async(stdout);
    let stderr_reader = read_bounded_async(stderr);
    let report_reader = tokio::task::spawn_blocking(move || report_rx.recv());
    let joined = tokio::time::timeout(request.deadline + Duration::from_secs(6), async {
        tokio::join!(writer, stdout_reader, stderr_reader, report_reader)
    })
    .await
    .map_err(|_| CommandError::OutcomeUnknown)?;
    joined.0?;
    let stdout = joined.1.map_err(|_| CommandError::InvalidOutput)??;
    let stderr = joined.2.map_err(|_| CommandError::InvalidOutput)??;
    let report = joined.3.map_err(|_| CommandError::InvalidOutput)??;
    if !report.scope.whole_tree_stopped {
        return Err(CommandError::OutcomeUnknown);
    }
    Ok(CommandResult {
        status: 0,
        stdout,
        stderr,
        whole_tree_stopped: true,
    })
}

#[cfg(windows)]
async fn read_bounded_async<R: tokio::io::AsyncRead + Unpin>(mut reader: R) -> io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let remaining = MAX_LOG_BYTES.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    Ok(output)
}
fn read_bounded<R: Read>(mut reader: R) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = MAX_LOG_BYTES.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    Ok(output)
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRecord {
    pub at_ms: u64,
    pub invocation_id: String,
    pub stream: String,
    pub text: String,
}

pub fn bounded_utf8(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_LOG_BYTES)]).into_owned()
}
