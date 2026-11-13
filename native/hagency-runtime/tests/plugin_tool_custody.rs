//! `tool.v1` custody against the real guardian: one approved call envelope
//! launches at most once (also across a service restart), and a denial, a
//! stale digest or a revoked plugin creates no process. The tool appends one
//! line to a marker file per launch, so launches are counted, not inferred.
use hagency_plugin::{ApprovalDecision, PluginError, PluginService};
use serde_json::json;
use std::{fs, path::Path};
use tempfile::{TempDir, tempdir};

fn guardian() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_hagency-runtime-probe").into()
}

/// A linked, enabled package whose `read` tool appends one line to `marker`
/// per launch and answers a valid tool result.
fn marker_tool(marker: &Path, state: &Path) -> (TempDir, PluginService, String) {
    let dir = tempdir().expect("package");
    let manifest = r#"
schema = "hagency.plugin/v1"
id = "example.plugin"
name = "Example"
version = "1.0.0"
min_hagency_version = "0.1.0"
platforms = ["linux-x86_64"]

[[tools]]
id = "read"
title = "Read"
description = "bounded read"
input_schema = { type = "object" }
command = ["sh", "-c", '''echo run >> "MARKER"; printf '%s' '{"content":[{"type":"text","text":"ok"}],"isError":false}' ''']
"#
    .replace("MARKER", marker.to_str().unwrap());
    fs::write(dir.path().join("hagency-plugin.toml"), manifest).expect("manifest");
    let manager = PluginService::open_with_guardian(state, guardian()).expect("manager");
    let record = manager.link(dir.path()).expect("link");
    manager.enable(&record.id).expect("enable");
    (dir, manager, record.id)
}
fn launches(marker: &Path) -> usize {
    fs::read_to_string(marker).map_or(0, |text| text.lines().count())
}

/// A denial (and an expired owner wait, which the approval coordinator
/// answers as a denial), a stale package digest, and a revoked (disabled)
/// plugin create no process and no marker. A denied call stays denied: an
/// approval replayed under the same call ID does not launch it. The control
/// at the end proves the same tool does launch when approved.
#[test]
fn native_plugin_tool_denial_and_timeout_do_not_launch() {
    let scratch = tempdir().expect("scratch");
    let marker = scratch.path().join("marker");
    let (_package, manager, id) = marker_tool(&marker, &scratch.path().join("state"));
    let input = json!({"path": "opaque"});
    let approval = manager
        .prepare_tool_approval("plugin", &id, "read", &input)
        .expect("approval");

    let denied = manager
        .invoke_approved_tool(&approval, &input, ApprovalDecision::Deny, "denied-call")
        .expect("denial");
    assert!(denied.is_error);
    let replayed = manager
        .invoke_approved_tool(
            &approval,
            &input,
            ApprovalDecision::ApproveOnce,
            "denied-call",
        )
        .expect("retained denial");
    assert!(replayed.is_error, "a denied call is never launched later");

    let mut stale = approval.clone();
    stale.package_sha256 = "0".repeat(64);
    assert!(matches!(
        manager.invoke_approved_tool(&stale, &input, ApprovalDecision::ApproveOnce, "stale-call"),
        Err(PluginError::Untrusted)
    ));
    assert_eq!(launches(&marker), 0, "no plugin process ran");
    assert_eq!(
        manager
            .scheduler_snapshot(&id)
            .expect("scheduler")
            .global_active,
        0
    );

    // Control: the same tool, approved under a fresh call ID, does launch.
    manager
        .invoke_approved_tool(&approval, &input, ApprovalDecision::ApproveOnce, "control")
        .expect("approved control run");
    assert_eq!(launches(&marker), 1);

    manager.disable(&id).expect("revoke");
    assert!(
        manager
            .invoke_approved_tool(
                &approval,
                &input,
                ApprovalDecision::ApproveOnce,
                "revoked-call"
            )
            .is_err()
    );
    assert_eq!(launches(&marker), 1, "a revoked plugin does not launch");
}

