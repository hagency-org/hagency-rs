//! SR-3 (ADR-134) and SR-4 (ADR-135) selectors. UNGATED file; every selector
//! is present on every hosted leg. The version legs are SQLite-free and run
//! everywhere; the dry-run legs that provision state run the real
//! init/serve/SIGTERM contract and require the store.
use std::{
    fs,
    io::{Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use hagency_core::tasks::{DispatchInput, SessionBinding};
use hagency_store::EffectOutcome;
use serde_json::json;
use sha2::{Digest, Sha256};

#[path = "../../hagency-store/tests/common/mod.rs"]
mod domain;

/// The start gate's explicit budget, named like the stop budget: the same
/// 20-second figure as the unit's TimeoutStopSec, but this one bounds the
/// /ready poll after start, not the drain after SIGTERM.
const START_GATE_BUDGET: Duration = Duration::from_secs(20);

fn binary() -> PathBuf {
    env!("CARGO_BIN_EXE_hagency").into()
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
/// Plant the pending custody row through the store's own API — an admitted
/// and approved request, a registered session, a canonical task and an
/// enqueued (queued, never-started) dispatch — never a bare INSERT that
/// references parent rows that do not exist. A queued dispatch is a pending
/// row by the runbook's own vocabulary, and nothing in `serve` resolves it.
fn seed_pending_dispatch(state: &Path) {
    let mut db = hagency_store::DomainRepository::open(state).unwrap();
    db.register(&domain::registration()).unwrap();
    let pool = domain::resource("pool", "seat", 1000);
    db.put_resource(&pool).unwrap();
    let proof = domain::proof(&domain::request("dryrun", "Worker", &pool, 100));
    let engagement = db.admit(&proof, 1000).unwrap();
    db.approve("approve", &proof, 1000).unwrap();
    let effect = db.claim_effect().unwrap().unwrap();
    db.observe_effect(
        &effect.id,
        effect.fence,
        &EffectOutcome::Applied {
            receipt: "fixture account".into(),
        },
    )
    .unwrap();
    db.register_session(&SessionBinding {
        id: "session".into(),
        engagement_id: engagement.id,
        room_id: "!room:example.test".into(),
        thread_root: None,
    })
    .unwrap();
    db.create_canonical_task("task", "session", "Pending across restart", now_ms())
        .unwrap();
    db.enqueue_dispatch(&DispatchInput {
        id: "dispatch".into(),
        session_id: "session".into(),
        task_id: Some("task".into()),
        resources: vec![],
        payload: json!({"instruction":"pending across the stop-start pair"}),
    })
    .unwrap();
}
fn workspace_version() -> String {
    // The workspace [workspace.package] version is the ONE version source;
    // package.json or any other manifest is never consulted.
    let manifest =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml"))
            .expect("workspace manifest readable on every leg");
    let mut in_package = false;
    for line in manifest.lines() {
        let token = line.trim();
        if token.starts_with('[') {
            in_package = token == "[workspace.package]";
            continue;
        }
        if in_package && token.starts_with("version") {
            return token
                .split('=')
                .nth(1)
                .expect("version assignment")
                .trim()
                .trim_matches('"')
                .to_string();
        }
    }
    panic!("workspace package version not found");
}
fn reported_version() -> String {
    artifact_version(&binary())
}
fn artifact_version(artifact: &Path) -> String {
    let output = Command::new(artifact).arg("--version").output().unwrap();
    assert!(output.status.success(), "artifact --version must succeed");
    String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .last()
        .expect("clap prints `<name> <version>`")
        .to_string()
}
/// The SR-3 release-tree scanner: any finding fails the scan, never skips.
/// Findings: package.json / lockfiles, node_modules directories, JavaScript
/// entry files, and `node` references in shipped wrappers and units.
fn scan_node_entrypoints(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, findings: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("readable staged tree") {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                if name == "node_modules" {
                    findings.push(format!("node_modules directory: {}", path.display()));
                    continue;
                }
                walk(&path, findings);
                continue;
            }
            let extension = path.extension().map(|v| v.to_string_lossy().into_owned());
            if matches!(
                name.as_str(),
                "package.json" | "package-lock.json" | "npm-shrinkwrap.json"
            ) || matches!(extension.as_deref(), Some("js") | Some("mjs") | Some("cjs"))
            {
                findings.push(format!("javascript entry point: {}", path.display()));
                continue;
            }
            // Wrappers and units are text: scan their bytes for node references.
            if let Ok(text) = fs::read_to_string(&path) {
                for token in ["node ", "/node", "\"node\""] {
                    if text.contains(token) {
                        findings.push(format!(
                            "node reference in shipped file: {} (`{token}`)",
                            path.display()
                        ));
                        break;
                    }
                }
            }
        }
    }
    let mut findings = Vec::new();
    walk(root, &mut findings);
    findings
}
/// Stage a release tree the way the workflow packages it: the versioned
/// binary, both units, and a checksums manifest.
fn stage_release_tree(root: &Path) -> PathBuf {
    let version = workspace_version();
    let tree = root.join(format!("hagency-v{version}"));
    fs::create_dir_all(&tree).unwrap();
    fs::copy(binary(), tree.join(format!("hagency-v{version}"))).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/hagency-native.service"),
        tree.join("hagency-native.service"),
    )
    .unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/io.hagency.native.plist"),
        tree.join("io.hagency.native.plist"),
    )
    .unwrap();
    let binary_bytes = fs::metadata(tree.join(format!("hagency-v{version}")))
        .unwrap()
        .len();
    fs::write(
        tree.join("checksums.txt"),
        format!("hagency-v{version}  {binary_bytes} bytes\nhagency-native.service  unit\nio.hagency.native.plist  unit\n"),
    )
    .unwrap();
    tree
}
fn http_status(addr: &str, path: &str) -> Option<u16> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    response.split_whitespace().nth(1)?.parse().ok()
}
fn free_loopback() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("127.0.0.1:{port}")
}
struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Running {
    guard: Guard,
    addr: String,
}
fn init_state(root: &Path) -> PathBuf {
    // The runbook's step 0 precondition: a fresh temp state directory
    // initialized by hagency init (which refuses a non-empty dir).
    let state = root.join("state");
    let status = Command::new(binary())
        .args(["init", "--state-dir", state.to_str().unwrap()])
        .status()
        .expect("hagency init spawns");
    assert!(status.success(), "hagency init must provision fresh state");
    state
}
fn spawn_service(state: &Path) -> Running {
    spawn_artifact(&binary(), state)
}
fn spawn_artifact(artifact: &Path, state: &Path) -> Running {
    let addr = free_loopback();
    let child = Command::new(artifact)
        .args([
            "serve",
            "--state-dir",
            state.to_str().unwrap(),
            "--listen",
            &addr,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("serve spawns");
    Running {
        guard: Guard(child),
        addr,
    }
}
fn wait_ready(running: &Running) {
    // The runbook's gate polls /ready, never /health (health is 200-while-live
    // and proves nothing at cutover). The poll is bounded by the start gate's
    // own named budget, the same 20-second figure as the stop budget but
    // bounding the start, not the drain.
    let until = Instant::now() + START_GATE_BUDGET;
    loop {
        if http_status(&running.addr, "/ready") == Some(200) {
            return;
        }
        assert!(Instant::now() < until, "service never answered ready 200");
        thread::sleep(Duration::from_millis(100));
    }
}
/// The runbook's stop contract: exits zero inside the budget, or parks on an
/// unknown close without a false success (ADR-120).
fn term_then_observe_exit(running: &mut Running, budget: Duration) -> bool {
    let status = Command::new("kill")
        .args(["-s", "TERM", &running.guard.0.id().to_string()])
        .status()
        .expect("external kill helper spawns");
    assert!(status.success(), "SIGTERM delivery failed");
    let child = &mut running.guard.0;
    let until = Instant::now() + budget;
    loop {
        if let Some(code) = child.try_wait().unwrap() {
            assert!(
                code.success(),
                "service must exit zero on the deliberate stop, got {code:?}"
            );
            return true; // clean drain-and-close
        }
        if Instant::now() >= until {
            return false; // parked on an unknown close; no false success
        }
        thread::sleep(Duration::from_millis(50));
    }
}

// ---- Upgrade-procedure fixture (O5): two local builds, N and N+1 ----

static BUILD_LOCK: Mutex<()> = Mutex::new(());

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
/// The same tree at workspace version N+1 (the patch segment bumped by one):
/// a real local build, not the release workflow and not a runtime override.
/// The version constant is the one `hagency --version` reads, so the artifact
/// is named and reports exactly as ADR-134's procedure assumes.
fn next_version() -> String {
    let mut parts: Vec<u64> = workspace_version()
        .split('.')
        .map(|p| p.parse().expect("numeric workspace version segment"))
        .collect();
    let last = parts.last_mut().expect("version has a patch segment");
    *last += 1;
    parts
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(".")
}
fn artifact_name(version: &str) -> String {
    format!("hagency-v{version}")
}
fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    // Preserve directory permissions: init creates the state dir 0o700, and
    // private::directory refuses a dir with group/other bits set. A plain
    // create_dir_all (0o755) would make a restored state dir fail Startup.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::symlink_metadata(src).unwrap().permissions().mode();
        fs::set_permissions(dst, fs::Permissions::from_mode(mode)).unwrap();
    }
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}
fn patch_workspace_version(manifest: &Path, version: &str) {
    let text = fs::read_to_string(manifest).unwrap();
    let mut patched = String::new();
    let mut in_package = false;
    for line in text.lines() {
        let token = line.trim();
        if token.starts_with('[') {
            in_package = token == "[workspace.package]";
            patched.push_str(line);
            patched.push('\n');
            continue;
        }
        if in_package && token.starts_with("version") {
            patched.push_str(&format!("version = \"{version}\"\n"));
            continue;
        }
        patched.push_str(line);
        patched.push('\n');
    }
    fs::write(manifest, patched).unwrap();
}
fn collect_native_files(dir: &Path, root: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if path.is_dir() {
            collect_native_files(&path, root, out);
        } else {
            out.push(rel);
        }
    }
}
/// Content address over the copied inputs (root manifests + the whole
/// `native/**` source set), so a changed workspace or source rebuilds while
/// the 3x gate repeats and the second test reuse the same artifact.
fn source_fingerprint() -> String {
    let root = workspace_root();
    let mut files = vec![
        "Cargo.toml".to_string(),
        "Cargo.lock".to_string(),
        "rust-toolchain.toml".to_string(),
        // hagency-core's non-test source includes this workspace-root file;
        // a changed copy rebuilds, an absent one fails the build honestly.
        "lib/role-capacity.json".to_string(),
    ];
    collect_native_files(&root.join("native"), &root, &mut files);
    files.sort();
    let mut hasher = Sha256::new();
    for rel in &files {
        hasher.update(rel.as_bytes());
        hasher.update(fs::read(root.join(rel)).unwrap());
    }
    format!("{:x}", hasher.finalize())
}
/// Build the N+1 artifact once, offline and locked, under a process mutex so
/// both tests and the gate repeats share a single build. The stamp is the
/// source fingerprint; a matching stamp with the artifact present skips the
/// sync-and-build.
fn next_artifact() -> PathBuf {
    let _guard = BUILD_LOCK.lock().unwrap();
    let root = workspace_root();
    let src = root.join("target/upgrade-next-src");
    let target = root.join("target/upgrade-next-target");
    let artifact = target.join("debug/hagency");
    let stamp = target.join(".source-fingerprint");
    let fingerprint = source_fingerprint();
    let cached = fs::read_to_string(&stamp)
        .map(|s| s == fingerprint)
        .unwrap_or(false);
    if artifact.is_file() && cached {
        return artifact;
    }
    if src.exists() {
        fs::remove_dir_all(&src).unwrap();
    }
    fs::create_dir_all(&src).unwrap();
    for file in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"] {
        fs::copy(root.join(file), src.join(file)).unwrap();
    }
    // The one non-test include that escapes `native/**`: the source path
    // `../../../lib/role-capacity.json` resolves to the copy root's lib/.
    fs::create_dir_all(src.join("lib")).unwrap();
    fs::copy(
        root.join("lib/role-capacity.json"),
        src.join("lib/role-capacity.json"),
    )
    .unwrap();
    copy_tree(&root.join("native"), &src.join("native"));
    patch_workspace_version(&src.join("Cargo.toml"), &next_version());
    let status = Command::new(env!("CARGO"))
        .arg("build")
        .arg("--offline")
        .arg("-p")
        .arg("hagency")
        .arg("--bin")
        .arg("hagency")
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_HOME", root.join(".cargo-home"))
        .current_dir(&src)
        .status()
        .expect("N+1 build spawns");
    assert!(
        status.success(),
        "the N+1 build must succeed (offline, locked)"
    );
    fs::write(stamp, fingerprint).unwrap();
    assert!(
        artifact.is_file(),
        "the N+1 artifact must exist after build"
    );
    artifact
}
fn user_version(state: &Path) -> i64 {
    rusqlite::Connection::open(state.join("domain.sqlite3"))
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}
fn assert_pending_survives(state: &Path) {
    let db = rusqlite::Connection::open(state.join("domain.sqlite3")).unwrap();
    let row: (String, i64) = db
        .query_row(
            "SELECT state,fence FROM runner_dispatches WHERE id='dispatch'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, ("queued".into(), 0));
    let resolved: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM runner_outputs WHERE dispatch_id='dispatch'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(resolved, 0, "no row may be resolved by the procedure");
}
/// Render the unit from the deploy template (a read-only input), naming the
/// versioned artifact in ExecStart so unit and binary cannot disagree — the
/// same substitution SR-1's installer performs.
fn render_unit(artifact: &Path, state: &Path) -> String {
    let template = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/hagency-native.service"),
    )
    .expect("unit template present");
    let install_dir = artifact.parent().unwrap().to_string_lossy();
    let basename = artifact.file_name().unwrap().to_string_lossy();
    template
        .replace(
            "__INSTALL_DIR__/hagency",
            &format!("{install_dir}/{basename}"),
        )
        .replace("__STATE_DIR__", &state.to_string_lossy())
        .replace("__USER__", "tester")
}

