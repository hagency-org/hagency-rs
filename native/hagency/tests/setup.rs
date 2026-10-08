//! `hagency setup`: a fresh state directory comes out with a
//! `fleet-runtime.json` that `serve`'s own loader accepts.
#![cfg(unix)]
use std::{os::unix::fs::PermissionsExt, path::Path, process::Command};

fn run(home: &Path, args: &[&str]) -> std::process::Output {
    // Hermetic: no coding agent of the machine running the tests is found.
    let empty = home.parent().unwrap().join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    run_with_path(home, &empty, args)
}
fn run_with_path(home: &Path, path: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_hagency"))
        .arg("setup")
        .args(args)
        .env("HOME", home)
        .env("PATH", path)
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .output()
        .unwrap()
}

/// A native-looking Claude Code binary and its own folder under the fake
/// home (ADR-192). The folder holds no sign-in: Setup never looks.
fn claude(root: &Path, home: &Path) -> std::path::PathBuf {
    let folder = home.join(".claude");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let binary = root.join("claude-bin").join("claude");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"\xcf\xfa\xed\xfe fake native claude").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    binary
}
fn written(state: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(state.join("fleet-runtime.json")).unwrap()).unwrap()
}

/// A native-looking Codex binary and a signed-in Codex folder under a fake
/// home, both private as the local Codex binding requires.
fn codex(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let home = root.join("home");
    let codex_home = home.join(".codex");
    std::fs::create_dir_all(&codex_home).unwrap();
    for dir in [&home, &codex_home] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(codex_home.join("auth.json"), "{}").unwrap();
    let binary = root.join("bin").join("codex");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"\xcf\xfa\xed\xfe fake native codex").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    (home, binary)
}

#[test]
fn native_setup_writes_a_runtime_serve_accepts() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (home, binary) = codex(&root);
    let state = root.join("state");
    let codex_arg = binary.to_str().unwrap();
    let state_arg = state.to_str().unwrap();

    let first = run(&home, &["--state-dir", state_arg, "--codex", codex_arg]);
    assert!(
        first.status.success(),
        "setup must succeed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let stdout = String::from_utf8_lossy(&first.stdout);
    assert!(stdout.contains("Initialized"), "{stdout}");
    assert!(stdout.contains("Wrote and validated"), "{stdout}");
    assert!(state.join("operator.token").is_file());
    let file = state.join("fleet-runtime.json");
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(written["profile"], "palpo_fleet_runtime_v1");
    assert_eq!(written["executable"], codex_arg);
    assert_eq!(written["local_codex"]["seat"], "local_codex_seat");
    assert_eq!(
        written["local_codex"]["codex_home"],
        home.join(".codex").to_str().unwrap()
    );

    // Loading JSON alone does not construct the provisioning host. Exercise
    // its actual custody boundary: managed homes must be outside service state.
    let homes = hagency_store::agent_home::ManagedHomePlan::new(
        written["home"]["root"].as_str().unwrap().into(),
        Vec::new(),
        written["home"]["task_client"].as_str().unwrap().into(),
    )
    .unwrap();
    assert!(
        homes.separate_from(&state).is_ok(),
        "setup must keep managed agent homes disjoint from credential/SDK state"
    );

    // A second run never replaces the file silently.
    let again = run(&home, &["--state-dir", state_arg, "--codex", codex_arg]);
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("--force"));

    // --force replaces it and keeps the old one.
    let forced = run(
        &home,
        &["--state-dir", state_arg, "--codex", codex_arg, "--force"],
    );
    assert!(
        forced.status.success(),
        "{}",
        String::from_utf8_lossy(&forced.stderr)
    );
    let backups = std::fs::read_dir(&state)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("fleet-runtime.json.bak-")
        })
        .count();
    assert_eq!(backups, 1);
}

#[test]
fn native_setup_refuses_a_directory_that_is_not_hagency_state() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (home, binary) = codex(&root);
    let state = root.join("other");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("unrelated.txt"), "x").unwrap();
    let output = run(
        &home,
        &[
            "--state-dir",
            state.to_str().unwrap(),
            "--codex",
            binary.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("operator.token"));
    assert!(!state.join("fleet-runtime.json").exists());
}

#[test]
fn native_setup_follows_the_npm_launcher_to_the_native_binary() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (home, _) = codex(&root);
    let package = root.join("lib/node_modules/@openai/codex");
    std::fs::create_dir_all(package.join("bin")).unwrap();
    std::fs::write(package.join("bin/codex.js"), "#!/usr/bin/env node\n").unwrap();
    let native =
        package.join("node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin");
    std::fs::create_dir_all(&native).unwrap();
    std::fs::write(native.join("codex"), b"\xcf\xfa\xed\xfe native").unwrap();
    let state = root.join("state");
    let output = run(
        &home,
        &[
            "--state-dir",
            state.to_str().unwrap(),
            "--codex",
            package.join("bin/codex.js").to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(state.join("fleet-runtime.json")).unwrap()).unwrap();
    assert_eq!(
        written["executable"],
        native.join("codex").to_str().unwrap()
    );
}

#[test]
fn native_setup_without_local_codex_needs_no_codex_folder() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (home, binary) = codex(&root);
    std::fs::remove_dir_all(home.join(".codex")).unwrap();
    let state = root.join("state");
    let output = run(
        &home,
        &[
            "--state-dir",
            state.to_str().unwrap(),
            "--codex",
            binary.to_str().unwrap(),
            "--no-local-codex",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(state.join("runtime-home").to_str().unwrap()),
        "{stdout}"
    );
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(state.join("fleet-runtime.json")).unwrap()).unwrap();
    assert!(written.get("local_codex").is_none());
}

