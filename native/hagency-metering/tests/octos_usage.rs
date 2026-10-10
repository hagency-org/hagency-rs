use hagency_metering::{
    Framework, MAX_TOKEN_COUNT, MeteringError, TokenCounts,
    observation::UsageObservation,
    octos_usage::{Coverage, OctosUsage},
    parse_session,
    runtime_usage::ProjectionDiagnostics,
};
fn input() -> OctosUsage {
    OctosUsage {
        counts: TokenCounts {
            input: Some(10),
            output: Some(20),
            cache_read: Some(30),
            cache_write: Some(40),
        },
        reasoning: Some(5),
        coverage: Coverage::Session,
        turns: 2,
        diagnostics: ProjectionDiagnostics::default(),
    }
}
fn encoded(input: OctosUsage) -> serde_json::Value {
    serde_json::to_value(UsageObservation::octos_runtime(input).unwrap()).unwrap()
}
/// ADR-193 decision 6: Octos usage is runtime evidence with framework octos.
/// Input is fresh input beside cache reads and writes, reasoning stays beside
/// output, and every field is part of the content identity.
#[test]
fn native_metering_octos_runtime() {
    let original = input();
    let observation = UsageObservation::octos_runtime(original).unwrap();
    assert_eq!(observation.framework(), Framework::Octos);
    assert!(observation.incomplete());
    // Reasoning is part of output, never added again.
    assert_eq!(
        observation.counts().unwrap().display_volume().unwrap(),
        Some(100)
    );
    assert!(observation.runtime_evidence().is_none());
    assert!(observation.claude_runtime_evidence().is_none());
    let evidence = observation.octos_runtime_evidence().unwrap();
    assert_eq!(evidence.version(), 1);
    assert!(evidence.stream_incomplete());
    assert!(evidence.usage() == &original);
    let first = encoded(original);
    assert_eq!(first["framework"], "octos");
    assert_eq!(
        first["octos_runtime_evidence"]["usage"]["coverage"],
        "session"
    );
    for mode in [
        "input",
        "output",
        "read",
        "write",
        "reasoning",
        "coverage",
        "turns",
        "missing",
    ] {
        let mut changed = original;
        match mode {
            "input" => changed.counts.input = Some(11),
            "output" => changed.counts.output = Some(21),
            "read" => changed.counts.cache_read = Some(31),
            "write" => changed.counts.cache_write = Some(41),
            "reasoning" => changed.reasoning = None,
            "coverage" => changed.coverage = Coverage::Turns,
            "turns" => changed.turns = 3,
            "missing" => changed.diagnostics.missing = true,
            _ => unreachable!(),
        };
        assert_ne!(
            first["snapshot_digest"],
            encoded(changed)["snapshot_digest"],
            "{mode}"
        );
    }
    // Unknown stays unknown, and flags its absence.
    let mut unknown = original;
    unknown.counts = TokenCounts {
        input: None,
        output: None,
        cache_read: None,
        cache_write: None,
    };
    let a = encoded(unknown);
    assert_eq!(a["totals"]["input"], serde_json::Value::Null);
    assert_eq!(
        a["octos_runtime_evidence"]["usage"]["diagnostics"]["missing"],
        true
    );
    let mut unsafe_input = original;
    unsafe_input.counts.input = Some(MAX_TOKEN_COUNT + 1);
    unsafe_input.reasoning = Some(u64::MAX);
    let a = encoded(unsafe_input);
    assert_eq!(a["totals"]["input"], serde_json::Value::Null);
    assert_eq!(
        a["octos_runtime_evidence"]["usage"]["reasoning"],
        serde_json::Value::Null
    );
    assert_eq!(
        a["octos_runtime_evidence"]["usage"]["diagnostics"]["invalid"],
        true
    );
    let mut overflow = original;
    overflow.counts.input = Some(MAX_TOKEN_COUNT);
    overflow.counts.output = None;
    assert!(matches!(
        UsageObservation::octos_runtime(overflow),
        Err(MeteringError::Overflow)
    ));
    let mut turns = original;
    turns.turns = 65;
    assert!(matches!(
        UsageObservation::octos_runtime(turns),
        Err(MeteringError::Capacity)
    ));
    // Octos has no transcript: its usage is never parsed from text.
    for snapshot in ["", "{\"usage\":{\"input_tokens\":1}}"] {
        assert!(matches!(
            parse_session(Framework::Octos, snapshot),
            Err(MeteringError::InvalidRecord)
        ));
    }
    for framework in [Framework::Claude, Framework::Codex] {
        let legacy = serde_json::to_value(UsageObservation::parse(framework, "").unwrap()).unwrap();
        assert!(legacy.get("octos_runtime_evidence").is_none());
    }
}
