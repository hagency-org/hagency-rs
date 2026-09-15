# turn-end-untransmitted: gate-expiry mechanism (WIP diagnosis)

Hosted Ubuntu run 34911532754 failed
`approval_loss::native_owned_approval_turn_end_untransmitted`
(native/hagency-execution/tests/support/approval_loss.rs:1378) with the owned-dispatch
thread panicking at native/hagency-execution/src/approval.rs:80
"bounded test receipt gate expired".

## Mechanism (established, not yet fixed)

1. The scenario holds the host at `Fault::RecheckGate`. The host coordinator
   thread enters `Gate::wait()` (approval.rs:76), sets `entered=true`, then spins
   for a derived 2.5s deadline (`OPERATION_BUDGET_MS/10`) until `release` is set.

2. The test waits for `gate.entered` (approval_loss.rs:1406-1413), then writes
   `owned-dispatch.approval-release`, then **waits for the probe's
   `owned-dispatch.approval-resolving` marker** (lines 1424-1431) **before**
   `gate.release.store(true)` (line 1432).

3. Meanwhile the probe (`owned-approval-turn-untransmitted`,
   approval_probe/mod.rs:174-190), on seeing `approval-release`: emits
   `turn/completed`, then `announce(approval-resolving)`, then `return Ok(false)`
   (exits).

4. The `turn/completed` note is read by the host's pump *while the gate is still
   held*: `drive.pump` polls `next_observed_or_control` (observations.rs:235),
   which observes `Update::TurnEnded` → terminal, then `runner.stop()` and
   `future.await` — i.e. it **blocks awaiting the gate release** inside
   `Gate::wait()`.

5. So the host is blocked on the gate release while the test is still polling for
   the probe marker. Under load the release chain (write release → probe wake →
   JSON flush → marker write → test poll) exceeds 2.5s → the gate deadline fires
   and `Gate::wait()` panics.

## Fix direction (handshake, never widen gate/budget)

The test must not sit on the gate-release critical path behind a probe marker
round-trip. The `turn-ended-in-flight-untransmitted` diagnostics mark already
proves the turn end was observed while in-flight (held) — release the gate as
soon as the host is proven held and the turn end is on the wire, or use a
process-lifetime lock handshake like `peer_gone_before_first_byte` (4573f769).
To be finalized.