/// ADR-189: a build without the embedded console refuses `start` by name and
/// leaves no half-made state behind.
#[test]
fn native_start_without_a_console_refuses_before_touching_state() {
    if option_env!("HAGENCY_CONSOLE_DIR").is_some() {
        return; // a release-style build carries the console
    }
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let output = Command::new(env!("CARGO_BIN_EXE_hagency"))
        .args(["start", "--no-open", "--state-dir", state.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--console-assets"));
    assert!(!state.exists());
}

/// ADR-192: a machine with Claude Code only gets a runtime with a Claude block
/// and its local binding, no Codex at all, and `serve`'s loader accepts it.
#[test]
fn native_setup_writes_a_claude_runtime_serve_accepts() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (home, _) = codex(&root);
    let binary = claude(&root, &home);
    let state = root.join("state");
    let output = run(
        &home,
        &[
            "--state-dir",
            state.to_str().unwrap(),
            "--no-codex",
            "--claude",
            binary.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Claude Code:"), "{stdout}");
    assert!(stdout.contains("uses its own sign-in"), "{stdout}");
    assert!(!stdout.contains("Codex:"), "{stdout}");
    let written = written(&state);
    assert!(written.get("executable").is_none());
    assert!(written.get("local_codex").is_none());
    assert_eq!(written["claude"]["executable"], binary.to_str().unwrap());
    let local = &written["claude"]["local_claude"];
    assert_eq!(local["profile"], "provider_owned_claude_v1");
    assert_eq!(local["preset"], "local_claude");
    assert_eq!(local["seat"], "local_claude_seat");
    assert_eq!(local["home"], home.to_str().unwrap());
    assert_eq!(local["config_dir"], home.join(".claude").to_str().unwrap());
}

/// ADR-192 operator decision 3: both coding agents side by side.
#[test]
fn native_setup_writes_codex_and_claude_together() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (home, codex_binary) = codex(&root);
    let claude_binary = claude(&root, &home);
    let state = root.join("state");
    let output = run(
        &home,
        &[
            "--state-dir",
            state.to_str().unwrap(),
            "--codex",
            codex_binary.to_str().unwrap(),
            "--claude",
            claude_binary.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let written = written(&state);
    assert_eq!(written["executable"], codex_binary.to_str().unwrap());
    assert_eq!(written["local_codex"]["seat"], "local_codex_seat");
    assert_eq!(
        written["claude"]["executable"],
        claude_binary.to_str().unwrap()
    );
    assert_eq!(
        written["claude"]["local_claude"]["seat"],
        "local_claude_seat"
    );
}

/// A Claude Code the operator names must be a native binary: a launcher
/// script's program would run under whatever interpreter PATH finds.
#[test]
fn native_setup_refuses_a_named_claude_launcher_script() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (home, _) = codex(&root);
    claude(&root, &home);
    let script = root.join("claude.js");
    std::fs::write(&script, "#!/usr/bin/env node\n").unwrap();
    let state = root.join("state");
    let output = run(
        &home,
        &[
            "--state-dir",
            state.to_str().unwrap(),
            "--no-codex",
            "--claude",
            script.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("launcher script"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!state.join("fleet-runtime.json").exists());
}

/// A Claude Code merely found on PATH that cannot be used never blocks Codex:
/// it is left out, and setup says why.
#[test]
fn native_setup_leaves_out_an_unusable_claude_found_on_path() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (home, codex_binary) = codex(&root);
    let path = root.join("path-with-script");
    std::fs::create_dir_all(&path).unwrap();
    let script = path.join("claude");
    std::fs::write(&script, "#!/usr/bin/env node\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let state = root.join("state");
    let output = run_with_path(
        &home,
        &path,
        &[
            "--state-dir",
            state.to_str().unwrap(),
            "--codex",
            codex_binary.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Claude Code left out"), "{stdout}");
    assert!(written(&state).get("claude").is_none());
}

/// ADR-192 decision 7: when Setup rewrites the runtime for a changed or newly
/// found agent, only the coding-agent blocks change. Every other setting of
/// the existing file, the operator's own included, is kept as it was.
#[test]
fn native_setup_rewrite_keeps_the_operators_other_settings() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (home, codex_binary) = codex(&root);
    let state = root.join("state");
    let first = run(
        &home,
        &[
            "--state-dir",
            state.to_str().unwrap(),
            "--codex",
            codex_binary.to_str().unwrap(),
        ],
    );
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    // The operator tunes the file by hand.
    let mut tuned = written(&state);
    tuned["matrix_sdk_timeout_ms"] = serde_json::json!(60000);
    tuned["operation_ms"] = serde_json::json!(420000);
    let path = state.join("fleet-runtime.json");
    std::fs::remove_file(&path).unwrap();
    hagency_store::private::write_new(&path, &serde_json::to_vec_pretty(&tuned).unwrap()).unwrap();
    // Claude Code is installed later; Setup rewrites the runtime.
    let claude_binary = claude(&root, &home);
    let rewrite = run(
        &home,
        &[
            "--state-dir",
            state.to_str().unwrap(),
            "--codex",
            codex_binary.to_str().unwrap(),
            "--claude",
            claude_binary.to_str().unwrap(),
            "--force",
        ],
    );
    assert!(
        rewrite.status.success(),
        "{}",
        String::from_utf8_lossy(&rewrite.stderr)
    );
    let after = written(&state);
    assert_eq!(after["matrix_sdk_timeout_ms"], 60000);
    assert_eq!(after["operation_ms"], 420000);
    assert_eq!(after["home"], tuned["home"]);
    assert_eq!(after["executable"], codex_binary.to_str().unwrap());
    assert_eq!(
        after["claude"]["executable"],
        claude_binary.to_str().unwrap()
    );
}
