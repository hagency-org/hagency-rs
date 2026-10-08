//! `hagency setup`: prepare a state directory for an imported Palpo fleet
//! (ADR-187) without hand-writing `fleet-runtime.json`.
//!
//! It initializes the state directory when it is new, finds each coding
//! agent on this machine (Codex and its sign-in folder, Claude Code and its
//! folder, ADR-192), writes `fleet-runtime.json` (0600) and then validates it
//! with the same loader `serve` uses, so a file this command accepts is a
//! file the service accepts.
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
    /// Leave Codex out. Without it a Codex missing from `PATH` is left out
    /// only when Claude Code is configured instead.
    pub no_codex: bool,
    /// The Claude Code executable (ADR-192); found on `PATH` when absent.
    pub claude: Option<PathBuf>,
    /// Claude Code's own folder; `$CLAUDE_CONFIG_DIR` or `~/.claude` when
    /// absent.
    pub claude_config_dir: Option<PathBuf>,
    /// Leave Claude Code out.
    pub no_claude: bool,
    /// Replace an existing `fleet-runtime.json` (the old one is kept as a
    /// `.bak-<seconds>` copy).
    pub force: bool,
}

/// What was done, for the operator.
#[derive(Debug)]
pub struct Report {
    pub initialized: bool,
    /// The Codex executable, when Codex is configured.
    pub executable: Option<PathBuf>,
    /// The folder Codex signs in to, and whether a sign-in is present.
    pub codex_home: Option<PathBuf>,
    pub signed_in: bool,
    pub local_codex: bool,
    /// The Claude Code executable, when Claude Code is configured, and the
    /// folder holding its sign-in.
    pub claude: Option<PathBuf>,
    pub claude_folder: Option<PathBuf>,
    /// Why a Claude Code found on `PATH` was left out.
    pub claude_problem: Option<String>,
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

