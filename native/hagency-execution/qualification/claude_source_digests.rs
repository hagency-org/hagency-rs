use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The sources a live Claude Code qualification run exercises (ADR-192
/// slice 4), LF-normalized for hosted Windows checkouts. A change to any of
/// them makes the recorded evidence stale until an operator runs it again.
pub fn current() -> BTreeMap<String, String> {
    let sources: &[(&str, &[u8])] = &[
        (
            "runtime/claude.rs",
            include_bytes!("../../hagency-runtime/src/claude.rs"),
        ),
        (
            "runtime/claude/session.rs",
            include_bytes!("../../hagency-runtime/src/claude/session.rs"),
        ),
        (
            "runtime/claude/session/control.rs",
            include_bytes!("../../hagency-runtime/src/claude/session/control.rs"),
        ),
        (
            "runtime/claude/session/io.rs",
            include_bytes!("../../hagency-runtime/src/claude/session/io.rs"),
        ),
        (
            "runtime/claude/task_mcp.rs",
            include_bytes!("../../hagency-runtime/src/claude/task_mcp.rs"),
        ),
        (
            "runtime/owned/claude.rs",
            include_bytes!("../../hagency-runtime/src/owned/claude.rs"),
        ),
        (
            "execution/local_codex.rs",
            include_bytes!("../src/local_codex.rs"),
        ),
        (
            "execution/approval/claude.rs",
            include_bytes!("../src/approval/claude.rs"),
        ),
        (
            "operator/claude_live.rs",
            include_bytes!("../tests/owned/claude_live.rs"),
        ),
        (
            "operator/claude_source_digests.rs",
            include_bytes!("claude_source_digests.rs"),
        ),
    ];
    sources
        .iter()
        .map(|(path, bytes)| {
            let normalized = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
            (
                path.to_string(),
                format!("{:x}", Sha256::digest(normalized.as_bytes())),
            )
        })
        .collect()
}