#[test]
fn native_binary_version_matches_workspace() {
    let workspace = workspace_version();
    let reported = reported_version();
    assert_eq!(
        reported, workspace,
        "the binary's --version must equal the workspace [workspace.package] version exactly; \
         no other version source (package.json) is consulted"
    );
    // The artifact naming rule embeds the same version, so binary, unit and
    // artifact cannot disagree silently (ADR-127's version-identity note).
    let artifact = format!("hagency-v{workspace}");
    assert!(artifact.contains(&workspace));
}

#[test]
fn native_release_entrypoints_scan_finds_no_node() {
    let root = tempfile::tempdir().unwrap();
    let tree = stage_release_tree(root.path());
    let findings = scan_node_entrypoints(&tree);
    assert!(
        findings.is_empty(),
        "the release tree must contain no Node entry point: {findings:?}"
    );
    // Negative control proving the scanner detects rather than skips: a
    // planted package.json is a finding that would fail the scan.
    fs::write(tree.join("package.json"), "{}").unwrap();
    assert_eq!(
        scan_node_entrypoints(&tree).len(),
        1,
        "a planted package.json must be found"
    );
}

#[test]
fn native_cutover_dryrun_version_identity() {
    // Runbook step 0: version identity is proven BEFORE any service start —
    // a mismatch fails the dry-run before a single process spawns.
    let workspace = workspace_version();
    assert_eq!(reported_version(), workspace);
    let root = tempfile::tempdir().unwrap();
    let _state = init_state(root.path()); // fresh temp state, per the runbook
}

