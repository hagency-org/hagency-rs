//! A freshly built plugin package must be able to launch before it is copied
//! and registered: the build exit status is honoured, its output is kept in
//! the plugin logs and on disk, cargo is pointed at the package, and a
//! launcher that cannot find its binary fails the install.
use super::*;
use hagency_plugin::{Manifest, PluginError, PluginService};

fn manifest(build: &str, launcher: &str) -> String {
    format!(
        r#"schema = "hagency.plugin/v1"
id = "example.install"
name = "Install fixture"
version = "1.0.0"
min_hagency_version = "0.1.0"
platforms = ["linux-x86_64"]

[[build]]
command = ["sh", "-c", "{build}"]

[[runtimes]]
id = "fake"
framework = "fake"
command = ["bin/runtime"]
protocol = "hagency.runtime/v1"
readiness = ["bin/runtime", "--probe"]
{launcher}
"#
    )
}

/// A package whose launcher script has `body`; returns the service, the
/// parsed manifest and the package root.
fn package(
    build: &str,
    body: &str,
) -> (tempfile::TempDir, PluginService, Manifest, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let package = root.path().join("package");
    fs::create_dir_all(package.join("bin")).unwrap();
    let text = manifest(build, "");
    fs::write(package.join("hagency-plugin.toml"), &text).unwrap();
    let launcher = package.join("bin/runtime");
    fs::write(&launcher, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(
        &launcher,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let service = PluginService::open_with_guardian(&state, binary()).unwrap();
    let parsed = Manifest::parse(text.as_bytes()).unwrap();
    (root, service, parsed, package, state)
}

#[test]
fn build_environment_points_cargo_at_the_package_target_dir() {
    let (_root, service, manifest, package, _state) = package(
        "printf %s \\\"$CARGO_TARGET_DIR\\\" > cargo-target",
        "exit 0",
    );
    service.build_package(&manifest, &package).unwrap();
    assert_eq!(
        fs::read_to_string(package.join("cargo-target")).unwrap(),
        package.join("target").to_str().unwrap(),
        "the build writes inside the package, so the install copy carries it"
    );
}

#[test]
fn failed_build_fails_install_with_a_captured_log() {
    let (_root, service, manifest, package, state) =
        package("echo compiling; echo broken >&2; exit 3", "exit 0");
    let error = service.build_package(&manifest, &package).unwrap_err();
    let PluginError::BuildFailed(message) = error else {
        panic!("{error:?}");
    };
    assert!(message.contains("status 3"), "{message}");
    assert!(message.contains("broken"), "{message}");
    let logs = service.logs("example.install").unwrap();
    assert!(
        logs.iter()
            .any(|l| l.stream == "stdout" && l.text.contains("compiling"))
    );
    assert!(
        logs.iter()
            .any(|l| l.stream == "stderr" && l.text.contains("broken"))
    );
    let on_disk =
        fs::read_to_string(state.join("plugins/logs/example.install/build-0.log")).unwrap();
    assert!(
        on_disk.contains("exit status 3") && on_disk.contains("broken"),
        "{on_disk}"
    );
}

#[test]
fn install_proof_fails_a_launcher_that_exits_127() {
    let (_root, service, manifest, package, _state) = package(
        "echo built",
        "echo 'herdr-runtime is not built' >&2; exit 127",
    );
    let error = service.build_package(&manifest, &package).unwrap_err();
    let PluginError::BuildFailed(message) = error else {
        panic!("{error:?}");
    };
    assert!(
        message.contains("127") && message.contains("not built"),
        "{message}"
    );
}

#[test]
fn install_proof_accepts_a_launcher_that_runs() {
    let (_root, service, manifest, package, _state) =
        package("echo built", "cat >/dev/null; exit 0");
    service.build_package(&manifest, &package).unwrap();
    let logs = service.logs("example.install").unwrap();
    assert!(logs.iter().any(|l| l.text.contains("built")));
}
