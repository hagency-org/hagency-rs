---
kind: decision
id: ADR-199
title: "Operator hardening: name reuse, retirement backoff, descriptor budget and provisioning recovery"
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

### Provisioning recovery after a transient failure past the owner invite (P6)

Seen live on 2026-11-16 (engagement `en_5da01cad...`, agent `qore-agent`): the
agent account, rooms, crypto enrollment and owner invite all succeeded and the
owner joined the DM. A later step failed with `Error::Transport`
(`provision room step failed ... error=Transport`, then `provision attempt
failed ... activated=false`). Two defects followed:

- `token_provision/rooms.rs` observed the effect `Unknown` on any error, and
  `provisioning.rs` `provision_pass` skips an uncertain provision whose job is
  still in `self.jobs`. In process it was never retried.
- After a restart, `recover_enrolled` called `resume_before_owner_invite`, whose
  rooms inspection refused every state with an owner-invite record
  (`OutcomeUnknown` at stage `inspect_enrollment`). The agent stayed
  `reserved`/`uncertain`, retire was the only way out, and the name stayed
  locked (see name reuse above).

Decision:

1. **Retryable classification.** `Error::is_transient()` is true for
   `Transport`, `Timeout`, `Remote(429)` and `Remote(5xx)` only. A transient
   error is retried only at a step proven read-only or idempotent: the owner
   step after the invite is proven (response stored, or `joined_dm` read the
   owner as invited or joined). A transient error at any write whose outcome is
   unknown (account register, room create, the invite POST itself, enrollment
   upload) is still observed `Unknown`; nothing is replayed.
2. **Bounded backoff.** The effect stays `Started`, nothing is observed
   `Unknown`, and the job is resumed by `provision_pass` when its retry is due:
   5 s doubling, capped at 300 s, at most 6 retries. The clock is injected
   (`TokenProvisioningHost::with_clock`); tests never sleep. After the sixth
   failure the effect is observed `Unknown` and the provision is parked with
   the reason `transport_retry_exhausted`. A provision parked on a transient
   error is dropped from `self.jobs` after one cap interval so the next pass
   re-inspects it through the resume path. A parked state that is not transient
   stays parked until an operator acts.
3. **Resume proof.** `resume_enrolled` (formerly `resume_before_owner_invite`)
   accepts a rooms custody with the identity, key ledger and rooms records
   complete, whether the owner invite is absent, possibly sent (read back,
   never repeated) or answered. It still requires the account whoami to match,
   the SDK enrollment status to be `Complete`, and the owner step to prove the
   owner is invited or joined. It creates nothing, enrolls nothing and
   re-registers nothing; the key ledger is not written. Refused and parked:
   an invite answer without its "possible" record, a completed step without
   invite records, an unreadable or unsafe DM snapshot (`Recipients`).
4. **Named parked reasons.** Fixed codes with a remedy: `enrollment_not_provable`
   (keys are never re-created; inspect or retire), `account_not_provable`,
   `owner_membership_not_provable` (accept the invitation to the DM room),
   `transport_retry_exhausted`, `provision_failed` (names the stage). Every park
   is logged once with stage, code and remedy.
5. **Owner-visible surfacing.** The provision pass publishes each stuck
   provision's state (`awaiting_owner`, `provision_retrying`,
   `provision_parked`), `provision_reason` and `provision_remedy` on the
   agent's console row. An owner who has not joined sees "accept the invitation
   to room X"; a retrying provision says no action is needed; a parked one names
   the operator action. Status only: no decision reads it.
6. **Invariants kept.** No key is re-created, no uncertain write is replayed, no
   fence is loosened, approvals are unchanged, and nothing is marked applied
   without proof. Retire stays the last resort when the reason says the state
   cannot be proven.

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
- `only_transport_timeout_5xx_and_429_are_transient`
- `backoff_doubles_and_is_capped`
- `a_scheduled_retry_is_due_only_when_the_injected_clock_reaches_it`
- `resume_proof_accepts_every_provable_invite_state`
- `resume_proof_refuses_a_state_it_cannot_prove`
- `a_parked_provision_names_its_reason_and_remedy`
- `a_retrying_provision_tells_the_owner_it_needs_no_action`
- `only_a_transient_park_is_rechecked_and_only_after_the_cap`
- `native_provisioning_recovers_completed_keys_before_owner_invite` (no second enrollment, ledger untouched)
