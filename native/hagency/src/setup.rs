//! `hagency setup`: prepare a state directory for an imported Palpo fleet
//! (ADR-187) without hand-writing `fleet-runtime.json`.
//!
//! It initializes the state directory when it is new, finds the Codex
//! executable and its sign-in folder, writes `fleet-runtime.json` (0600) and
//! then validates it with the same loader `serve` uses, so a file this
//! command accepts is a file the service accepts.
use crate::bootstrap::Failure;
use hagency_store::{DomainRepository, Repository, private};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
};

const RUNTIME_FILE: &str = "fleet-runtime.json";
/// Defaults that fit one operator's machine: a 4 MiB file limit, a 5-minute
/// tool operation, the 2 s response cap the host enforces, and a warm agent
/// kept for 20 minutes between turns.
const FILE_LIMIT: usize = 4 * 1024 * 1024;
const OPERATION_MS: u64 = 300_000;
const RESPONSE_MS: u64 = 2_000;
const APPROVAL_OWNER_WAIT_MS: u64 = 180_000;
const IDLE_MS: u64 = 1_200_000;

/// What the operator asked for.
pub struct Options {
    pub state_dir: PathBuf,
    pub listen: SocketAddr,
    /// The Codex executable; found on `PATH` when absent.
    pub codex: Option<PathBuf>,
    /// The folder holding the Codex sign-in; `$CODEX_HOME` or `~/.codex`
    /// when absent.
    pub codex_home: Option<PathBuf>,
    /// Use `<state>/runtime-home` instead of the host's own Codex sign-in.
    pub no_local_codex: bool,
    /// Replace an existing `fleet-runtime.json` (the old one is kept as a
    /// `.bak-<seconds>` copy).
    pub force: bool,
}

/// What was done, for the operator.
#[derive(Debug)]
pub struct Report {
    pub initialized: bool,
    pub executable: PathBuf,
    /// The folder Codex signs in to, and whether a sign-in is present.
    pub codex_home: PathBuf,
    pub signed_in: bool,
    pub local_codex: bool,
    pub runtime_file: PathBuf,
}

/// `hagency init`: a new or empty private directory, a fresh operator token
/// and both databases.
pub fn init_state(state_dir: &Path) -> Result<(), String> {
    private::directory(state_dir).map_err(|e| format!("state directory: {e}"))?;
    if std::fs::read_dir(state_dir)
        .map_err(|e| format!("state directory: {e}"))?
        .next()
        .is_some()
    {
        return Err("init requires empty state; no existing data will be imported".into());
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "secure randomness unavailable".to_owned())?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    private::write_new(&state_dir.join("operator.token"), token.as_bytes())
        .map_err(|e| format!("operator.token: {e}"))?;
    drop(Repository::open(state_dir).map_err(|e| format!("custody database: {e}"))?);
    drop(DomainRepository::open(state_dir).map_err(|e| format!("domain database: {e}"))?);
    Ok(())
}

pub fn run(options: &Options) -> Result<Report, String> {
    let state = &options.state_dir;
    let fresh = !state.exists()
        || std::fs::read_dir(state)
            .map_err(|e| format!("state directory: {e}"))?
            .next()
            .is_none();
    if fresh {
        init_state(state)?;
    } else if !state.join("operator.token").is_file() {
        return Err(format!(
            "{} is not empty and is not a Hagency state directory (no operator.token)",
            state.display()
        ));
    }
    let mut report = configure(options)?;
    report.initialized = fresh;
    Ok(report)
}

