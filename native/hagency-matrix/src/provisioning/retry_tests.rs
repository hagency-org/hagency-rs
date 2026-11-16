//! ADR-199 P6: retry classification, bounded backoff on an injected clock, and
//! named parked reasons. No test sleeps; time is a counter.
use super::*;
use crate::collector::fixtures as common;
use std::sync::atomic::{AtomicU64, Ordering};

fn host(clock: Arc<AtomicU64>) -> TokenProvisioningHost {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("accounts");
    hagency_store::private::directory(&state).unwrap();
    // Keep the directory alive for the test process; the host only holds the path.
    std::mem::forget(dir);
    TokenProvisioningHost::new(
        common::domain::registration(),
        "https://matrix.example.test/",
        "synthetic-registration-token",
        state,
        [73; 32],
        common::load_limits(),
    )
    .unwrap()
    .with_clock(Arc::new(move || clock.load(Ordering::SeqCst)))
}
fn job(
    recovery: Recovery,
    result: Option<Result<Arc<ProvisionedTokenAccount>, Error>>,
) -> Arc<Job> {
    Arc::new(Job {
        factory: None,
        home: Mutex::new(None),
        account: Mutex::new(None),
        result: Mutex::new(result),
        effect: Mutex::new(None),
        awaiting_since: Mutex::new(None),
        recovery: Mutex::new(recovery),
    })
}

#[test]
fn only_transport_timeout_5xx_and_429_are_transient() {
    for error in [
        Error::Transport,
        Error::Timeout,
        Error::Remote(429),
        Error::Remote(500),
        Error::Remote(503),
    ] {
        assert!(error.is_transient(), "{error:?}");
    }
    for error in [
        Error::OutcomeUnknown,
        Error::Storage,
        Error::Identity,
        Error::Recipients,
        Error::Unauthorized,
        Error::Conflict,
        Error::Remote(400),
        Error::Remote(404),
        Error::Cancelled,
    ] {
        assert!(!error.is_transient(), "{error:?}");
    }
}

#[test]
fn backoff_doubles_and_is_capped() {
    let delays: Vec<u64> = (1..=MAX_PROVISION_RETRIES).map(retry_delay_ms).collect();
    assert_eq!(delays, [5_000, 10_000, 20_000, 40_000, 80_000, 160_000]);
    assert_eq!(retry_delay_ms(7), 300_000);
    assert_eq!(retry_delay_ms(1000), 300_000);
}

#[test]
fn a_scheduled_retry_is_due_only_when_the_injected_clock_reaches_it() {
    let clock = Arc::new(AtomicU64::new(1_000));
    let host = host(clock.clone());
    let scheduled = job(
        Recovery {
            attempts: 1,
            next_ms: Some(6_000),
            last: Some(Error::Transport),
            parked: None,
        },
        Some(Err(Error::AwaitingOwner)),
    );
    assert!(!host.retry_due(&scheduled));
    clock.store(5_999, Ordering::SeqCst);
    assert!(!host.retry_due(&scheduled));
    clock.store(6_000, Ordering::SeqCst);
    assert!(host.retry_due(&scheduled));
    // Nothing scheduled (exhausted or done) is never due.
    assert!(!host.retry_due(&job(Recovery::default(), None)));
}

#[test]
fn a_retrying_provision_tells_the_owner_it_needs_no_action() {
    let clock = Arc::new(AtomicU64::new(0));
    let host = host(clock.clone());
    host.jobs.lock().unwrap().insert(
        "provision_en_x".into(),
        job(
            Recovery {
                attempts: 2,
                next_ms: Some(10_000),
                last: Some(Error::Transport),
                parked: None,
            },
            Some(Err(Error::AwaitingOwner)),
        ),
    );
    let notes = host.provision_notes();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].engagement, "en_x");
    assert_eq!(notes[0].state, "provision_retrying");
    assert_eq!(notes[0].reason, "transport_retrying");
    assert!(
        notes[0].remedy.contains("retry 2/6 in about 10s"),
        "{notes:?}"
    );
    assert!(notes[0].remedy.contains("no action needed"));
}

#[test]
fn a_parked_provision_names_its_reason_and_remedy() {
    let clock = Arc::new(AtomicU64::new(0));
    let host = host(clock.clone());
    let parked = |stage, error| Recovery {
        attempts: MAX_PROVISION_RETRIES,
        next_ms: None,
        last: None,
        parked: Some(Parked {
            stage,
            error,
            since_ms: 0,
        }),
    };
    {
        let mut jobs = host.jobs.lock().unwrap();
        jobs.insert(
            "provision_en_a".into(),
            job(
                parked("owner_invite", Error::Transport),
                Some(Err(Error::Transport)),
            ),
        );
        jobs.insert(
            "provision_en_b".into(),
            job(
                parked("inspect_enrollment", Error::OutcomeUnknown),
                Some(Err(Error::OutcomeUnknown)),
            ),
        );
        jobs.insert(
            "provision_en_c".into(),
            job(
                parked("continue_setup", Error::Recipients),
                Some(Err(Error::Recipients)),
            ),
        );
    }
    let notes: BTreeMap<String, ProvisionNote> = host
        .provision_notes()
        .into_iter()
        .map(|n| (n.engagement.clone(), n))
        .collect();
    assert_eq!(notes["en_a"].reason, "transport_retry_exhausted");
    assert_eq!(notes["en_b"].reason, "enrollment_not_provable");
    assert!(notes["en_b"].remedy.contains("never re-created"));
    assert_eq!(notes["en_c"].reason, "owner_membership_not_provable");
    assert!(notes["en_c"].remedy.contains("accept the invitation"));
    assert!(notes.values().all(|n| n.state == "provision_parked"));
}

#[test]
fn only_a_transient_park_is_rechecked_and_only_after_the_cap() {
    let clock = Arc::new(AtomicU64::new(0));
    let host = host(clock.clone());
    let parked = |error| {
        job(
            Recovery {
                attempts: 0,
                next_ms: None,
                last: None,
                parked: Some(Parked {
                    stage: "owner_invite",
                    error,
                    since_ms: 1_000,
                }),
            },
            Some(Err(Error::Transport)),
        )
    };
    let transient = parked(Error::Transport);
    let unprovable = parked(Error::OutcomeUnknown);
    clock.store(1_000 + RETRY_CAP_MS - 1, Ordering::SeqCst);
    assert!(!host.recheck_due(&transient));
    clock.store(1_000 + RETRY_CAP_MS, Ordering::SeqCst);
    assert!(host.recheck_due(&transient));
    // A state that cannot be proven stays parked until an operator acts.
    assert!(!host.recheck_due(&unprovable));
}
