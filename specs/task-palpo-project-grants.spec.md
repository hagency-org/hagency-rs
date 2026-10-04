spec: task
name: "Bounded Palpo project grants for Rinx ADR 0010"
inherits: project
tags: [active, rust, palpo, grants]
---

## Intent

Implement Hagency's part of the accepted Rinx role correction. Only Hagency
contributes resources; one designated Palpo administrator approves projects;
explicitly assigned project administrators decide agents and top-ups within an
accepted finite grant. No second human Hagency verdict is required inside it.

## Constraints

- Preserve original owners, registrations and engagement identities.
- Catalog publication and connection verification confer no budget authority.
- Bind issuer, current registration generation, project/room/owner, revision,
  explicit approvers, self-approval policy, limits and expiry.
- Reserve real provider/shared-seat capacity before reporting a project allocated.
- Atomically debit grants, create effects and persist immutable decision receipts.
- Replay cannot debit twice, including after restart or rolling audit retention.
- Recheck authority after the actual writer lock; reject unassigned/cross-project,
  demoted, expired and revoked decisions. Fence provisioning and execution.
- Cleanup uncertainty does not release capacity or authorize another agent.
- Keep execution/tool approvals on their existing owner-authorized path.
- Do not expose these trusted store interfaces as unauthenticated HTTP APIs or
  advertise a transport capability before its complete consumer/receipt path.

## Current slice

Grant types, migrations 61/62, transaction/accounting primitives, administrator
reassignment, grant retirement, writer clocks, authenticated Palpo commands,
short execution leases, atomic business receipts and frozen-publication recovery. Relevant boundaries are
hagency-core, hagency-store, its historical migration fixtures, fixed Matrix
error categories, and the native fleet reconciliation loop. The follow-up also
includes authenticated console contribution routes, resource-page controls and
bounded contribution pages through the existing frozen publication lane.

## Acceptance

- Real SQLite grant tests cover shared-seat headroom, no double reservation,
  concurrent overdraw, exactly-once top-up, current role/project scope, revision
  changes, explicit self-approval, expiry after lock, late provisioning completion
  and revoked runner capabilities.
- The store regression suite and native Hagency executable compile/check pass.
- An actual served-console browser walk exercises loss of a committed response,
  reload, exact retry, single reservation and revocation. Desktop and 430px
  layouts are inspected; transport tests cover page retry/restart and cursor
  advancement; store tests cover fleet scope and registration rotation.
- The remaining full-ADR integration is explicit in the design checkpoint.

## Remaining full workflow

Hagency-originated association, Palpo Inbox/Rinx command integration, richer
status/usage publication, explicit legacy migration and
inspected capacity release, and live
Rinx approval-to-Matrix readiness/cleanup validation. See
`docs/design/palpo-project-grants-v1.md` and Rinx ADR 0010.