/// Write and validate `fleet-runtime.json` in an existing state directory
/// (the setup page's path, ADR-189; `run` adds initialization).
pub fn configure(options: &Options) -> Result<Report, String> {
    let fresh = false;
    let state = options
        .state_dir
        .canonicalize()
        .map_err(|e| format!("state directory: {e}"))?;

    let executable = codex_executable(options.codex.as_deref())?;
    let digest = sha256_file(&executable)?;
    // The host's own Codex folder matters only to the local Codex binding;
    // without it, Codex signs in to `<state>/runtime-home` instead.
    let codex_home = if options.no_local_codex {
        state.join("runtime-home")
    } else {
        codex_home(options.codex_home.as_deref())?
    };
    let signed_in = codex_home.join("auth.json").is_file();

    // The provisioning host refuses managed homes inside credential/SDK
    // custody. Keep the default specific to this state directory, but disjoint.
    let mut homes_name = state
        .file_name()
        .ok_or("state directory has no name")?
        .to_os_string();
    homes_name.push("-agent-homes");
    let homes = state.with_file_name(homes_name);
    private::directory(&homes).map_err(|e| format!("agent-homes: {e}"))?;
    let task_client = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|e| format!("running executable path: {e}"))?;
    let mut document = serde_json::json!({
        "profile": "palpo_fleet_runtime_v1",
        "executable": executable,
        "executable_sha256": digest,
        "send_file": true,
        "receive_file": true,
        "file_limit": FILE_LIMIT,
        "operation_ms": OPERATION_MS,
        "response_ms": RESPONSE_MS,
        "approval_owner_wait_ms": APPROVAL_OWNER_WAIT_MS,
        "idle_ms": IDLE_MS,
        "home": {"root": homes, "task_client": task_client, "projects": []},
    });
    if !options.no_local_codex {
        let home = user_home()?;
        document["local_codex"] = serde_json::json!({
            "profile": "provider_owned_codex_v1",
            "preset": "local_codex",
            "seat": "local_codex_seat",
            "home": home,
            "codex_home": codex_home,
        });
    }
    let bytes = serde_json::to_vec_pretty(&document).map_err(|e| e.to_string())?;

    let path = state.join(RUNTIME_FILE);
    if path.exists() {
        if !options.force {
            return Err(format!(
                "{} already exists; pass --force to replace it (the old file is kept as a backup)",
                path.display()
            ));
        }
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        std::fs::rename(&path, state.join(format!("{RUNTIME_FILE}.bak-{seconds}")))
            .map_err(|e| format!("could not keep the old {RUNTIME_FILE}: {e}"))?;
    }
    private::write_new(&path, &bytes).map_err(|e| format!("{RUNTIME_FILE}: {e}"))?;
    if let Err(failure) = crate::bootstrap::check_fleet_runtime(&state, options.listen) {
        // Never leave a file the service would refuse at its next start.
        let _ = std::fs::rename(&path, state.join(format!("{RUNTIME_FILE}.rejected")));
        return Err(match failure {
            Failure::Config { field, fix } => format!("{field}: {fix}"),
            other => other.to_string(),
        });
    }
    Ok(Report {
        initialized: fresh,
        executable,
        codex_home,
        signed_in,
        local_codex: !options.no_local_codex,
        runtime_file: path,
    })
}

fn user_home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_owned())?
        .canonicalize()
        .map_err(|e| format!("HOME: {e}"))
}

fn codex_home(requested: Option<&Path>) -> Result<PathBuf, String> {
    let path = match requested {
        Some(path) => path.to_owned(),
        None => match std::env::var_os("CODEX_HOME") {
            Some(home) => PathBuf::from(home),
            None => user_home()?.join(".codex"),
        },
    };
    path.canonicalize().map_err(|_| {
        format!(
            "Codex folder {} not found; run `codex login` first, or pass --codex-home",
            path.display()
        )
    })
}

/// The Codex executable `serve` must run: an absolute, canonical, regular
/// file. `codex` on `PATH` is often the npm launcher script, so a script is
/// resolved to the native binary the npm package ships beside it.
fn codex_executable(requested: Option<&Path>) -> Result<PathBuf, String> {
    let found = match requested {
        Some(path) => path.to_owned(),
        None => which("codex")
            .ok_or("codex was not found on PATH; install Codex or pass --codex <path-to-binary>")?,
    };
    let canonical = found
        .canonicalize()
        .map_err(|e| format!("{}: {e}", found.display()))?;
    if !is_script(&canonical)? {
        return Ok(canonical);
    }
    native_beside_script(&canonical).ok_or_else(|| {
        format!(
            "{} is a launcher script and no native Codex binary was found beside it; pass --codex <path-to-binary>",
            canonical.display()
        )
    })
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn is_script(path: &Path) -> Result<bool, String> {
    let mut head = [0u8; 2];
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let read = file
        .read(&mut head)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(read == 2 && &head == b"#!")
}

/// The npm package `@openai/codex` keeps its native binaries under
/// `node_modules/@openai/codex-<platform>/vendor/<target>/{bin,codex}/codex`.
fn native_beside_script(script: &Path) -> Option<PathBuf> {
    let package = script.ancestors().find(|dir| {
        dir.file_name().is_some_and(|name| name == "codex")
            && dir
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "@openai")
    })?;
    let mut roots = vec![package.join("node_modules").join("@openai")];
    roots.extend(package.parent().map(Path::to_owned));
    let mut candidates = Vec::new();
    for root in roots {
        let Ok(platforms) = std::fs::read_dir(&root) else {
            continue;
        };
        for platform in platforms.flatten() {
            if !platform.file_name().to_string_lossy().starts_with("codex-") {
                continue;
            }
            let Ok(targets) = std::fs::read_dir(platform.path().join("vendor")) else {
                continue;
            };
            for target in targets.flatten() {
                for sub in ["bin", "codex"] {
                    let binary = target.path().join(sub).join("codex");
                    if binary.is_file() && !is_script(&binary).unwrap_or(true) {
                        candidates.push(binary);
                    }
                }
            }
        }
    }
    candidates.sort();
    candidates.into_iter().next()?.canonicalize().ok()
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_launcher_script_resolves_to_the_native_binary() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("lib/node_modules/@openai/codex");
        std::fs::create_dir_all(package.join("bin")).unwrap();
        std::fs::write(package.join("bin/codex.js"), "#!/usr/bin/env node\n").unwrap();
        let vendor =
            package.join("node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin");
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::write(vendor.join("codex"), b"\x7fELF native").unwrap();
        let resolved = codex_executable(Some(&package.join("bin/codex.js"))).unwrap();
        assert_eq!(resolved, vendor.join("codex").canonicalize().unwrap());
    }

    #[test]
    fn a_native_binary_is_used_as_given() {
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("codex");
        std::fs::write(&binary, b"\xcf\xfa\xed\xfe native").unwrap();
        assert_eq!(
            codex_executable(Some(&binary)).unwrap(),
            binary.canonicalize().unwrap()
        );
    }

    #[test]
    fn a_changed_codex_binary_makes_the_runtime_stale() {
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("codex");
        std::fs::write(&binary, b"version one").unwrap();
        let digest = sha256_file(&binary).unwrap();
        std::fs::write(
            root.path().join(RUNTIME_FILE),
            serde_json::json!({"executable": binary, "executable_sha256": digest}).to_string(),
        )
        .unwrap();
        assert!(runtime_matches(root.path(), &binary));
        // A Codex update replaces the binary in place.
        std::fs::write(&binary, b"version two").unwrap();
        assert!(!runtime_matches(root.path(), &binary));
        // A different binary path is stale too.
        assert!(!runtime_matches(root.path(), &root.path().join("other")));
    }

    #[test]
    fn a_script_with_nothing_beside_it_is_refused_by_name() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("codex");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        let error = codex_executable(Some(&script)).unwrap_err();
        assert!(error.contains("launcher script"), "{error}");
    }
}