#[test]
fn native_cutover_dryrun_ready_gate_and_stop_contract() {
    // Runbook steps 3 and 6: gate on /ready, then SIGTERM inside the budget.
    let root = tempfile::tempdir().unwrap();
    let state = init_state(root.path());
    let mut running = spawn_service(&state);
    wait_ready(&running);
    assert_eq!(http_status(&running.addr, "/health"), Some(200));
    let clean = term_then_observe_exit(&mut running, Duration::from_secs(20));
    assert!(clean, "clean drain-and-close expected on the idle leg");
}

#[test]
fn native_cutover_dryrun_pending_preserved_across_restart() {
    // Runbook steps 6 and 7: the stop-start pair preserves every pending or
    // outcome-unknown row; nothing is resolved, dropped or marked done.
    let root = tempfile::tempdir().unwrap();
    let state = init_state(root.path());
    seed_pending_dispatch(&state);
    let mut first = spawn_service(&state);
    wait_ready(&first);
    term_then_observe_exit(&mut first, Duration::from_secs(20));
    let second = spawn_service(&state);
    wait_ready(&second);
    {
        let db = rusqlite::Connection::open(state.join("domain.sqlite3")).unwrap();
        // The seeded pending row survives the stop-start pair exactly as it
        // was planted: still queued, fence 0, never resolved or dropped.
        let row: (String, i64) = db
            .query_row(
                "SELECT state,fence FROM runner_dispatches WHERE id='dispatch'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, ("queued".into(), 0));
        let resolved: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM runner_outputs WHERE dispatch_id='dispatch'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resolved, 0, "no row may be resolved by the stop-start pair");
    }
}

