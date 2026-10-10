//! ADR-193 decision 4: an Octos approval becomes the same owner approval card
//! as a Codex or Claude Code one, on the same store protocol. Octos is
//! answered with scope `request` only and never records a rule of its own.
use super::{approval_fixture::*, octos::octos_pool, *};
use hagency_core::approvals::ApprovalChoice;

fn operation(
    f: &Fixture,
    mode: &str,
    policy: hagency_execution::ApprovalHost,
) -> (Operation, hagency_execution::ApprovalRequests) {
    let host = f.octos_host(mode).with_approvals(policy).unwrap();
    let mut operation = Operation::start(f.domain.clone(), f.cap.clone(), host, limits()).unwrap();
    let notices = operation.take_approval_requests().unwrap();
    (operation, notices)
}
/// Every `approval/respond` the fake Octos read, in order.
fn answers(f: &Fixture) -> Vec<serde_json::Value> {
    fs::read_to_string(f.work.join("owned-dispatch.responses"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn session(f: &Fixture) -> String {
    let requests = fs::read_to_string(f.work.join("owned-dispatch.requests")).unwrap();
    let open: serde_json::Value = serde_json::from_str(requests.lines().nth(2).unwrap()).unwrap();
    open["params"]["session_id"].as_str().unwrap().to_owned()
}
fn answer(f: &Fixture, index: usize, decision: &str) -> serde_json::Value {
    json!({"session_id":session(f),"approval_id":format!("approval-{index}"),
        "decision":decision,"approval_scope":"request"})
}

/// The card names Octos, its tool and what Octos says the command does, and
/// offers the reusable scope of that exact command in that directory, as for
/// a Codex command. Approve once answers that one approval; the dispatch then
/// completes with Octos's own reply once Octos is idle.
#[tokio::test]
async fn native_octos_approval_once_approves_that_request() {
    let f = Fixture::configured_resource(true, false, octos_pool());
    let (mut op, mut notices) = operation(&f, "approval-answer", policy());
    let request = notice(&f, &mut op, &mut notices).await;
    assert_eq!(f.state(), "parked");
    let approval = f
        .domain
        .private_approval(request.request_id.clone())
        .await
        .unwrap();
    let work = f.work.to_str().unwrap();
    assert_eq!(approval.method, "octos/approval");
    assert_eq!(approval.params["toolName"], "shell");
    assert_eq!(approval.params["itemId"], "approval-1");
    assert_eq!(approval.params["command"], "rm -rf build");
    assert_eq!(approval.params["cwd"], work);
    assert_eq!(approval.params["title"], "Run a command");
    assert!(hagency_runtime::octos::uuid(
        approval.params["octosTurnId"].as_str().unwrap()
    ));
    assert_eq!(approval.params["turnId"], approval.params["octosTurnId"]);
    assert!(approval.summary.reusable_scope);
    let card = f
        .domain
        .private_approval_card(request.request_id.clone(), request.owner_expires_at)
        .await
        .unwrap();
    let detail = &card.content()["com.agentchat.approval"];
    assert_eq!(detail["runtime"], "octos");
    assert_eq!(detail["tool_name"], "shell");
    assert_eq!(
        detail["input_preview"],
        serde_json::to_string(&json!({"title":"Run a command","body":"rm -rf build",
            "command":"rm -rf build","cwd":work}))
        .unwrap()
    );
    assert_eq!(detail["reusable_scope"]["kind"], "exact_command");
    let body = card.content()["body"].as_str().unwrap();
    assert!(body.contains("Runtime: octos\nTool: shell\n"), "{body}");
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
    assert_eq!(report.text.as_deref(), Some("octos approved reply"));
    assert_eq!(answers(&f), vec![answer(&f, 1, "approve")]);
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

/// Deny answers that approval with a deny: Octos continues without the
/// command and goes idle with its own reply, which settles like any completed
/// dispatch, as for a Codex decline.
#[tokio::test]
async fn native_octos_approval_deny_lets_octos_continue() {
    let f = Fixture::configured_resource(true, false, octos_pool());
    let (mut op, mut notices) = operation(&f, "approval-answer", policy());
    let request = notice(&f, &mut op, &mut notices).await;
    choose(&f, &request.request_id, ApprovalChoice::Deny).await;
    let report = op.wait().await.unwrap();
    assert_eq!(answers(&f), vec![answer(&f, 1, "deny")]);
    assert_eq!(
        report.protocol,
        Protocol::Completed,
        "{:?} {:?}",
        report.failure,
        report.turn_failure
    );
    assert_eq!(
        report.text.as_deref(),
        Some("octos reply without the denied command")
    );
    assert_eq!(report.failure, None);
    assert_eq!(
        f.count("SELECT COUNT(*) FROM approval_responses WHERE write_accepted=1"),
        1
    );
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        assert_eq!(report.settlement, Settlement::Completed);
        assert_eq!(f.state(), "completed");
    }
    drop(report);
    f.domain.shutdown().await.unwrap();
}

/// Allow for this task and Always allow are Hagency-side grants on the exact
/// command and directory: the next identical approval is answered by Hagency
/// itself, with no second card, and Octos still gets scope `request` only.
#[tokio::test]
async fn native_octos_approval_grant_answers_the_next_identical_request() {
    for choice in [ApprovalChoice::Task, ApprovalChoice::Always] {
        let f = Fixture::configured_resource(true, false, octos_pool());
        let (mut op, mut notices) = operation(&f, "approval-twice", policy());
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
        assert_eq!(report.text.as_deref(), Some("octos approved reply"));
        assert_eq!(
            answers(&f),
            vec![answer(&f, 1, "approve"), answer(&f, 2, "approve")],
            "{choice:?}"
        );
        // One card only: the second approval was decided by the grant.
        assert!(notices.recv().await.is_none(), "{choice:?}");
        assert_eq!(
            f.count("SELECT COUNT(*) FROM owner_approvals"),
            2,
            "{choice:?}"
        );
        assert_eq!(
            f.count("SELECT COUNT(*) FROM approval_grants WHERE scope_kind='\"exact_command\"'"),
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
/// it, as for Codex. The approval is decided once, nothing is granted, and
/// Octos goes on to its own reply.
#[tokio::test]
async fn native_octos_approval_owner_wait_expiry_denies() {
    let f = Fixture::configured_resource(true, false, octos_pool());
    let (mut op, mut notices) = operation(
        &f,
        "approval-answer",
        hagency_execution::ApprovalHost::new(4, 2, 1500, 8000).unwrap(),
    );
    let request = notice(&f, &mut op, &mut notices).await;
    let report = op.wait().await.unwrap();
    assert!(now() >= request.owner_expires_at);
    assert_eq!(answers(&f), vec![answer(&f, 1, "deny")]);
    let summary = f
        .domain
        .approval_summary(request.request_id.clone())
        .await
        .unwrap();
    assert_eq!(summary.choice, Some(ApprovalChoice::Deny));
    assert_eq!(f.count("SELECT COUNT(*) FROM approval_grants"), 0);
    assert_eq!(report.protocol, Protocol::Completed);
    assert_eq!(
        report.text.as_deref(),
        Some("octos reply without the denied command")
    );
    drop(report);
    f.domain.shutdown().await.unwrap();
}

/// Octos settles its approval itself before the owner answered: the callback
/// is cancelled (ADR-046, as for a Codex resolution before admission),
/// nothing is ever written to Octos, and the child is stopped.
#[tokio::test]
async fn native_octos_approval_withdrawn_before_an_answer_cancels() {
    let f = Fixture::configured_resource(true, false, octos_pool());
    let (mut op, _notices) = operation(&f, "approval-cancel", policy());
    let report = op.wait().await.unwrap();
    assert_eq!(report.failure, Some(Failure::ApprovalCancelled));
    assert!(f.work.join("owned-dispatch.cancelled").exists());
    assert!(answers(&f).is_empty());
    assert!(matches!(report.cleanup, Cleanup::Observed(_)));
    drop(report);
    f.domain.shutdown().await.unwrap();
}
