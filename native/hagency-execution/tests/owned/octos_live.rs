//! ADR-193: an operator-run live check of Octos on the product path: the
//! Octos Host, the fixed launch profile with Hagency's own settings, owner
//! cards through the store and the usage ledger, against the operator's real
//! Octos and one of their own profiles. Ignored: it spends a little of that
//! profile's model quota. Run it with
//!
//! ```text
//! HAGENCY_OCTOS_QUALIFY_BIN=<canonical path of octos> HAGENCY_OCTOS_QUALIFY_PROFILE=dev \
//!   cargo test -p hagency-execution --test owned \
//!   native_octos_live_check -- --ignored --exact --nocapture
//! ```
use super::{approval_fixture::*, *};
use hagency_core::approvals::ApprovalChoice;
use hagency_execution::ApprovalHost;
use serde_json::Value;
use std::path::Path;

const BIN_ENV: &str = "HAGENCY_OCTOS_QUALIFY_BIN";
const PROFILE_ENV: &str = "HAGENCY_OCTOS_QUALIFY_PROFILE";

struct Scenario {
    name: &'static str,
    instruction: &'static str,
    choice: ApprovalChoice,
}

/// One dispatch on a fresh store, played to Octos's idle: every owner card is
/// answered with the scenario's choice. Returns the facts the verdict reads.
async fn run(octos: &Path, profile: &str, scenario: &Scenario) -> Value {
    let pool: Resource = serde_json::from_value(json!({
        "presetId":"pool","seatId":"seat","framework":"octos","model":"glm-5.3-flash",
        "provider":"zai-coding","octosProfile":profile,
        "ceiling":{"tokens":10_000_000,"period":"monthly"}
    }))
    .unwrap();
    let f = Fixture::configured_payload(
        true,
        false,
        pool,
        json!({"instruction": scenario.instruction}),
    );
    // The directory the command removes, inside the workspace only.
    fs::create_dir(f.work.join("build")).unwrap();
    fs::write(f.work.join("build/keep.txt"), "build output").unwrap();
    // Octos finds the user's profiles under HOME; it gets no other variable
    // that could carry a key.
    let mut environment = BTreeMap::new();
    for key in ["HOME", "USER", "PATH", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            environment.insert(key.into(), value);
        }
    }
    let host = Host::new(
        binary(),
        octos.to_owned(),
        environment,
        BTreeMap::from([("work".into(), f.work.clone())]),
    )
    .unwrap()
    .with_octos_runner(f.root.path().join("octos"))
    .unwrap()
    // Its task tools are served by the offline helper (ADR-193 decision 5).
    .with_task_helper(binary(), "127.0.0.1:13300".parse().unwrap())
    .unwrap()
    .with_approvals(ApprovalHost::new(4, 2, 120_000, 10_000).unwrap())
    .unwrap();
    let limits = Limits {
        operation_ms: 300_000,
        response_ms: 2_000,
    };
    let started = std::time::Instant::now();
    let mut op = Operation::start(f.domain.clone(), f.cap.clone(), host, limits).unwrap();
    let mut notices = op.take_approval_requests().unwrap();
    let mut asked = Vec::new();
    // The notice channel closes when the operation ends.
    while let Some(notice) = notices.recv().await {
        let approval = f
            .domain
            .private_approval(notice.request_id.clone())
            .await
            .unwrap();
        let card = f
            .domain
            .private_approval_card(notice.request_id.clone(), notice.owner_expires_at)
            .await
            .unwrap();
        let detail = &card.content()["com.agentchat.approval"];
        asked.push(json!({
            "method": approval.method,
            "tool": approval.params["toolName"],
            "command": approval.params["command"],
            "body": approval.params["body"],
            "reusable_scope": approval.summary.reusable_scope,
            "card_runtime": detail["runtime"],
            "card_tool": detail["tool_name"],
            "card_actions": detail["actions"].as_array().map(|actions| {
                actions.iter().map(|a| a["id"].clone()).collect::<Vec<_>>()
            }),
        }));
        choose(&f, &notice.request_id, scenario.choice).await;
    }
    let report = op.wait().await.unwrap();
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let leader_exited =
        matches!(report.cleanup, Cleanup::Observed(cleanup) if cleanup.scope.leader_exited);
    let usage = report.usage_status();
    let facts = json!({
        "protocol": format!("{:?}", report.protocol),
        "failure": report.failure.as_ref().map(|failure| format!("{failure:?}")),
        "turn_failure": report.turn_failure,
        "settlement": format!("{:?}", report.settlement),
        "reply": report.text.as_deref().map(|text| text.chars().take(400).collect::<String>()),
        "approvals": asked,
        "answers_recorded": f.count("SELECT COUNT(*) FROM approval_responses WHERE write_accepted=1"),
        "build_exists": f.work.join("build").exists(),
        "usage_attached": usage.attached,
        "usage_failure": usage.failure.map(|failure| format!("{failure:?}")),
        "usage_sources": f.count("SELECT COUNT(*) FROM usage_sources WHERE framework='octos'"),
        "usage_receipts": f.count("SELECT COUNT(*) FROM usage_receipts"),
        "leader_exited": leader_exited,
        "elapsed_ms": elapsed_ms,
    });
    drop(report);
    f.domain.shutdown().await.unwrap();
    facts
}

