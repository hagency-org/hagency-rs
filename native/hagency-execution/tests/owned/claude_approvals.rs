//! ADR-192 decision 4: a Claude Code permission request becomes the same
//! owner approval card as a Codex one, on the same store protocol.
use super::{approval_fixture::*, *};
use hagency_core::approvals::ApprovalChoice;

fn operation(
    f: &Fixture,
    mode: &str,
    policy: hagency_execution::ApprovalHost,
) -> (Operation, hagency_execution::ApprovalRequests) {
    let host = f.claude_host(mode).with_approvals(policy).unwrap();
    let mut operation = Operation::start(f.domain.clone(), f.cap.clone(), host, limits()).unwrap();
    let notices = operation.take_approval_requests().unwrap();
    (operation, notices)
}
/// Every answer the fake Claude read, in order.
fn answers(f: &Fixture) -> Vec<serde_json::Value> {
    fs::read_to_string(f.work.join("owned-dispatch.responses"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn input() -> serde_json::Value {
    json!({"command":"offline literal","description":"Offline step"})
}
fn allow(index: usize) -> serde_json::Value {
    json!({"type":"control_response","response":{"subtype":"success",
        "request_id":format!("owned-permission-{index}"),
        "response":{"behavior":"allow","updatedInput":input()}}})
}
fn deny() -> serde_json::Value {
    json!({"type":"control_response","response":{"subtype":"success",
        "request_id":"owned-permission-1",
        "response":{"behavior":"deny","message":"Permission denied by Hagency.","interrupt":true}}})
}
fn claude_resource() -> Resource {
    super::claude_pool()
}

/// The card names Claude, its tool and its exact input, offers the reusable
/// scope of that exact call, and Approve once answers with the original input
/// unchanged; the turn then completes with Claude's own reply.
#[tokio::test]
async fn native_claude_approval_once_allows_the_exact_input() {
    let f = Fixture::configured_resource(true, false, claude_resource());
    let (mut op, mut notices) = operation(&f, "task-approval", policy());
    let request = notice(&f, &mut op, &mut notices).await;
    assert_eq!(f.state(), "parked");
    let approval = f
        .domain
        .private_approval(request.request_id.clone())
        .await
        .unwrap();
    assert_eq!(approval.method, "claude/canUseTool");
    assert_eq!(approval.params["toolName"], "Bash");
    assert_eq!(approval.params["toolUseId"], "owned-tool-1");
    assert_eq!(approval.params["itemId"], "owned-permission-1");
    assert_eq!(approval.params["threadId"], "owned-claude");
    assert_eq!(approval.params["input"], input());
    assert!(approval.summary.reusable_scope);
    let card = f
        .domain
        .private_approval_card(request.request_id.clone(), request.owner_expires_at)
        .await
        .unwrap();
    let detail = &card.content()["com.agentchat.approval"];
    assert_eq!(detail["runtime"], "claude");
    assert_eq!(detail["tool_name"], "Bash");
    assert_eq!(
        detail["input_preview"],
        serde_json::to_string(&input()).unwrap()
    );
    assert_eq!(detail["reusable_scope"]["kind"], "exact_tool_call");
    let body = card.content()["body"].as_str().unwrap();
    assert!(body.contains("Runtime: claude\nTool: Bash\n"), "{body}");
    assert!(answers(&f).is_empty());

    choose(&f, &request.request_id, ApprovalChoice::Once).await;
    let report = op.wait().await.unwrap();
    assert_eq!(
        report.protocol,
        Protocol::Completed,
        "{:?} {:?}",
        report.failure,
        report.turn_failure
    );
    assert_eq!(report.text.as_deref(), Some("claude fixture reply"));
    assert_eq!(answers(&f), vec![allow(1)]);
    assert_eq!(
        f.count("SELECT COUNT(*) FROM approval_responses WHERE write_accepted=1"),
        1
    );
    assert_eq!(f.count("SELECT COUNT(*) FROM approval_grants"), 0);
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        assert_eq!(report.settlement, Settlement::Completed);
        assert_eq!(f.state(), "completed");
    }
    drop(report);
    f.domain.shutdown().await.unwrap();
}

/// Deny answers with the fixed deny and `interrupt` (ADR-156). Claude then
/// ends its turn in an error result, which settles like a failed turn.
#[tokio::test]
async fn native_claude_approval_deny_interrupts_the_turn() {
    let f = Fixture::configured_resource(true, false, claude_resource());
    let (mut op, mut notices) = operation(&f, "task-approval", policy());
    let request = notice(&f, &mut op, &mut notices).await;
    choose(&f, &request.request_id, ApprovalChoice::Deny).await;
    let report = op.wait().await.unwrap();
    assert_eq!(answers(&f), vec![deny()]);
    assert_eq!(report.protocol, Protocol::Failed);
    assert_eq!(report.turn_failure, "error_during_execution");
    assert_eq!(report.failure, Some(Failure::Protocol));
    assert_eq!(
        f.count("SELECT COUNT(*) FROM approval_responses WHERE write_accepted=1"),
        1
    );
    f.quarantined();
    drop(report);
    f.domain.shutdown().await.unwrap();
}

/// Allow for this task and Always allow are Hagency-side grants on the exact
/// tool call: the next identical request is answered by Hagency itself, with
/// no second card, and Claude never receives a permission update.
#[tokio::test]
async fn native_claude_approval_grant_answers_the_next_identical_request() {
    for choice in [ApprovalChoice::Task, ApprovalChoice::Always] {
        let f = Fixture::configured_resource(true, false, claude_resource());
        let (mut op, mut notices) = operation(&f, "task-approval-twice", policy());
        let request = notice(&f, &mut op, &mut notices).await;
        choose(&f, &request.request_id, choice).await;
        let report = op.wait().await.unwrap();
        assert_eq!(
            report.protocol,
            Protocol::Completed,
            "{choice:?}: {:?} {:?}",
            report.failure,
            report.turn_failure
        );
        assert_eq!(answers(&f), vec![allow(1), allow(2)], "{choice:?}");
        // One card only: the second request was decided by the grant.
        assert!(notices.recv().await.is_none(), "{choice:?}");
        assert_eq!(
            f.count("SELECT COUNT(*) FROM owner_approvals"),
            2,
            "{choice:?}"
        );
        assert_eq!(
            f.count("SELECT COUNT(*) FROM approval_grants WHERE scope_kind='\"exact_tool_call\"'"),
            1,
            "{choice:?}"
        );
        assert_eq!(
            f.count("SELECT COUNT(*) FROM approval_responses WHERE write_accepted=1"),
            2,
            "{choice:?}"
        );
        drop(report);
        f.domain.shutdown().await.unwrap();
    }
}

/// Nobody answers: at the owner bound the host records the deny and answers
/// it, as for Codex. The request is decided once and nothing is granted.
#[tokio::test]
async fn native_claude_approval_owner_wait_expiry_denies() {
    let f = Fixture::configured_resource(true, false, claude_resource());
    let (mut op, mut notices) = operation(
        &f,
        "task-approval",
        hagency_execution::ApprovalHost::new(4, 2, 1500, 8000).unwrap(),
    );
    let request = notice(&f, &mut op, &mut notices).await;
    let report = op.wait().await.unwrap();
    assert!(now() >= request.owner_expires_at);
    assert_eq!(answers(&f), vec![deny()]);
    let summary = f
        .domain
        .approval_summary(request.request_id.clone())
        .await
        .unwrap();
    assert_eq!(summary.choice, Some(ApprovalChoice::Deny));
    assert_eq!(f.count("SELECT COUNT(*) FROM approval_grants"), 0);
    assert_eq!(report.protocol, Protocol::Failed);
    drop(report);
    f.domain.shutdown().await.unwrap();
}

/// Claude withdraws its request before the owner answered: the callback is
/// cancelled (ADR-046, as for a Codex resolution before admission), nothing
/// is ever written to Claude, and the child is stopped.
#[tokio::test]
async fn native_claude_approval_withdrawn_before_an_answer_cancels() {
    let f = Fixture::configured_resource(true, false, claude_resource());
    let (mut op, _notices) = operation(&f, "task-approval-cancel", policy());
    let report = op.wait().await.unwrap();
    assert_eq!(report.failure, Some(Failure::ApprovalCancelled));
    assert!(f.work.join("owned-dispatch.cancelled").exists());
    assert!(answers(&f).is_empty());
    assert!(matches!(report.cleanup, Cleanup::Observed(_)));
    drop(report);
    f.domain.shutdown().await.unwrap();
}
