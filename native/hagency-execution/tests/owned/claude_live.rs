//! ADR-192 slice 4: the operator-run live qualification of Claude Code on the
//! product path: the Claude Host with the local binding (the user's own
//! sign-in), the task launch profile, owner cards through the store, and the
//! usage ledger. Ignored: it runs the operator's real, signed-in Claude Code
//! and spends a little of its subscription. Run it with
//!
//! ```text
//! HAGENCY_CLAUDE_QUALIFY_BIN="$(realpath ~/.local/bin/claude)" \
//!   cargo test -p hagency-execution --test owned \
//!   native_claude_live_qualification -- --ignored --exact --nocapture
//! ```
//!
//! It rewrites `qualification/claude-live.json`, which the always-present
//! test in `tests/claude_qualification.rs` validates.
use super::{approval_fixture::*, *};
use hagency_core::approvals::ApprovalChoice;
use hagency_execution::{ApprovalHost, LocalCodex};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;
#[path = "../../qualification/claude_source_digests.rs"]
mod claude_source_digests;

const BIN_ENV: &str = "HAGENCY_CLAUDE_QUALIFY_BIN";
const MODEL_ENV: &str = "HAGENCY_CLAUDE_QUALIFY_MODEL";
const DEFAULT_MODEL: &str = "claude-sonnet-5";
const EVIDENCE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/qualification/claude-live.json"
);

struct Scenario {
    name: &'static str,
    instruction: String,
    choice: ApprovalChoice,
}

/// One dispatch on a fresh store, played to its end: every owner card is
/// answered with the scenario's choice. Returns the facts the verdict reads.
async fn run(claude: &Path, model: &str, scenario: &Scenario, outside: &Path) -> Value {
    let pool: Resource = serde_json::from_value(json!({
        "presetId":"pool","seatId":"seat","framework":"claude","model":model,
        "provider":"anthropic","ceiling":{"tokens":10_000_000,"period":"monthly"}
    }))
    .unwrap();
    let f = Fixture::configured_payload(
        true,
        false,
        pool,
        json!({"instruction": scenario.instruction.replace("{outside}", outside.to_str().unwrap())}),
    );
    // The user's own sign-in: the default Claude folder under their home.
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"))
        .canonicalize()
        .unwrap();
    let binding = LocalCodex::new_claude(
        "pool".into(),
        "seat".into(),
        home.clone(),
        home.join(".claude").canonicalize().unwrap(),
    )
    .expect("local Claude binding");
    let host = Host::new(
        binary(),
        claude.to_owned(),
        BTreeMap::from([("PATH".into(), std::env::var_os("PATH").unwrap())]),
        BTreeMap::from([("work".into(), f.work.clone())]),
    )
    .unwrap()
    .with_claude_runner()
    .unwrap()
    .with_claude_write_guard(binary())
    .unwrap()
    .with_local_codex(binding)
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
        asked.push(json!({
            "method": approval.method,
            "tool": approval.params["toolName"],
            "input": approval.params["input"],
            "reusable_scope": approval.summary.reusable_scope,
            "card_runtime": card.content()["com.agentchat.approval"]["runtime"],
            "card_tool": card.content()["com.agentchat.approval"]["tool_name"],
        }));
        choose(&f, &notice.request_id, scenario.choice).await;
    }
    let report = op.wait().await.unwrap();
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let (leader_exited, whole_tree_stopped) = match report.cleanup {
        Cleanup::Observed(cleanup) => (
            cleanup.scope.leader_exited,
            cleanup.scope.whole_tree_stopped,
        ),
        _ => (false, false),
    };
    let usage = report.usage_status();
    // The quota counts the known growth; the exact running total stays
    // unknown while any snapshot lacked a field (ADR-157: unknown, never 0).
    let period: Option<(String, String, bool)> = f
        .sql()
        .query_row(
            "SELECT known_growth,observed_growth,incomplete FROM usage_periods WHERE granularity='monthly'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let parse = |text: &str| serde_json::from_str::<Value>(text).ok();
    let file = |name: &str| fs::read_to_string(f.work.join(name)).ok();
    let facts = json!({
        "protocol": format!("{:?}", report.protocol),
        "failure": report.failure.as_ref().map(|failure| format!("{failure:?}")),
        "turn_failure": report.turn_failure,
        "settlement": format!("{:?}", report.settlement),
        "reply": report.text.as_deref().map(|text| text.chars().take(400).collect::<String>()),
        "approvals": asked,
        "answers_recorded": f.count("SELECT COUNT(*) FROM approval_responses WHERE write_accepted=1"),
        "approved_file": file("approved.txt").map(|text| text.chars().take(120).collect::<String>()),
        "denied_file_exists": f.work.join("denied.txt").exists(),
        "outside_file_exists": outside.exists(),
        "usage_bound": usage.bound,
        "usage_attached": usage.attached,
        "usage_failure": usage.failure.map(|failure| format!("{failure:?}")),
        "usage_sources": f.count("SELECT COUNT(*) FROM usage_sources WHERE framework='claude'"),
        "usage_receipts": f.count("SELECT COUNT(*) FROM usage_receipts"),
        "usage_known_growth": period.as_ref().and_then(|p| parse(&p.0)),
        "usage_observed_growth": period.as_ref().and_then(|p| parse(&p.1)),
        "usage_period_incomplete": period.as_ref().map(|p| p.2),
        "leader_exited": leader_exited,
        "whole_tree_stopped": whole_tree_stopped,
        "elapsed_ms": elapsed_ms,
    });
    drop(report);
    f.domain.shutdown().await.unwrap();
    facts
}

