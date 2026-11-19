---
kind: decision
id: ADR-197
title: "Upgrade an engagement's runtime plugin by guarded rebind, not retire and reprovision"
status: Accepted
satisfies: [REQ-NATIVE-PLUGIN-SYSTEM]
amends: [ADR-195]
tags: [native, plugins, engagement, upgrade, audit, operator]
---

## Context

An engagement freezes its resource, including the `runtimePlugin` package and
settings sha, in its provision effect. `owned_dispatch.rs` reads
`effect.resource` and checks the dispatch fingerprint against it.
`PluginService::runtime_launch` and `claim_runtime` (`hagency-plugin/src/lib.rs`)
refuse unless the frozen package equals the plugin's active package. Two
consequences hit the operator on 2026-11-16 while bringing the Qore agent
(herdr-runtime) back up:

- Updating a plugin immediately breaks every running agent that uses it, with an
  Admission failure.
- `PATCH /console/api/resources/{id}/configuration` answers `resource_in_use`
  (`console/resources.rs`) while any engagement is active, so the only path is
  to retire and reprovision. The operator CLI parity table says exactly that
  (`docs/parity/operator-cli-ops-2026-09-24.md`, row 24). Retire and reprovision
  changes the Matrix identity, rooms, workspace and budget, which is far more
  than a plugin update.

## Decision

1. **Older revisions stay launchable while referenced.** The plugin registry
   already keeps `revisions` per package sha. A revision that is not the active
   one remains launchable for an engagement whose frozen package names it. The
   registry keeps a reference count per (plugin_id, package_sha256) derived from
   non-revoked engagements (the frozen package, the rollback slot and the
   marked upgrade target, whether `upgrade_pending`, `pinned` or `halted`), and
   garbage-collects a non-active revision only when its count reaches zero. Launching a non-active revision is allowed only for
   the exact package an engagement is bound to; it is never offered to a new
   provisioning.
2. **A resource runtime change marks engagements, not refuses.** A resource-level
   change to `runtimePlugin` (package or settings) no longer returns
   `resource_in_use`. It records the new qualified target and marks every
   engagement on that resource `upgrade_pending`. Changes that are not runtime
   changes (budget, role set, anything else the guard protects today) keep their
   current refusal.