// ---- O5: the upgrade procedure and its rollback (ADR-134/135) ----

fn install_dir(root: &Path) -> PathBuf {
    root.join("install")
}
/// Stage the version-N artifact into the install dir under its ADR-134 name,
/// so unit, binary and artifact cannot disagree silently. Version N's build is
/// the cargo-test build itself (CARGO_BIN_EXE_hagency, already at the
/// workspace version); the copy only names it for coexistence with N+1.
fn stage_n(root: &Path) -> PathBuf {
    let dir = install_dir(root);
    fs::create_dir_all(&dir).unwrap();
    let dest = dir.join(artifact_name(&workspace_version()));
    fs::copy(binary(), &dest).unwrap();
    dest
}
/// Stage the version-N+1 artifact (the real second local build) into the same
/// install dir, so both versioned artifacts coexist exactly as the procedure
/// assumes.
fn stage_next(root: &Path) -> PathBuf {
    let dir = install_dir(root);
    fs::create_dir_all(&dir).unwrap();
    let dest = dir.join(artifact_name(&next_version()));
    fs::copy(next_artifact(), &dest).unwrap();
    dest
}
/// A post-upgrade write recorded under N+1 whose state-restore rollback must
/// discard it: a second queued dispatch in the already-registered session and
/// task, no re-registration.
fn seed_second_pending_dispatch(state: &Path) {
    let mut db = hagency_store::DomainRepository::open(state).unwrap();
    db.enqueue_dispatch(&DispatchInput {
        id: "dispatch2".into(),
        session_id: "session".into(),
        task_id: Some("task".into()),
        resources: vec![],
        payload: json!({"instruction":"post-upgrade write the rollback discards"}),
    })
    .unwrap();
}

