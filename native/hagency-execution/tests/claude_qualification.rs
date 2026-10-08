//! ADR-192 slice 4: the live Claude Code qualification gate. It reads the
//! evidence an operator records with `native_claude_live_qualification`
//! (`tests/owned/claude_live.rs`) and FAILS, never skips, while that
//! evidence is missing, stale against the Claude launch sources, recorded
//! for an unqualified model, or not passing.
use serde_json::{Value, json};
#[path = "../qualification/claude_source_digests.rs"]
mod claude_source_digests;

const EVIDENCE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/qualification/claude-live.json"
);

#[test]
fn native_claude_live_qualification_evidence() {
    let text = std::fs::read_to_string(EVIDENCE).unwrap_or_else(|error| {
        panic!(
            "Claude qualification evidence is missing at {EVIDENCE} ({error}); an operator \
             runs `native_claude_live_qualification` (see tests/owned/claude_live.rs) and \
             commits the evidence it writes"
        )
    });
    let evidence: Value = serde_json::from_str(&text).expect("evidence is JSON");
    assert_eq!(evidence["schema"], "hagency-claude-live-qualification-v1");
    assert_eq!(
        evidence["placeholder"],
        json!(false),
        "evidence is a placeholder"
    );
    assert_eq!(
        evidence["launch_source_digests"],
        serde_json::to_value(claude_source_digests::current()).unwrap(),
        "Claude launch sources changed since the evidence was recorded; run the live \
         qualification again"
    );
    let digest = evidence["claude_executable_sha256"]
        .as_str()
        .unwrap_or_default();
    assert!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "evidence records no executable digest"
    );
    assert!(
        evidence["claude_version"]
            .as_str()
            .is_some_and(|version| version.contains("Claude Code")),
        "evidence records no Claude Code version"
    );
    // Only a model Hagency qualifies for Claude resources (decision 8).
    let model = evidence["model"].as_str().unwrap_or_default();
    let choices = hagency_core::qualification::configuration_choices(
        &hagency_core::qualification::ModelProfile {
            framework: "claude".into(),
            model: String::new(),
            provider: Some("anthropic".into()),
            reasoning: None,
        },
    )
    .unwrap();
    assert!(
        choices
            .iter()
            .any(|choice| choice.model == model && choice.reasoning.is_none()),
        "evidence model {model:?} is not a qualified Claude model"
    );
    let commit = evidence["commit"].as_str().unwrap_or_default();
    assert!(
        commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "evidence records no commit"
    );
    let recorded = evidence["recorded_at_ms"].as_u64().unwrap_or_default();
    assert!(recorded > 1_700_000_000_000 && recorded < 4_102_444_800_000);
    assert!(
        evidence["host"]["os"]
            .as_str()
            .is_some_and(|os| !os.is_empty())
    );
    for name in ["approve_once", "deny_continues", "outside_workspace"] {
        let verdict = &evidence["verdicts"][name];
        assert_eq!(
            verdict["pass"],
            json!(true),
            "{name} did not pass: {verdict}"
        );
        assert_eq!(verdict["protocol"], "Completed", "{name}: {verdict}");
        assert_eq!(verdict["usage_attached"], json!(true), "{name}: {verdict}");
        assert!(
            verdict["usage_receipts"].as_u64().is_some_and(|n| n > 0),
            "{name} recorded no usage: {verdict}"
        );
        assert!(
            verdict["usage_known_growth"]["output"]
                .as_u64()
                .is_some_and(|n| n > 0),
            "{name} counted no output tokens toward the quota: {verdict}"
        );
    }
    let approve = &evidence["verdicts"]["approve_once"];
    assert!(
        approve["approved_file"]
            .as_str()
            .is_some_and(|text| text.contains("gh version")),
        "the approved command did not run: {approve}"
    );
    assert_eq!(
        evidence["verdicts"]["deny_continues"]["denied_file_exists"],
        json!(false),
        "the denied command ran"
    );
    assert_eq!(
        evidence["verdicts"]["outside_workspace"]["outside_file_exists"],
        json!(false),
        "a file appeared outside the workspace"
    );
}