3. **Per-engagement rebind.** `upgrade_pending` is resolved by rebinding one
   engagement to the new package of the same `plugin_id`, `adapter_id` and
   `framework`. The new package and role must pass the existing qualification
   check (the resource must still qualify for the engagement's role). A package
   with a different plugin, adapter or framework is not a rebind; it still needs
   retire and reprovision. The host also confirms the registry can launch the
   target package for the engagement's framework; otherwise the rebind is
   refused as `target_not_launchable` and nothing is frozen.
4. **Idle boundary.** A rebind commits only in one store transaction that sees
   all of: no dispatch in `leased`, `started` or `parked`; no unresolved
   dispatch (outcome unknown, awaiting resolution); no pending outgoing custody
   (unsettled outgoing journal entry). Otherwise the engagement stays
   `upgrade_pending` with the named blocker, and the rollout re-checks later.
   The rebind never cancels, redoes or re-fences work.
5. **Rolling rollout with a canary.** Engagements roll one at a time. The first
   is the canary. The rollout continues only after the canary's readiness probe
   passes or one task completes on the new package. On failure the rollout
   stops (state `halted`), remaining engagements stay `upgrade_pending` on their
   old, still-launchable package, and the operator may roll the canary back one
   step to the previous package. Rollback is the same guarded rebind in reverse
   and obeys the same idle boundary.
6. **Pin.** The operator may pin an engagement to its current package. A pinned
   engagement is skipped by the rollout, keeps `upgrade_pending` visible as
   `pinned`, and holds its revision's reference count. Unpin returns it to the
   rollout queue.
7. **Identity unchanged.** Engagement id, Matrix identity, rooms, workspace,
   allocation and budget are not touched by a rebind. Only the frozen
   package/settings sha (and the harness template digest bound to it, see
   ADR-198) change.
8. **Audit.** Every rebind, rollback, pin, unpin and refusal writes an append-only
   record: engagement id, old and new package sha, old and new settings sha,
   actor, time, outcome (`applied`, `refused:<reason>`, `rolled_back`,
   `halted`). The record is visible in the console.
9. **Operator surface.** The console gets routes under
   `/console/api/engagements/{id}/upgrade` (status, `pin`, `unpin`, `rollback`)
   and a resource-level rollout status route, with a minimal table in the
   resource and agent pages. Mutating routes use the same operator
   authentication and CSRF rules as the existing resource routes.
10. **Invariants kept.** The fence and custody rules (ADR-165, ADR-181), the
    qualification requirement for the new package and role, and never-silent-redo
    are unchanged. A rebind is refused, not forced, when any of them would be
    weakened. The dispatch fingerprint check stays; the fingerprint is computed
    from the engagement's current binding.

## Consequences

Good, because a plugin update no longer breaks running agents and no longer
forces a retire that destroys identity, rooms and budget.
Good, because the canary and one-step rollback bound the blast radius of a bad
package.
Bad, because the registry now carries reference counts and GC; a counting bug
could keep old packages on disk or collect one still in use. Tests below cover
both.
Bad, because engagements on busy agents may stay `upgrade_pending` for long. The
console shows the blocker.

Rollback: the rebind routes and the marking can be disabled by a single
configuration flag; engagements then keep their frozen package, and a resource
runtime change returns to `resource_in_use`.

Already fixed before this ADR and not redone: silent worker death on an
unsettled outgoing journal (e0eeac52), release of a host-cancelled outgoing
journal entry (b75a0df1, f22a9cb8), herdr-runtime transcript answer, usage and
readiness (plugin 63ad201, 9292adf).

## Alternatives Considered

- Keep retire and reprovision: rejected, it discards identity, rooms, workspace
  and budget for a package update.
- Auto-upgrade every engagement when the plugin updates: rejected, no idle
  boundary, no canary, no operator control.
- Accept any installed package at launch: rejected, it breaks the frozen-resource
  fingerprint and qualification.
- Rebind with in-flight work re-fenced: rejected, it risks silent redo.

## Tests later steps must create

- `engagement_upgrade_older_revision_stays_launchable_while_referenced`
- `engagement_upgrade_revision_collected_when_unreferenced`
- `resource_runtime_change_marks_engagements_upgrade_pending`
- `resource_non_runtime_change_still_refused_while_in_use`
- `engagement_rebind_refused_while_dispatch_leased_started_or_parked`
- `engagement_rebind_refused_with_unresolved_dispatch`
- `engagement_rebind_refused_with_pending_outgoing_custody`
- `engagement_rebind_requires_same_plugin_adapter_framework_and_qualification`
- `engagement_rebind_refuses_a_target_the_registry_cannot_launch`
- `engagement_upgrade_target_counts_as_a_package_reference`
- `engagement_rebind_applies_at_idle_and_keeps_identity`
- `engagement_rollout_halts_after_failed_canary`
- `engagement_rollback_restores_previous_package_one_step`
- `engagement_pin_skips_rollout_and_holds_revision`
- `engagement_rebind_writes_audit_record`
- `console_engagement_upgrade_routes_require_operator_auth`

## Addendum: the provision receipt across a rebind

A rebind rewrites `resource.runtimePlugin` in the complete provision effect.
Re-attach proves that effect by recomputing the inline-factory receipt from it
and comparing the result with `effects.outcome_digest`, which was written at
provision time over the old payload. After a rebind the two differ, so the
engagement was silently dropped from re-attach on every restart.

Decision: keep the stored receipt and the exact-match rule. When the digest of
the current payload does not match, the check reconstructs the one payload
change a rebind can make from `engagement_runtime_rebinds` (append-only):

- the rows of that engagement with `rebind`/`applied` or `rollback`/`rolled_back`,
  in id order, must chain unbroken (each row's old package and settings sha equal
  the previous row's new ones) and end at the payload's current package and
  settings sha;
- with the first row's old package and settings sha put back into
  `resource.runtimePlugin`, the recomputed receipt must equal the stored one.

Any other change (resource, request, plugin id, adapter id, a runtime the audit
does not end at) fails the chain or the digest, so the invariant is not loosened:
a payload still needs the receipt written over it, or the audit that explains it.

Alternatives: storing the original payload or receipt at rebind time needs a
schema change and does not cover the row already rebound live. Re-deriving the
outcome digest inside the rebind transaction rewrites provisioning evidence with
the same authority that changes the payload, so a tampered row could be
re-blessed. The chosen check needs only columns the audit has always held
(package and settings sha, old and new), so rows rebound before this fix are
accepted as they are, with no DB edit. plugin id and adapter id are not in the
audit; a rebind never changes them (`target_blocker`), and the digest covers
them.

Skipped engagements are never silent: `inline_factory_engagements` logs one
warning per active engagement with a complete provision that it leaves out,
naming the engagement and the reason.
