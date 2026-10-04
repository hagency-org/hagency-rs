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

## Foundation checkpoint limits

At the foundation checkpoint, no Palpo command consumer, operator contribution UI, workflow capability
advertisement, client relaunch, live Matrix send or production activation is
part of this slice. Existing ownership and deployment state are preserved.
Final capacity release, legacy migration, remote authority synchronization,
transport receipts and full runtime/Matrix cleanup acceptance remain required.
No Makepad, Android, OpenHarmony, hosted OctoSense or App Hub acceptance is claimed.


## Command transport follow-up

The next slice adds the closed v1 workflow consumer and a fixed authenticated
Palpo authorization endpoint, ten-second leases checked after the SQLite writer
lock, and immutable business receipts committed with the actual operation.
Admission, provider/project debit, provision effect and receipt use the existing
transaction kernels; there is no second accounting implementation. The expiry
path can terminalize without a working Matrix or Palpo connection.

Review found and resolved:

1. Atomic admission: a failed decision or receipt write must not leave a new
   pending engagement. Trigger-injected receipt failures roll back the entire
   operation; a subsequent retry succeeds once.
2. Cross-fleet scope: top-up/revoke must read the grant's fleet, not merely trust
   a globally unique-looking grant ID supplied in a command.
3. Current role lookup: a command queued before demotion/reassignment cannot
   use its old actor context as fresh authority. Outages and expired execution
   leases retry; explicit refusals persist. Leases provide a bounded distributed
   authorization window, not an instantaneous global revocation guarantee.
4. Frozen publication: response loss and restart resend the original update and
   sequence; newer receipts are not marked published by an older acknowledgement.
5. Immutable historical results: replay after assignment/revocation cannot reset
   the current grant. Business receipts are independent of delivery ACKs.
6. Durable status sources and pagination: a missing probe JSON file does not hide
   workflow agents. Pages are scoped to the current fleet registration and are
   retained until their exact publication is acknowledged; the fixture includes
   126 agents to exercise later pages.
7. Closed wire decoding extends through legacy request/agent objects. An
   independently generated shared JSON corpus pins all seven commands, Unicode
   names, safe integer deadlines, digests and typed receipts in Rust and Node.

No workflow capability is advertised yet. Operator contribution controls,
contribution snapshots, Palpo Inbox command/result projection and Rinx forms are
still outstanding. No live Matrix messages, production activation or user profile
restart occurred. These backend tests are not Makepad/mobile acceptance evidence.

Follow-up validation:

- `cargo test -p hagency-core -p hagency-store -p hagency-palpo --locked`:
  **667 passed, zero failed, 74 ignored**, across 87 executable/doc-test groups.
  This includes eight project-command store scenarios, 15 grant scenarios and
  seven TLS catalog/authorization/publication tests.
- A final closed nested-field decoder review followed that broad run.
  `cargo test -p hagency-core --locked --lib project_commands` passed the shared
  corpus and nested unknown-field refusals; final `cargo check -p hagency
  --locked` also passed with the production consumer using that decoder.
- Companion Palpo `bb593b6`: all **93 backend tests passed**; the final focused
  command/outbound run passed **23 tests** after the atomic receipt guard.
- Shared corpus SHA-256:
  `98b4796dfd9322300b0106d0b0114fa2cca7477414b638c8e112dea504dee84d`.
- `git diff --check`: passed. No deployment or Makepad claim for this slice.

## Operator contribution UI and publication follow-up

Added authenticated create/list/revoke routes and the actual native console
resource-page form. Queued mutations retain their original finite session gate,
resource revision and registration generation. Creation/revocation reuse the
accounting/retirement kernels. Exact pending requests are saved before POST and
recover after reload. Revocation explicitly keeps capacity held. A bounded target
endpoint supplies actual fleet IDs; the older project-side ID is a server name.

Contribution pages carry at most 16 rows in frozen publications. Their cursor
advances from acknowledged bytes; restart and lost ACKs cannot skip later rows.
Current registration scoping excludes old grants and foreign fleets. Palpo
validates immutable grant data, finite limits and monotonic held reservations,
committing observations with the sequence. Accepted project records retain their
originating registration generation so rotation fences old authority.

Validation:

- Store selectors `resource_contributions`, `project_grants`, `project_commands`:
  **26 passed**, no failures. Original-session retirement, changed registration,
  publication scope and preserved historical reservations are covered.
- `hagency-palpo --test catalog`: **8 passed**, including 18 contributions over
  two pages, exact frozen replay after restart, then cursor wrap.
- Native HTTP/browser contribution selector: **4 passed**. Chrome uses the
  production built assets and real isolated SQLite. It drops the first committed
  POST response, reloads, retries identical bytes, verifies one reservation,
  revokes and verifies persistence. A final browser run passed after capture
  adjustment. Desktop and 430px captures were inspected; narrow capture uses a
  tall viewport to avoid stitching the fixed console shell. This is responsive
  browser evidence, not device acceptance.
- Production console asset build passed. Palpo companion full backend suite:
  **98 passed**, zero failures. `git diff --check` passed in both repositories.

No production activation, visible client restart, live Matrix mutation or new
Makepad/mobile acceptance is part of this follow-up. Hagency-originated
association and Palpo/Rinx project-budget forms, assigned-admin Inbox decisions
and their result projection remain before capability advertisement.