    // Each coding agent found is configured (ADR-192 decision 7). A path the
    // operator names must work. Claude Code merely found on `PATH` is left
    // out with its reason when it cannot be used; Codex merely missing from
    // `PATH` is left out only when Claude Code is configured instead.
    let mut claude_problem = None;
    let requested = options.claude.is_some() || options.claude_config_dir.is_some();
    let claude = if options.no_claude || (!requested && which("claude").is_none()) {
        None
    } else {
        // Claude Code keeps its sign-in itself; Hagency neither asks about
        // nor checks it (ADR-192 decision 6), and only names its folder.
        let found = claude_executable(options.claude.as_deref()).and_then(|executable| {
            Ok((
                executable,
                claude_folder(options.claude_config_dir.as_deref())?,
            ))
        });
        match found {
            Ok(found) => Some(found),
            Err(problem) if !requested => {
                claude_problem = Some(problem);
                None
            }
            Err(problem) => return Err(problem),
        }
    };
    let codex = if options.no_codex
        || (options.codex.is_none() && which("codex").is_none() && claude.is_some())
    {
        None
    } else {
        Some(codex_executable(options.codex.as_deref())?)
    };
    if codex.is_none() && claude.is_none() {
        return Err(claude_problem.unwrap_or_else(|| {
            "no coding agent to configure; install Codex or Claude Code".to_owned()
        }));
    }
    // The host's own Codex folder matters only to the local Codex binding;
    // without it, Codex signs in to `<state>/runtime-home` instead.
    let codex_home = match &codex {
        Some(_) if options.no_local_codex => Some(state.join("runtime-home")),
        Some(_) => Some(codex_home(options.codex_home.as_deref())?),
        None => None,
    };
    let signed_in = codex_home
        .as_ref()
        .is_some_and(|home| home.join("auth.json").is_file());

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
    let path = state.join(RUNTIME_FILE);
    // A rewrite (an agent updated or newly found, ADR-192 decision 7) replaces
    // only the coding-agent blocks: every other setting of the existing file,
    // the operator's own included, is kept as it was.
    let existing = options
        .force
        .then(|| std::fs::read(&path).ok())
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .filter(serde_json::Value::is_object);
    let mut document = match existing {
        Some(mut existing) => {
            if let Some(settings) = existing.as_object_mut() {
                for agent in ["executable", "executable_sha256", "local_codex", "claude"] {
                    settings.remove(agent);
                }
            }
            existing
        }
        None => serde_json::json!({
            "profile": "palpo_fleet_runtime_v1",
            "send_file": true,
            "receive_file": true,
            "file_limit": FILE_LIMIT,
            "operation_ms": OPERATION_MS,
            "response_ms": RESPONSE_MS,
            "approval_owner_wait_ms": APPROVAL_OWNER_WAIT_MS,
            "idle_ms": IDLE_MS,
            "home": {"root": homes, "task_client": task_client, "projects": []},
        }),
    };
    if let Some(executable) = &codex {
        document["executable"] = serde_json::json!(executable);
        document["executable_sha256"] = serde_json::json!(sha256_file(executable)?);
        if !options.no_local_codex {
            document["local_codex"] = serde_json::json!({
                "profile": "provider_owned_codex_v1",
                "preset": "local_codex",
                "seat": "local_codex_seat",
                "home": user_home()?,
                "codex_home": codex_home,
            });
        }
    }
    if let Some((executable, folder)) = &claude {
        document["claude"] = serde_json::json!({
            "executable": executable,
            "executable_sha256": sha256_file(executable)?,
            "local_claude": {
                "profile": "provider_owned_claude_v1",
                "preset": "local_claude",
                "seat": "local_claude_seat",
                "home": user_home()?,
                "config_dir": folder,
            },
        });
    }
    let bytes = serde_json::to_vec_pretty(&document).map_err(|e| e.to_string())?;

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
        local_codex: codex.is_some() && !options.no_local_codex,
        executable: codex,
        codex_home,
        signed_in,
        claude_folder: claude.as_ref().map(|(_, folder)| folder.clone()),
        claude: claude.map(|(executable, _)| executable),
        claude_problem,
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

/// Claude Code's own folder: as given, else `$CLAUDE_CONFIG_DIR`, else
/// `~/.claude`.
fn claude_folder(requested: Option<&Path>) -> Result<PathBuf, String> {
    let path = match requested {
        Some(path) => path.to_owned(),
        None => match std::env::var_os("CLAUDE_CONFIG_DIR") {
            Some(folder) => PathBuf::from(folder),
            None => user_home()?.join(".claude"),
        },
    };
    path.canonicalize().map_err(|_| {
        format!(
            "Claude Code folder {} not found; start Claude Code once on this machine first",
            path.display()
        )
    })
}

/// The Claude Code executable `serve` must run (ADR-192): an absolute,
/// canonical, native binary. The native installer's `claude` is a link to
/// one version's binary, which is what is pinned. A launcher script is
/// refused: its program would run under whatever interpreter `PATH` finds.
fn claude_executable(requested: Option<&Path>) -> Result<PathBuf, String> {
    let found = match requested {
        Some(path) => path.to_owned(),
        None => which("claude").ok_or(
            "Claude Code was not found on PATH; install it or pass --claude <path-to-binary>",
        )?,
    };
    let canonical = found
        .canonicalize()
        .map_err(|e| format!("{}: {e}", found.display()))?;
    if is_script(&canonical)? {
        return Err(format!(
            "{} is a launcher script; install the native Claude Code (`claude install`) or pass --claude <path-to-binary>",
            canonical.display()
        ));
    }
    Ok(canonical)
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
        assert!(runtime_matches(root.path(), Some(&binary), None));
        // A Codex update replaces the binary in place.
        std::fs::write(&binary, b"version two").unwrap();
        assert!(!runtime_matches(root.path(), Some(&binary), None));
        // A different binary path is stale too.
        assert!(!runtime_matches(
            root.path(),
            Some(&root.path().join("other")),
            None
        ));
    }

