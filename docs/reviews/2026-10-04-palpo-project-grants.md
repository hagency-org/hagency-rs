# Project-grant foundation validation — 2026-10-04

Base: Hagency main `e51a0b1`. Development branch: `feat/palpo-project-grants`.
Source and contract: `docs/design/palpo-project-grants-v1.md`.
Full Rinx ADR 0010 remains in progress.

## Evidence

- `cargo test -p hagency-store --locked --no-fail-fast`: 595 passed, zero failed,
  50 ignored across 67 executables/doc-test groups. This broad run preceded the
  final clock/resource-binding review; affected paths were rerun below.
- Final focused run: `cargo test -p hagency-store --locked --test project_grants
  --test domain --test engagement_allocation --test schema_fixtures --test tasks`:
  46 passed, zero failed. The project grant executable contains 15 scenarios.
- Final `cargo check -p hagency --locked`: passed, including store, execution,
  Palpo, Matrix and native executable integration.
- `git diff --check`: passed.

The new scenarios exercise actual SQLite transactions, concurrent writer jobs,
restart/replay after audit receipt removal, exact actor/project/registration
binding, administrator reassignment, explicit self-approval, shared provider-seat
capacity, aggregate limits, resource edits, revoked queued/started provisioning,
late completion, stale runner capabilities and expiry during a real SQLite lock.
The lock scenario checks that the operation was blocked before expiry and then
refused after the lock released, rather than accepting a contention error as proof.

## Review findings resolved

1. Add provider reservations to existing accounting without debiting again when
   an agent is assigned; continue to count different resource presets on one seat.
2. Preserve grant debits and financial receipts independently of engagement and
   rolling decision-history retention.
3. Route grant expiry/revocation through the existing retirement kernel; fence
   late provision completion and existing runtime capabilities.
4. Read the writer clock after the actual SQLite lock for monetary decisions;
   also check provision claim/completion at their transaction boundary.
5. Prevent the legacy console approval path from bypassing a project's grant.
6. Prevent a contributed resource from moving to a different seat/model while
   its capacity is held; continue to honor a subsequently reduced ceiling.
7. Reconstruct old schemas accurately in historical migration fixtures instead
   of weakening the production migration with `IF NOT EXISTS`.

## Limits

No Palpo command consumer, operator contribution UI, workflow capability
advertisement, client relaunch, live Matrix send or production activation is
part of this slice. Existing ownership and deployment state are preserved.
Final capacity release, legacy migration, remote authority synchronization,
transport receipts and full runtime/Matrix cleanup acceptance remain required.
No Makepad, Android, OpenHarmony, hosted OctoSense or App Hub acceptance is claimed.