/// One approved call envelope runs once: a replay, also after the service
/// reopens, returns the retained result without a launch, and one custody
/// record holds it. A different call ID is a different call.
#[test]
fn native_plugin_tool_approved_replay_launches_once() {
    let scratch = tempdir().expect("scratch");
    let marker = scratch.path().join("marker");
    let state = scratch.path().join("state");
    let (_package, manager, id) = marker_tool(&marker, &state);
    let input = json!({"path": "opaque"});
    let approval = manager
        .prepare_tool_approval("plugin", &id, "read", &input)
        .expect("approval");

    let first = manager
        .invoke_approved_tool(&approval, &input, ApprovalDecision::ApproveOnce, "call-a")
        .expect("first run");
    assert!(!first.is_error);
    assert_eq!(launches(&marker), 1);
    let replay = manager
        .invoke_approved_tool(&approval, &input, ApprovalDecision::ApproveOnce, "call-a")
        .expect("replay");
    assert_eq!(replay, first);
    assert_eq!(launches(&marker), 1, "a replay never launches");

    drop(manager);
    let reopened = PluginService::open_with_guardian(&state, guardian()).expect("reopen");
    let after_restart = reopened
        .invoke_approved_tool(&approval, &input, ApprovalDecision::ApproveOnce, "call-a")
        .expect("replay after reopen");
    assert_eq!(after_restart, first);
    assert_eq!(launches(&marker), 1, "custody survives a restart");

    let custody: serde_json::Value = serde_json::from_slice(
        &fs::read(state.join("plugins/tool-calls.json")).expect("custody file"),
    )
    .expect("custody json");
    let records: Vec<_> = custody
        .as_object()
        .expect("custody map")
        .values()
        .filter(|call| call["callId"] == "call-a")
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["state"], "succeeded");

    reopened
        .invoke_approved_tool(&approval, &input, ApprovalDecision::ApproveOnce, "call-b")
        .expect("a new call");
    assert_eq!(launches(&marker), 2);
}

/// An `action.v1` invocation runs under the real guardian, settles durably,
/// and a submission with the same idempotency key after the service reopens
/// returns the same invocation in the same terminal state without a rerun.
#[test]
fn native_plugin_invocation_replays_after_service_reopen() {
    let scratch = tempdir().expect("scratch");
    let marker = scratch.path().join("marker");
    let state = scratch.path().join("state");
    let package = tempdir().expect("package");
    let manifest = r#"
schema = "hagency.plugin/v1"
id = "example.plugin"
name = "Example"
version = "1.0.0"
min_hagency_version = "0.1.0"
platforms = ["linux-x86_64"]

[[actions]]
id = "ping"
title = "Ping"
command = ["sh", "-c", '''echo run >> "MARKER"; printf '{"ok":true}' ''']
"#
    .replace("MARKER", marker.to_str().unwrap());
    fs::write(package.path().join("hagency-plugin.toml"), manifest).expect("manifest");
    let manager = PluginService::open_with_guardian(&state, guardian()).expect("manager");
    let record = manager.link(package.path()).expect("link");
    manager.enable(&record.id).expect("enable");
    let submitted = manager
        .submit_action(&record.id, "ping", json!({"ok": true}), "replay-key")
        .expect("submit");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let terminal = loop {
        let current = manager.invocation(&submitted.invocation_id).expect("poll");
        if matches!(
            current.state.as_str(),
            "succeeded" | "failed" | "outcome_unknown"
        ) {
            break current;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "invocation did not settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(terminal.state, "succeeded", "{terminal:?}");
    assert_eq!(terminal.result, Some(json!({"ok": true})));
    assert_eq!(launches(&marker), 1);
    drop(manager);
    let reopened = PluginService::open_with_guardian(&state, guardian()).expect("reopen");
    let replay = reopened
        .submit_action(&record.id, "ping", json!({"ok": true}), "replay-key")
        .expect("replay");
    assert_eq!(replay.invocation_id, submitted.invocation_id);
    assert_eq!(replay.state, "succeeded");
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(launches(&marker), 1, "a replayed key never reruns");
}
