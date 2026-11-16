---
kind: decision
id: ADR-199
title: "Operator hardening: name reuse, retirement backoff and descriptor budget"
status: Accepted
satisfies: [REQ-NATIVE-PLUGIN-SYSTEM]
amends: [ADR-187]
tags: [native, operations, palpo, retirement, limits, console]
---

## Context

Three operator problems seen on 2026-11-16 share one theme: a retired agent or a
long-running process must not degrade the service. Each is small enough to
decide here.

## Decision

### Name reuse after retirement

A name whose engagements are all revoked may be reused. The new request gets a
new engagement id and a new Matrix identity. Hagency's collision check already
counts only `pending`, `reserved` and `active` engagements
(`hagency-store/src/domain.rs`), so a revoked name is free on the Hagency side.
If the lock lives on the Palpo side (an account or localpart that is not
released), Hagency's refusal or notice must name the cause (`name_held_by_palpo`)
and the fix instead of a generic conflict. Where the cause lies is investigated
in step ops-name-retire; this ADR fixes only the policy.

#### Investigation result (step ops-name-retire)

Hagency does not lock a revoked name. The collision check in
`DomainRepository::admit` and the partial unique index `live_project_names`
(`domain.sql`) cover only `pending`, `reserved` and `active`; `revoke` moves the
engagement to `revoked` in the same transaction that schedules the retire effect,
so the name is free immediately, whatever the retire effect state (`uncertain`
while Palpo's `retire-agent` returns 503 included). The engagement id derives
from `(fleet_id, request_id)` and the Matrix identity from the engagement id
(`@{fleet}_{engagement}:{server}`), so a reused name never reuses an identity.
The Palpo coordinator and Rinx request path are not in this repository (the
checked-out Palpo crates hold no agent request handling), so a Palpo-side name
check cannot be inspected here. Hagency therefore makes its own refusal exact:
a live holder is refused with reason `agent_name_in_use` (was the generic
`request_conflict`), and the message names the holding engagement, its state and
the fix (retire it or choose another name). A refusal that shows
`request_conflict` or no Hagency reason for a name whose engagements are all
revoked comes from outside Hagency (Palpo or Rinx) and must be raised there.
The `name_held_by_palpo` reason named earlier is not emitted, because Hagency
cannot observe a Palpo hold. See `docs/guides/agent-retirement-and-names.md`.

### Retirement retry backoff and visible state

`bootstrap/palpo_retirement.rs` retries every pending retire effect every 5
seconds forever and logs a WARN each time, while Palpo's
`POST {fleet endpoint}/retire-agent` (`hagency-palpo/src/retirement.rs`) returns
503 because it is not implemented yet. Policy: per-agent exponential backoff
from 5 s, doubling, capped at about 10 minutes, reset after success. The WARN is
logged on the first failure and when the failure kind changes; repeats log at
DEBUG. The console shows per-agent cleanup state (`pending`, attempt count, last
error kind, next attempt time). An effect is never marked applied without
Palpo's proof; a 503 or any non-proof answer leaves it pending.
Implemented in `cleanup_state.rs` (injected clock) and
`bootstrap/palpo_retirement.rs`; the console route is
`GET /console/api/engagements/cleanup-state`, shown on revoked rows of the
engagements page. The state is in memory, so it restarts at attempt 0 with the
process; the effect itself stays durable in the store.

### Descriptor budget

hagency-native ran with soft `RLIMIT_NOFILE` 1024 and hit `EMFILE`. About 816
descriptors came from console assets: `console/assets.rs` `load()` keeps a
`snapshot` proof per asset, each holding the file and parent directory handles
(about 171 files). Each agent's Matrix SDK store holds about 11 descriptors and
revoked agents' stores stayed open. Policy:

1. `LimitNOFILE=65536` in `deploy/hagency-native.service` (the drop-in installed
   by hand on the server on 2026-11-16 becomes the shipped default).
2. Console assets hold bytes and a digest in memory and no per-asset open
   handles after load.
3. A revoked engagement's Matrix SDK store is closed.
4. A health component reports the open descriptor count against the soft limit
   and warns above 80 percent.

## Consequences

Good, because retirement stops filling logs, the operator sees why cleanup is
pending, and descriptor use is bounded and observable.
Bad, because assets served from memory raise RSS slightly (a few MB for about
171 files) and an asset changed on disk needs a reload, as today.

Already fixed before this ADR and not redone: the Herdr systemd user unit, and
the `LimitNOFILE` drop-in installed by hand on the server.

## Alternatives Considered

- Keep the fixed 5 s retry: rejected, it is the log noise and request load seen.
- Raise only the limit: rejected, 816 handles for static assets is a leak of
  intent and the limit hides it.
- Mark retirement applied on a 503: rejected, it would claim cleanup without
  proof.

## Tests later steps must create

- `revoked_name_can_be_requested_again_with_new_identity`
- `name_refusal_names_palpo_hold_when_palpo_holds_it` (implemented as `name_refusal_names_the_live_holder_and_the_fix`; no Palpo hold is observable)
- `retirement_backoff_doubles_to_cap_and_resets_on_success`
- `retirement_503_is_never_marked_applied`
- `retirement_warn_logged_once_per_failure_kind`
- `console_shows_per_agent_cleanup_state`
- `console_assets_hold_no_open_handles_after_load`
- `revoked_engagement_matrix_store_is_closed`
- `health_warns_when_open_descriptors_near_limit`
- `service_unit_sets_limit_nofile_65536`
