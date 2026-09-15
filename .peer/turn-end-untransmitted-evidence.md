# Evidence: spec vs test on the turn-end-untransmitted approval-loss path

All coordinates measured on `0c871f68` ("style: cargo fmt on the refusal
tests"), branch `peer/glm3-turn-end-divergence`. The earlier stale-tree
analysis was discarded; every claim below was re-read on this ref.

## The divergence

- Spec `specs/task-rust-owned-approval-lifecycle.spec.md:162-167`, scenario
  `native_owned_approval_turn_end_untransmitted`, Then: "the operation reports
  **PeerUnavailable never Completed** and the untransmitted arm is stamped".
- Test `native/hagency-execution/tests/support/approval_loss.rs:1378` asserts
  the opposite on both axes: `report.protocol == Protocol::Completed`
  (:1437-1447) and `report.failure` ∈ the quiet family — `None` on Linux,
  `None | ApprovalCancelled | CleanupUnknown` tolerated on macOS (:1449-1456)
  — with the `turn-ended-in-flight-untransmitted` trace stamp required
  (:1463-1466).

## What the product does (production code, this ref)

The turn-end observation is `Update::TurnEnded`
(`hagency-execution/src/approval/observations.rs:59`). Its verdict rule
(:166-181), on the termination snapshot's write custody:

- transmitted (accepted bytes > 0 or a write in custody) →
  `Err(Failure::SettlementUnknown)` (:173);
- host-side cause (`HostClosed`/`Closed`) with zero accepted bytes → `Ok(true)`
  — the quiet family, the drive completes (:174-178);
- peer-side cause with an observed zero-byte snapshot →
  `Err(Failure::PeerUnavailable)` (:181).

The name is deliberately reserved: "never `PeerUnavailable` — that name stays
reserved for `Io("stdin write")` against a gone reader and `PeerEof`"
(:153-156). The same reservation is in the send classifier
(`control.rs:33-44`): `Io`/`PeerEof` with `zero_accepted || !armed` →
`PeerUnavailable`; any transport cause otherwise → `SettlementUnknown`.
`protocol` is the peer's own outcome: `runner.protocol_outcome()` →
`Protocol::Completed` when the peer completed its turn
(`operation.rs:1040-1051`).

The sibling scenario proves the product produces `PeerUnavailable` where it
belongs: `native_owned_approval_peer_gone_before_first_byte`
(`approval_loss.rs:1262`) asserts `Some(Failure::PeerUnavailable)` (:1326)
with transport cause `Io(_)`/`PeerEof` (:1333-1337).

## Verdict: the SPEC overstates

The scenario's Given/When pins the quiet arm, not the `PeerUnavailable` arm.
"The probe ends the turn and exits before the first byte" — the host then
stops the session itself. `host_side` (:160-165) matches `HostClosed`/`Closed`;
with zero accepted bytes the verdict is `Ok(true)` (:174-178), and the peer
ended its turn so `protocol_outcome()` is `Completed` (:1040-1043). That is
correct ADR-046 who-closed-first behaviour: nothing was sent, the fate is
KNOWN (never transmitted), and the trace names it
(`turn-ended-in-flight-untransmitted`). `PeerUnavailable` would be the wrong
name for a host-side close — the peer did not vanish; the host retired the
frame. The product already reserves the name correctly (:155-156) and the
sibling test (:1326) proves it fires for a genuine peer-gone cause.

The test is NOT masking a product bug; its assertions match the product. The
only inconsistency inside the test is its doc comment (:1374-1376), which
repeats the spec's wrong wording ("with zero accepted bytes the verdict is
`PeerUnavailable` (never transmitted), never `Completed`") directly above
assertions that demand `Completed` + quiet — the comment, not the code, is
stale.

The macOS tolerance is the OS close shape, named in the test (:1449-1452) and
the custody comment (`operation.rs:1087-1090`): on macOS the host's own
`HostClosed` can surface the cleanup failure beside the quiet drive
(`CleanupUnknown` from `operation.rs:1121/:1150`, cleanup phase), or the
cancel arm (`ApprovalCancelled`, `observations.rs:126-131`) when another
admitted-but-never-sent entry is present. Both stay inside the quiet family —
never a settlement or protocol verdict — so the split is principled
(OS-level close reporting), not invented.

## Spec rewording

The Then must name what the product guarantees: `Protocol::Completed`, quiet
family, the stamp. Replacement wording is a separate commit on this branch;
bindings gate passes (the Test selector is unchanged, so no binding moves).