/// What must hold for the scenario to pass on this path.
fn verdict(name: &str, facts: &Value) -> bool {
    let completed = facts["protocol"] == "Completed"
        && facts["failure"].is_null()
        && facts["reply"]
            .as_str()
            .is_some_and(|reply| !reply.trim().is_empty());
    let carded = facts["approvals"].as_array().is_some_and(|asked| {
        !asked.is_empty()
            && asked.iter().all(|a| {
                a["method"] == "octos/approval"
                    && a["card_runtime"] == "octos"
                    && a["body"]
                        .as_str()
                        .is_some_and(|body| body.contains("rm -rf build"))
            })
    });
    let answered =
        facts["answers_recorded"].as_u64() == facts["approvals"].as_array().map(|a| a.len() as u64);
    let usage = facts["usage_attached"] == true
        && facts["usage_failure"].is_null()
        && facts["usage_sources"].as_u64() == Some(1)
        && facts["usage_receipts"].as_u64().is_some_and(|n| n > 0);
    let common = completed && carded && answered && usage && facts["leader_exited"] == true;
    match name {
        "approve_once" => common && facts["build_exists"] == false,
        "deny_continues" => common && facts["build_exists"] == true,
        _ => false,
    }
}

#[tokio::test]
#[ignore = "operator-run: drives the operator's real Octos and one of their profiles (ADR-193)"]
async fn native_octos_live_check() {
    let octos = PathBuf::from(
        std::env::var_os(BIN_ENV)
            .unwrap_or_else(|| panic!("set {BIN_ENV} to the canonical path of the Octos binary")),
    );
    assert!(
        octos.is_absolute() && octos.canonicalize().ok().as_ref() == Some(&octos),
        "{BIN_ENV} must be the canonical path of the native binary"
    );
    let profile = std::env::var(PROFILE_ENV).unwrap_or_else(|_| "dev".to_owned());
    let scenarios = [
        Scenario {
            name: "approve_once",
            instruction: "There is a directory named build in the current working directory. Delete it by running exactly this command once with your shell tool, from the current directory: rm -rf build\nThen reply with one short sentence. Do not run any other command.",
            choice: ApprovalChoice::Once,
        },
        Scenario {
            name: "deny_continues",
            instruction: "There is a directory named build in the current working directory. Delete it by running exactly this command once with your shell tool, from the current directory: rm -rf build\nIf the command is denied, do not retry and do not run any other command; reply with one short sentence saying it was not allowed.",
            choice: ApprovalChoice::Deny,
        },
    ];
    let mut passed = true;
    for scenario in &scenarios {
        let mut facts = run(&octos, &profile, scenario).await;
        let pass = verdict(scenario.name, &facts);
        passed &= pass;
        facts["pass"] = json!(pass);
        eprintln!(
            "{}: {}",
            scenario.name,
            serde_json::to_string_pretty(&facts).unwrap()
        );
    }
    assert!(passed, "a live Octos scenario failed; see the facts above");
}