/// What the setup page shows for one coding agent (ADR-189). Hagency runs
/// only the agent's version flag and its own sign-in status command; it
/// never signs in and never reads credentials.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub kind: &'static str,
    pub found: bool,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub signed_in: bool,
    /// `chatgpt` or `api_key`, as the agent reports its sign-in.
    pub sign_in_kind: Option<&'static str>,
    /// Why the agent could not be used, in words for the page.
    pub problem: Option<String>,
}

/// Detect Codex: the binary `setup` would use, its version, and
/// `codex login status`.
pub async fn detect_codex() -> AgentStatus {
    let mut status = AgentStatus {
        kind: "codex",
        found: false,
        path: None,
        version: None,
        signed_in: false,
        sign_in_kind: None,
        problem: None,
    };
    let executable = match codex_executable(None) {
        Ok(path) => path,
        Err(problem) => {
            status.problem = Some(problem);
            return status;
        }
    };
    status.found = true;
    status.path = Some(executable.clone());
    status.version = run_agent(&executable, &["--version"])
        .await
        .ok()
        .map(|(_, out)| out.trim().to_owned())
        .filter(|v| !v.is_empty());
    match run_agent(&executable, &["login", "status"]).await {
        Ok((true, out)) => {
            status.signed_in = true;
            status.sign_in_kind = if out.contains("ChatGPT") {
                Some("chatgpt")
            } else if out.contains("API key") {
                Some("api_key")
            } else {
                None
            };
        }
        Ok((false, _)) => {}
        Err(problem) => status.problem = Some(problem),
    }
    status
}

/// Run a detected agent binary with fixed arguments, bounded in time and
/// output. Returns whether it exited successfully and its stdout.
async fn run_agent(executable: &Path, args: &[&str]) -> Result<(bool, String), String> {
    let child = tokio::process::Command::new(executable)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), child)
        .await
        .map_err(|_| format!("{} did not answer within 10 s", executable.display()))?
        .map_err(|e| format!("{}: {e}", executable.display()))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text.truncate(4096);
    Ok((output.status.success(), text))
}

/// Whether `<state>/fleet-runtime.json` still names this Codex binary with its
/// current SHA-256. A Codex update changes the binary, and `serve` refuses a
/// pinned digest that no longer matches (`refused_config`); the setup page
/// rewrites the file when this is false.
pub fn runtime_matches(state: &Path, executable: &Path) -> bool {
    let Ok(bytes) = std::fs::read(state.join(RUNTIME_FILE)) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    value["executable"].as_str() == executable.to_str()
        && sha256_file(executable)
            .ok()
            .is_some_and(|digest| value["executable_sha256"].as_str() == Some(digest.as_str()))
}
