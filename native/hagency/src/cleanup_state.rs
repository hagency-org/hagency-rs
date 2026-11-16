//! ADR-199: per-effect retry backoff for identity cleanup and the per-agent
//! state the console shows. Pure and clock-injected: callers pass `now_ms`, so
//! tests never sleep. Nothing here marks an effect applied; only Palpo's proof
//! does, in `bootstrap/palpo_retirement.rs`.
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Mutex;

pub(crate) const BASE_MS: u64 = 5_000;
pub(crate) const CAP_MS: u64 = 600_000;

/// Delay after `failures` consecutive failures (1 => 5 s, doubling, capped).
pub(crate) fn delay_ms(failures: u32) -> u64 {
    let shift = failures.saturating_sub(1).min(20);
    (BASE_MS << shift).min(CAP_MS)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CleanupStatus {
    pub id: String,
    /// Always `pending`: an applied effect leaves the board.
    pub state: &'static str,
    pub attempts: u32,
    pub last_error: String,
    pub next_attempt_at_ms: u64,
}

#[derive(Default)]
pub(crate) struct Backoff {
    entries: BTreeMap<String, CleanupStatus>,
}
impl Backoff {
    pub(crate) fn due(&self, id: &str, now_ms: u64) -> bool {
        self.entries
            .get(id)
            .is_none_or(|entry| now_ms >= entry.next_attempt_at_ms)
    }
    /// Records a failed attempt. Returns true when it should be logged at WARN:
    /// the first failure, or a different failure kind than the last one.
    pub(crate) fn failure(&mut self, id: &str, kind: &str, now_ms: u64) -> bool {
        let previous = self.entries.get(id);
        let attempts = previous.map_or(0, |entry| entry.attempts).saturating_add(1);
        let loud = previous.is_none_or(|entry| entry.last_error != kind);
        self.entries.insert(
            id.to_owned(),
            CleanupStatus {
                id: id.to_owned(),
                state: "pending",
                attempts,
                last_error: kind.to_owned(),
                next_attempt_at_ms: now_ms.saturating_add(delay_ms(attempts)),
            },
        );
        loud
    }
    /// Success (Palpo's proof was recorded) resets the effect's schedule.
    pub(crate) fn success(&mut self, id: &str) {
        self.entries.remove(id);
    }
    /// Drops entries whose effect is no longer pending (retired elsewhere).
    pub(crate) fn retain_only(&mut self, live: &[String]) {
        self.entries.retain(|id, _| live.contains(id));
    }
    pub(crate) fn snapshot(&self) -> Vec<CleanupStatus> {
        self.entries.values().cloned().collect()
    }
}

static BOARD: Mutex<Vec<CleanupStatus>> = Mutex::new(Vec::new());

pub(crate) fn publish(rows: Vec<CleanupStatus>) {
    if let Ok(mut board) = BOARD.lock() {
        *board = rows;
    }
}
pub(crate) fn board() -> Vec<CleanupStatus> {
    BOARD.lock().map(|board| board.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retirement_backoff_doubles_to_cap_and_resets_on_success() {
        let mut backoff = Backoff::default();
        let mut now = 1_000;
        let mut seen = Vec::new();
        for _ in 0..10 {
            assert!(backoff.due("en_a", now));
            backoff.failure("en_a", "http 503", now);
            let next = backoff.snapshot()[0].next_attempt_at_ms;
            assert!(!backoff.due("en_a", next - 1));
            seen.push(next - now);
            now = next;
        }
        assert_eq!(
            seen,
            [
                5_000, 10_000, 20_000, 40_000, 80_000, 160_000, 320_000, 600_000, 600_000, 600_000
            ]
        );
        // Another effect keeps its own schedule.
        assert!(backoff.due("en_b", 0));
        backoff.success("en_a");
        assert!(backoff.due("en_a", 0));
        backoff.failure("en_a", "http 503", now);
        assert_eq!(backoff.snapshot()[0].attempts, 1);
        assert_eq!(backoff.snapshot()[0].next_attempt_at_ms - now, 5_000);
    }

    #[test]
    fn retirement_warn_logged_once_per_failure_kind() {
        let mut backoff = Backoff::default();
        assert!(backoff.failure("en_a", "http 503", 0));
        assert!(!backoff.failure("en_a", "http 503", 5_000));
        assert!(!backoff.failure("en_a", "http 503", 15_000));
        assert!(backoff.failure("en_a", "timeout", 35_000));
        assert!(!backoff.failure("en_a", "timeout", 75_000));
        // Attempts keep counting across a kind change.
        assert_eq!(backoff.snapshot()[0].attempts, 5);
    }

    #[test]
    fn console_shows_per_agent_cleanup_state() {
        let mut backoff = Backoff::default();
        backoff.failure("en_a", "http 503", 1_000);
        backoff.failure("en_b", "binding", 2_000);
        backoff.retain_only(&["en_a".to_owned()]);
        publish(backoff.snapshot());
        let rows = board();
        assert_eq!(rows.len(), 1);
        let json = serde_json::to_value(&rows[0]).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"id":"en_a","state":"pending","attempts":1,"lastError":"http 503","nextAttemptAtMs":6_000})
        );
    }
}
