//! Open-descriptor pressure (ADR-199 P5). A process that runs out of file
//! descriptors fails at `accept` with EMFILE, long after the cause. This
//! module reads the live count and the soft `RLIMIT_NOFILE` and says when the
//! count nears the limit, so `/health` and the log name it first. Unknown
//! (a platform without `/proc`) is reported as unknown, never as healthy.
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use hagency_matrix::CancellationToken;

/// Percentage of the soft limit above which the process is near it.
const NEAR_PERCENT: u64 = 80;
const PERIOD: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pressure {
    Unknown,
    Ok,
    Near,
}
impl Pressure {
    pub fn word(self) -> &'static str {
        match self {
            Pressure::Unknown => "unknown",
            Pressure::Ok => "healthy",
            Pressure::Near => "degraded",
        }
    }
}
/// Pure classification from an injected count and limit.
pub fn assess(open: Option<u64>, limit: Option<u64>) -> Pressure {
    match (open, limit) {
        (Some(open), Some(limit)) if limit > 0 => {
            if open.saturating_mul(100) > limit.saturating_mul(NEAR_PERCENT) {
                Pressure::Near
            } else {
                Pressure::Ok
            }
        }
        _ => Pressure::Unknown,
    }
}
/// Descriptors open in this process right now.
pub fn open_count() -> Option<u64> {
    let entries = std::fs::read_dir("/proc/self/fd")
        .or_else(|_| std::fs::read_dir("/dev/fd"))
        .ok()?;
    // The directory handle used for this listing is itself one entry.
    Some((entries.count() as u64).saturating_sub(1))
}
/// Soft `RLIMIT_NOFILE`; `None` when unreadable or unlimited.
pub fn soft_limit() -> Option<u64> {
    parse_soft_limit(&std::fs::read_to_string("/proc/self/limits").ok()?)
}
fn parse_soft_limit(limits: &str) -> Option<u64> {
    let line = limits.lines().find(|l| l.starts_with("Max open files"))?;
    line.split_whitespace().nth(3)?.parse().ok()
}
/// The live reading for `/health`.
pub fn snapshot() -> (Pressure, Option<u64>, Option<u64>) {
    let (open, limit) = (open_count(), soft_limit());
    (assess(open, limit), open, limit)
}
/// Log the transition into `Near` once (re-armed when pressure clears).
pub fn report(pressure: Pressure, open: Option<u64>, limit: Option<u64>, logged: &AtomicBool) {
    if pressure == Pressure::Near {
        if !logged.swap(true, Ordering::Relaxed) {
            tracing::error!(
                "[descriptors] {} of {} file descriptors are open (over {NEAR_PERCENT}% of the soft limit); \
                 EMFILE will refuse new connections. Raise LimitNOFILE or find the descriptor leak",
                open.unwrap_or_default(),
                limit.unwrap_or_default()
            );
        }
    } else {
        logged.store(false, Ordering::Relaxed);
    }
}
/// Periodic check beside the other background owners; exits on shutdown.
pub fn start(shutdown: CancellationToken) {
    tokio::spawn(async move {
        let logged = AtomicBool::new(false);
        loop {
            let (pressure, open, limit) = snapshot();
            report(pressure, open, limit, &logged);
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(PERIOD) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn health_warns_when_open_descriptors_near_limit() {
        assert_eq!(assess(Some(800), Some(1000)), Pressure::Ok);
        assert_eq!(assess(Some(801), Some(1000)), Pressure::Near);
        assert_eq!(assess(Some(1000), Some(1000)), Pressure::Near);
        assert_eq!(assess(None, Some(1000)), Pressure::Unknown);
        assert_eq!(assess(Some(5), None), Pressure::Unknown);
        assert_eq!(assess(Some(5), Some(0)), Pressure::Unknown);
        assert_eq!(Pressure::Near.word(), "degraded");
    }
    #[test]
    fn near_limit_logs_once_until_cleared() {
        let logged = AtomicBool::new(false);
        report(Pressure::Near, Some(9), Some(10), &logged);
        assert!(logged.load(Ordering::Relaxed));
        report(Pressure::Ok, Some(1), Some(10), &logged);
        assert!(!logged.load(Ordering::Relaxed));
    }
    #[test]
    fn soft_limit_is_parsed_from_proc_limits() {
        let text = "Limit Soft Limit Hard Limit Units\nMax open files            1024                 524288               files\n";
        assert_eq!(parse_soft_limit(text), Some(1024));
        assert_eq!(
            parse_soft_limit("Max open files unlimited unlimited files"),
            None
        );
    }
}
