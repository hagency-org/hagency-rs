use hagency_metering::{Framework, MAX_TOKEN_COUNT, TokenCounts, observation::UsageObservation};

fn counts(input: Option<u64>, output: Option<u64>) -> TokenCounts {
    TokenCounts {
        input,
        output,
        cache_write: None,
        cache_read: Some(5),
    }
}

/// ADR-199 P7: a plugin's reported counts map one to one, an unreported count
/// stays unknown, a count past the safe bound is unknown rather than clamped,
/// and the observation is always incomplete.
#[test]
fn plugin_usage_observation_is_incomplete_and_maps_counts() {
    let observation = UsageObservation::plugin_runtime(counts(Some(120), Some(30))).unwrap();
    assert_eq!(observation.framework(), Framework::Plugin);
    assert_eq!(observation.counts(), Some(counts(Some(120), Some(30))));
    assert!(observation.incomplete());
    let past = UsageObservation::plugin_runtime(counts(Some(MAX_TOKEN_COUNT + 1), None)).unwrap();
    assert_eq!(past.counts(), Some(counts(None, None)));
    // The content digest follows the counts, not the call.
    let again = UsageObservation::plugin_runtime(counts(Some(120), Some(30))).unwrap();
    assert_eq!(
        serde_json::to_string(&observation).unwrap(),
        serde_json::to_string(&again).unwrap()
    );
}