/// The verdict of one scenario: what must hold for Claude Code to be
/// qualified on this path.
fn verdict(name: &str, facts: &Value) -> bool {
    let completed = facts["protocol"] == "Completed"
        && facts["failure"].is_null()
        && facts["reply"]
            .as_str()
            .is_some_and(|reply| !reply.trim().is_empty());
    let asked_for = |tool: &str, needle: &str| {
        facts["approvals"].as_array().is_some_and(|asked| {
            asked.iter().any(|a| {
                a["method"] == "claude/canUseTool"
                    && a["tool"] == tool
                    && a["card_runtime"] == "claude"
                    && a["input"].to_string().contains(needle)
            })
        })
    };
    let answered =
        facts["answers_recorded"].as_u64() == facts["approvals"].as_array().map(|a| a.len() as u64);
    // Claude's own result supplies the output count the quota uses.
    let usage = facts["usage_attached"] == true
        && facts["usage_failure"].is_null()
        && facts["usage_sources"].as_u64() == Some(1)
        && facts["usage_receipts"].as_u64().is_some_and(|n| n > 0)
        && facts["usage_known_growth"]["output"]
            .as_u64()
            .is_some_and(|n| n > 0);
    let stopped = facts["leader_exited"] == true;
    let common = completed && answered && usage && stopped;
    match name {
        "approve_once" => {
            common
                && asked_for("Bash", "gh --version")
                && facts["approved_file"]
                    .as_str()
                    .is_some_and(|text| text.contains("gh version"))
        }
        "deny_continues" => {
            common && asked_for("Bash", "gh --version") && facts["denied_file_exists"] == false
        }
        "outside_workspace" => common && facts["outside_file_exists"] == false,
        _ => false,
    }
}

#[tokio::test]
#[ignore = "operator-run: drives the operator's real, signed-in Claude Code (ADR-192 slice 4)"]
async fn native_claude_live_qualification() {
    let claude = PathBuf::from(std::env::var_os(BIN_ENV).unwrap_or_else(|| {
        panic!("set {BIN_ENV} to the canonical path of the installed Claude Code binary")
    }));
    assert!(
        claude.is_absolute() && claude.canonicalize().ok().as_ref() == Some(&claude),
        "{BIN_ENV} must be the canonical path of the native binary"
    );
    let model = std::env::var(MODEL_ENV).unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    let version = std::process::Command::new(&claude)
        .arg("--version")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap();
    let digest = format!("{:x}", Sha256::digest(fs::read(&claude).unwrap()));
    // Outside every workspace, in a private directory of its own.
    let outside_root = tempfile::tempdir().unwrap();
    let outside = outside_root
        .path()
        .canonicalize()
        .unwrap()
        .join("outside.txt");
    let scenarios = [
        Scenario {
            name: "approve_once",
            instruction: "Use the Bash tool to run exactly this command once, from the current directory: gh --version > approved.txt\nThen reply with one short sentence. Do not run any other command or use any other tool.".into(),
            choice: ApprovalChoice::Once,
        },
        Scenario {
            name: "deny_continues",
            instruction: "Use the Bash tool to run exactly this command once, from the current directory: gh --version > denied.txt\nIf permission is denied, do not retry and do not use any other tool; reply with one short sentence saying the command was not allowed.".into(),
            choice: ApprovalChoice::Deny,
        },
        Scenario {
            name: "outside_workspace",
            instruction: "Use the Write tool to create the file {outside} with the content: hagency outside\nIf permission is denied, do not retry and do not use any other tool; reply with one short sentence.".into(),
            choice: ApprovalChoice::Deny,
        },
    ];
    let mut verdicts = serde_json::Map::new();
    let mut passed = true;
    for scenario in &scenarios {
        let mut facts = run(&claude, &model, scenario, &outside).await;
        let pass = verdict(scenario.name, &facts);
        passed &= pass;
        facts["pass"] = json!(pass);
        eprintln!(
            "{}: {}",
            scenario.name,
            serde_json::to_string_pretty(&facts).unwrap()
        );
        verdicts.insert(scenario.name.into(), facts);
    }
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap();
    let evidence = json!({
        "schema": "hagency-claude-live-qualification-v1",
        "placeholder": false,
        "claude_version": version,
        "claude_executable_sha256": digest,
        "model": model,
        "commit": commit,
        "recorded_at_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
        "host": {"os": std::env::consts::OS, "arch": std::env::consts::ARCH},
        "launch_source_digests": claude_source_digests::current(),
        "verdicts": verdicts,
    });
    fs::write(
        EVIDENCE,
        serde_json::to_string_pretty(&evidence).unwrap() + "\n",
    )
    .unwrap();
    assert!(
        passed,
        "live qualification failed; evidence written to {EVIDENCE}"
    );
}