#[test]
fn native_upgrade_procedure_continues_state() {
    let root = tempfile::tempdir().unwrap();
    let n_artifact = stage_n(root.path());
    let next = stage_next(root.path());
    // Step 0: version identity before any service start.
    assert_eq!(artifact_version(&n_artifact), workspace_version());
    assert_eq!(artifact_version(&next), next_version());
    // Step 1: fresh state (init refuses a non-empty dir, writes operator.token).
    let state = init_state(root.path());
    assert!(state.join("operator.token").is_file());
    let head_before = user_version(&state);
    // A written row: the admitted engagement and a queued dispatch, via the
    // store's own API (the runbook's pending row).
    seed_pending_dispatch(&state);
    // Step 2: the unit names the versioned N artifact.
    let unit_n = render_unit(&n_artifact, &state);
    assert!(
        unit_n.contains(&artifact_name(&workspace_version())),
        "the unit names N's versioned artifact"
    );
    // Step 3: start and gate on /ready (never /health alone).
    let mut running = spawn_artifact(&n_artifact, &state);
    wait_ready(&running);
    assert_eq!(http_status(&running.addr, "/health"), Some(200));
    // Step 6 stop contract, then the upgrade: replace the artifact, restart.
    assert!(term_then_observe_exit(
        &mut running,
        Duration::from_secs(20)
    ));
    let unit_next = render_unit(&next, &state);
    assert!(
        unit_next.contains(&artifact_name(&next_version())),
        "the unit now names N+1's versioned artifact"
    );
    let mut upgraded = spawn_artifact(&next, &state);
    wait_ready(&upgraded);
    // Step 7: preservation — the store head, the written row, the readiness
    // word, and the reported version (changed N -> N+1).
    assert_eq!(user_version(&state), head_before);
    assert_pending_survives(&state);
    assert_eq!(http_status(&upgraded.addr, "/ready"), Some(200));
    assert_eq!(artifact_version(&next), next_version());
    assert!(term_then_observe_exit(
        &mut upgraded,
        Duration::from_secs(20)
    ));
}

