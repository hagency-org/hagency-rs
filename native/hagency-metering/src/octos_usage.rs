//! Fixed untrusted numbers, not source credentials or complete capture proof.
use crate::runtime_usage::ProjectionDiagnostics;
use crate::{MAX_TOKEN_COUNT, MeteringError, TokenCounts};
use serde::Serialize;

/// Which Octos record the counters come from (ADR-193 decision 6).
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// The sum of the ended turns' own `token_usage`.
    Turns,
    /// Octos's totals for the whole session, its sub-agents included.
    Session,
}
/// Octos normalizes every provider to fresh input apart from cache reads and
/// writes, as Claude reports; reasoning is part of output and kept beside it.
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OctosUsage {
    pub counts: TokenCounts,
    pub reasoning: Option<u64>,
    pub coverage: Coverage,
    pub turns: u32,
    pub diagnostics: ProjectionDiagnostics,
}
#[derive(Clone, Serialize)]
pub struct RuntimeEvidence {
    version: u8,
    stream_incomplete: bool,
    usage: OctosUsage,
}
impl RuntimeEvidence {
    pub fn version(&self) -> u8 {
        self.version
    }
    pub fn stream_incomplete(&self) -> bool {
        self.stream_incomplete
    }
    pub fn usage(&self) -> &OctosUsage {
        &self.usage
    }
}
pub(crate) fn normalize(
    mut usage: OctosUsage,
) -> Result<(TokenCounts, RuntimeEvidence), MeteringError> {
    if usage.turns > 64 {
        return Err(MeteringError::Capacity);
    }
    for value in [
        &mut usage.counts.input,
        &mut usage.counts.output,
        &mut usage.counts.cache_read,
        &mut usage.counts.cache_write,
    ] {
        if value.is_some_and(|n| n > MAX_TOKEN_COUNT) {
            *value = None;
            usage.diagnostics.invalid = true;
        }
        usage.diagnostics.missing |= value.is_none();
    }
    if usage.reasoning.is_some_and(|n| n > MAX_TOKEN_COUNT) {
        usage.reasoning = None;
        usage.diagnostics.invalid = true;
    }
    // Every known category, so overflow with another category absent is
    // refused too.
    usage.counts.display_volume()?;
    Ok((
        usage.counts,
        RuntimeEvidence {
            version: 1,
            stream_incomplete: true,
            usage,
        },
    ))
}
