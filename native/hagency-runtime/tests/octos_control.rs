//! Octos approvals on the owned session (ADR-193 decision 4): one answer per
//! approval, with scope `request` only, its acknowledgement and decision echo
//! consumed, withdrawals, and the host's deny at the owner bound.
use hagency_platform::Launch;
use hagency_runtime::{
    octos::session::{
        ApprovalControlPolicy, ControlUpdate, Error, Event, Limits, PermissionDecision,
        Permissions, PreparedUpdate, QUIET_MS,
    },
    owned::{Cleanup, OwnedOctosSession},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const TURN: &str = "0192f0c1-0000-7000-8000-000000000001";
const SESSION: &str = "coding:local:hagency-dispatch-1";

fn binary() -> PathBuf {
    env!("CARGO_BIN_EXE_hagency-runtime-probe").into()
}
fn launch(root: &Path, mode: &str, marker: &Path) -> Launch {
    let mut environment = BTreeMap::new();
    environment.insert("PATH".into(), "".into());
    if let Some(root) = std::env::var_os("SystemRoot") {
        environment.insert("SystemRoot".into(), root);
    }
    Launch {
        executable: binary(),
        arguments: vec!["fake-octos".into(), mode.into(), marker.into()],
        directory: root.into(),
        environment,
        require_crash_containment: false,
    }
}
fn limits() -> Limits {
    Limits {
        write_timeout_ms: 1000,
        event_wait_ms: QUIET_MS + 3000,
        lifetime_ms: 20_000,
    }
}
fn policy(owner_wait_ms: u64) -> ApprovalControlPolicy {
    ApprovalControlPolicy {
        owner_wait_ms,
        response_reserve_ms: 1500,
    }
}
struct Run {
    _root: tempfile::TempDir,
    marker: PathBuf,
    runner: OwnedOctosSession,
}
async fn start(mode: &str, owner_wait_ms: u64) -> Run {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join(mode);
    let workspace = root.path().join("work");
    fs::create_dir(&workspace).unwrap();
    let mut runner =
        OwnedOctosSession::spawn(&binary(), &launch(root.path(), mode, &marker), limits()).unwrap();
    runner.hello().await.unwrap();
    runner
        .open(
            SESSION,
            "coding",
            workspace.to_str().unwrap(),
            Permissions::WorkspaceWrite,
        )
        .await
        .unwrap();
    runner
        .start_turn(TURN, "do the offline work")
        .await
        .unwrap();
    runner
        .enable_approval_control(policy(owner_wait_ms))
        .unwrap();
    Run {
        _root: root,
        marker,
        runner,
    }
}
impl Run {
    fn responses(&self) -> Vec<Value> {
        fs::read_to_string(self.marker.with_extension("responses"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    async fn approval(&mut self) -> String {
        match self.runner.next().await.unwrap() {
            Event::Approval {
                approval_id,
                turn_id,
                params,
            } => {
                assert_eq!(turn_id, TURN);
                assert_eq!(
                    params["typed_details"]["command"]["command_line"],
                    "rm -rf build"
                );
                approval_id
            }
            _ => panic!("the approval first"),
        }
    }
    async fn answer(&mut self, id: &str, decision: PermissionDecision) {
        let mut prepared = self.runner.prepare_approval(id, decision).unwrap();
        assert!(self.runner.prepared_admissible(&prepared));
        // Nothing arrives while the answer is written: Octos waits for it.
        match self
            .runner
            .send_prepared_approval(&mut prepared)
            .await
            .unwrap()
        {
            PreparedUpdate::WriteAccepted(write) => assert!(write.flushed),
            PreparedUpdate::Event(_) => panic!("no event before the answer leaves"),
        }
        assert!(!self.runner.prepared_admissible(&prepared));
    }
    /// The rest of the session: the decision's echo and Octos's answer are
    /// consumed, never events.
    async fn reply(&mut self) -> Option<String> {
        loop {
            match self.runner.next().await.unwrap() {
                Event::Idle(idle) => return idle.reply,
                Event::TurnEnded { .. } => {}
                _ => panic!("nothing between the answer and the reply"),
            }
        }
    }
}
fn stopped(cleanup: Cleanup) {
    assert!(matches!(cleanup, Cleanup::Observed(report) if report.scope.leader_exited));
}

#[tokio::test]
async fn native_octos_control_approve_once_reaches_octos() {
    let mut run = start("approval-answer", 8_000).await;
    let id = run.approval().await;
    run.answer(&id, PermissionDecision::Allow).await;
    assert_eq!(run.reply().await.as_deref(), Some("octos approved reply"));
    // Scope `request` only: Octos records no rule of its own.
    assert_eq!(
        run.responses(),
        [
            json!({"session_id":SESSION,"approval_id":"approval-1","decision":"approve",
            "approval_scope":"request"})
        ]
    );
    stopped(run.runner.stop());
}

#[tokio::test]
async fn native_octos_control_deny_lets_octos_continue() {
    let mut run = start("approval-answer", 8_000).await;
    let id = run.approval().await;
    run.answer(&id, PermissionDecision::Deny).await;
    assert_eq!(
        run.reply().await.as_deref(),
        Some("octos reply without the denied command")
    );
    assert_eq!(run.responses()[0]["decision"], "deny");
    stopped(run.runner.stop());
}

#[tokio::test]
async fn native_octos_control_withdrawal_is_a_settlement() {
    let mut run = start("approval-cancel", 8_000).await;
    let id = run.approval().await;
    match run.runner.next().await.unwrap() {
        Event::ApprovalSettled { approval_id } => assert_eq!(approval_id, id),
        _ => panic!("the withdrawal"),
    }
    // A withdrawn approval is never answered.
    assert!(matches!(
        run.runner.prepare_approval(&id, PermissionDecision::Allow),
        Err(Error::PermissionUnavailable)
    ));
    assert!(run.responses().is_empty());
    stopped(run.runner.stop());
}

#[tokio::test]
async fn native_octos_control_owner_bound_leaves_only_the_deny() {
    let mut run = start("approval-answer", 300).await;
    run.runner.enable_owner_wait_expiry().unwrap();
    let id = run.approval().await;
    // Not before the owner bound.
    assert!(matches!(
        run.runner.expire_approval(&id),
        Err(Error::Timeout)
    ));
    let wait = tokio::time::sleep(Duration::from_millis(400));
    tokio::pin!(wait);
    assert!(matches!(
        run.runner.next_or_control(wait.as_mut()).await.unwrap(),
        ControlUpdate::Control(())
    ));
    run.runner.expire_approval(&id).unwrap();
    run.answer(&id, PermissionDecision::Deny).await;
    assert_eq!(
        run.reply().await.as_deref(),
        Some("octos reply without the denied command")
    );
    assert_eq!(run.responses()[0]["decision"], "deny");
    stopped(run.runner.stop());
}