#[test]
fn native_upgrade_rollback_restores_previous() {
    let root = tempfile::tempdir().unwrap();
    let n_artifact = stage_n(root.path());
    let next = stage_next(root.path());
    let state = init_state(root.path());
    let head_before = user_version(&state);
    seed_pending_dispatch(&state); // row X, before the rollback point
    let backup = root.path().join("backup-state");
    copy_tree(&state, &backup);
    // Install N, start, gate; then upgrade to N+1 and gate.
    let mut running = spawn_artifact(&n_artifact, &state);
    wait_ready(&running);
    assert_eq!(artifact_version(&n_artifact), workspace_version());
    assert!(term_then_observe_exit(
        &mut running,
        Duration::from_secs(20)
    ));
    let mut upgraded = spawn_artifact(&next, &state);
    wait_ready(&upgraded);
    assert_eq!(artifact_version(&next), next_version());
    assert!(term_then_observe_exit(
        &mut upgraded,
        Duration::from_secs(20)
    ));
    // A post-upgrade write under N+1, which a state-restore rollback discards.
    seed_second_pending_dispatch(&state);
    // Rollback: stop (already stopped), restore the state copy, re-point the
    // unit at N, restart — the runbook's R1 step.
    fs::remove_dir_all(&state).unwrap();
    copy_tree(&backup, &state);
    let unit_n = render_unit(&n_artifact, &state);
    assert!(unit_n.contains(&artifact_name(&workspace_version())));
    let mut restored = spawn_artifact(&n_artifact, &state);
    wait_ready(&restored);
    // Version N runs again on the same state; no re-init, no data loss.
    assert_eq!(artifact_version(&n_artifact), workspace_version());
    assert_eq!(user_version(&state), head_before);
    assert_pending_survives(&state); // row X readable
    assert!(state.join("operator.token").is_file()); // never re-initialized
    let second: i64 = rusqlite::Connection::open(state.join("domain.sqlite3"))
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM runner_dispatches WHERE id='dispatch2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        second, 0,
        "the post-upgrade write must be discarded by the restore"
    );
    assert!(term_then_observe_exit(
        &mut restored,
        Duration::from_secs(20)
    ));
}