    /// ADR-192 decision 7: the runtime names exactly the coding agents found,
    /// each with its current digest; anything else is rewritten.
    #[test]
    fn the_runtime_names_exactly_the_agents_found() {
        let root = tempfile::tempdir().unwrap();
        let codex = root.path().join("codex");
        let claude = root.path().join("claude");
        std::fs::write(&codex, b"codex one").unwrap();
        std::fs::write(&claude, b"claude one").unwrap();
        let both = serde_json::json!({
            "executable": codex, "executable_sha256": sha256_file(&codex).unwrap(),
            "claude": {"executable": claude, "executable_sha256": sha256_file(&claude).unwrap()},
        });
        let write = |value: &serde_json::Value| {
            std::fs::write(root.path().join(RUNTIME_FILE), value.to_string()).unwrap()
        };
        write(&both);
        assert!(runtime_matches(root.path(), Some(&codex), Some(&claude)));
        // A Claude Code update replaces its binary.
        std::fs::write(&claude, b"claude two").unwrap();
        assert!(!runtime_matches(root.path(), Some(&codex), Some(&claude)));
        std::fs::write(&claude, b"claude one").unwrap();
        // An agent named but no longer found would be refused by serve.
        assert!(!runtime_matches(root.path(), Some(&codex), None));
        assert!(!runtime_matches(root.path(), None, Some(&claude)));
        // An agent found but not named is not configured yet.
        let mut codex_only = both.clone();
        codex_only.as_object_mut().unwrap().remove("claude");
        write(&codex_only);
        assert!(runtime_matches(root.path(), Some(&codex), None));
        assert!(!runtime_matches(root.path(), Some(&codex), Some(&claude)));
        // Claude Code alone.
        let mut claude_only = both.clone();
        let object = claude_only.as_object_mut().unwrap();
        object.remove("executable");
        object.remove("executable_sha256");
        write(&claude_only);
        assert!(runtime_matches(root.path(), None, Some(&claude)));
        assert!(!runtime_matches(root.path(), Some(&codex), Some(&claude)));
    }

    #[cfg(unix)]
    #[test]
    fn a_native_claude_is_pinned_through_its_link_and_a_script_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let versions = root.path().join("versions");
        std::fs::create_dir(&versions).unwrap();
        let native = versions.join("2.1.292");
        std::fs::write(&native, b"\xcf\xfa\xed\xfe native").unwrap();
        let link = root.path().join("claude");
        std::os::unix::fs::symlink(&native, &link).unwrap();
        // The installer's `claude` is a link; the version binary is pinned.
        assert_eq!(
            claude_executable(Some(&link)).unwrap(),
            native.canonicalize().unwrap()
        );
        let script = root.path().join("cli.js");
        std::fs::write(&script, "#!/usr/bin/env node\n").unwrap();
        let error = claude_executable(Some(&script)).unwrap_err();
        assert!(error.contains("launcher script"), "{error}");
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
/// only the agent's version flag and, for Codex, its own sign-in status
/// command; it never signs in and never reads credentials.
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
    /// The sign-in is assumed, not checked: Claude Code (ADR-192 decision 6).
    pub sign_in_assumed: bool,
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
        sign_in_assumed: false,
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

/// Detect Claude Code (ADR-192 decision 6): the binary `setup` would pin and
/// `claude --version`, nothing else. Its sign-in is assumed: Hagency neither
/// asks about it nor checks it, and a signed-out Claude Code shows up as
/// refused turns on its agent, not here.
pub async fn detect_claude() -> AgentStatus {
    let mut status = AgentStatus {
        kind: "claude",
        found: false,
        path: None,
        version: None,
        signed_in: false,
        sign_in_kind: None,
        sign_in_assumed: true,
        problem: None,
    };
    let executable = match claude_executable(None) {
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
    // Usable once its own folder exists; the sign-in in it is assumed.
    match claude_folder(None) {
        Ok(_) => status.signed_in = true,
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

/// Whether `<state>/fleet-runtime.json` names exactly the coding agents found
/// here, each binary with its current SHA-256 (ADR-192 decision 7). An
/// update changes a binary, and `serve` refuses a pinned digest that no
/// longer matches (`refused_config`); an agent found but not named is not
/// configured yet, and one named but gone would be refused too. The setup
/// page rewrites the file when this is false.
pub fn runtime_matches(state: &Path, codex: Option<&Path>, claude: Option<&Path>) -> bool {
    let Ok(bytes) = std::fs::read(state.join(RUNTIME_FILE)) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    let pinned = |block: &serde_json::Value, found: Option<&Path>| match (
        block["executable"].as_str(),
        found,
    ) {
        (None, None) => true,
        (Some(named), Some(found)) => {
            Some(named) == found.to_str()
                && sha256_file(found).ok().is_some_and(|digest| {
                    block["executable_sha256"].as_str() == Some(digest.as_str())
                })
        }
        _ => false,
    };
    pinned(&value, codex) && pinned(&value["claude"], claude)
}
