---
kind: decision
id: ADR-184
title: "A provisioned agent publishes its keys before anyone can write to it: enroll alone, then invite the owner"
status: Accepted
satisfies: [REQ-RUST-MIGRATION-EXECUTION]
tags: [native, matrix, provisioning, e2ee, enrollment]
---

## Context

`appservice_login_home_rooms_enrollment_step_v1` (ADR-147) runs its stages
in this order:

1. The agent creates its encrypted owner DM with the owner already in `invite`.
2. The agent joins the project room.
3. The job waits for the owner's actual join.
4. Only then does pre-activation SDK enrollment (ADR-102) upload the agent's
   device keys and cross-signing identity. `current_at` refuses `Recipients`
   until the owner is joined in both rooms.

Between the owner's join and the key upload, the agent has no device keys.
The owner's client encrypts to the devices it can see, which is none of the
agent's. So anything the owner writes in that window never reaches the agent.
Matrix clients do not re-share a room key with another user's device later.
The loss is permanent.

Observed live on 2026-09-30 on the mini1 Palpo:
- Agent maya's owner joined the DM at 21:04:03 and wrote at 21:04:29 and
  21:04:43.
- maya uploaded its device keys at 21:04:48.
- Both messages stayed undecryptable. No turn ran and nothing was reported.

TS has no such window. Its agent DMs are plaintext (`bridge-matrix.js:10061`),
and its encrypted approval bot holds keys from startup. The operator chose to
keep encrypted agent DMs and close the window, rather than follow TS to
plaintext.

The 2026-10-05 human E2E exposed a second first-message window after key
publication. Activation ran an observation sync with `timeline.limit: 0`
after the owner had joined and sent a DM. That sync received the room key,
but advanced the shared cursor past the ciphertext. The existing late-key
retry could not recover an envelope that intake had never received.

## Decision

The agent is enrolled before the owner is invited. Its enrolled user set
already names the owner, so nothing about the set changes when the owner
joins.

1. **The DM is created with no invite.** It keeps its preset, encryption,
   `history_visibility: invited` and power levels.
2. **The agent's project-room join is unchanged.** The representative
   invites, then the agent joins.
3. **Enrollment runs before the owner is invited.**
   - The frozen user set is the agent plus the owner, and the owner is
     anchored by the pinned master key. It also includes the joined members
     of an encrypted group room, as before.
   - Enrollment needs no room membership to do its work. It reads the
     owner's keys through `/keys/query`, checks them against the anchor and
     claims Olm sessions for the owner's signed devices.
   - The DM check in `current_at` requires the agent joined and no other
     joined member except the owner. The owner may be absent (before the
     invite), invited (a resumed wait) or joined (the pre-activation
     re-check). A left or banned owner refuses. Every provisioning attempt
     re-runs this check, so it must hold at each of those points. The owner
     is joined before activation because the owner stage (step 4) returns
     only once the owner has joined.
   - The ledger's user set is frozen as ADR-102 requires. The pre-activation
     re-verify sees the same set after the owner joins, so ADR-183 B only
     adds devices that appeared in the meantime.
   - Before returning successful enrollment, capture the initial sync cursor
     and durably reserve it for inbox intake. This precedes the owner invite.
     Activation and driver observations still verify identity and room state,
     but cannot advance that cursor with a timeline-less sync. The reservation
     survives restart and grants no task or session authority.
   - If the first ciphertext arrives before its room key, intake retains it
     across restart. A later key-only sync admits and wakes that message
     exactly once, without another message or an `@` mention from the owner.
4. **The owner is invited by new create-only custody stages.**
   - The rooms step ends with `agent-rooms` once the agent-only DM exists and
     the agent has joined the project room.
   - After enrollment, the owner stage records `owner-invite-possible`, then
     `owner-invite-response`, then `complete` once the owner has joined.
   - A lost invite response is inspected with a GET and never repeated.
   - The owner-join wait moves after the invite and keeps its no-deadline
     rule.
   - As before, only the job that saw the wait may resume it. On disk, an
     invite without `complete` could be a wait or a completed custody that
     lost its last record, and a restart during the wait stays with the
     operator.
5. **Activation stays behind the second, GET-only enrollment check.**
   `finish_factory` already runs it after the owner has joined, before
   activation.
6. **Rooms custody from before this change stays valid.** It has
   `complete` and no owner-invite stages, and its DM already holds the
   owner. Re-attach accepts it unchanged. Provisions still in flight were
   not considered: none exist on any rig.

The check cannot tell a resumed wait's `invite` from an owner who joined,
left and was re-invited before activation. Both are accepted, and the owner
stage's own join check is what holds activation back.

An encrypted project room keeps a smaller version of the same window. The
owner can post there between the agent's join and its key upload. Project
rooms on the Palpo rig are plaintext, so this is noted and not addressed.

## Consequences

Good, because the owner's first message is always encrypted to the agent's
device: the device exists and is published before the owner can write.

Good, because nothing is weakened. The anchors, the frozen user set, the
fresh-account check, the recipient rule and the approval cards are unchanged.
Only the moment the owner is invited moves.

Bad, because the rooms custody gains two stages and the owner-join wait
moves out of the rooms step.
Every existing selector that asserts the old order must change, and resumption
must tell the two orders apart.

Bad, because the owner sees the DM invite a few seconds later, after
enrollment completes.

The cursor boundary is covered by
`native_provisioning_sdk_preserves_first_dm_cursor_before_activation` and
the real encrypted-message/late-key restart case in
`native_matrix_retains_undecryptable_event_until_late_room_key`.
`native_configured_fleet_first_dm_during_activation` also runs the service
process with two agents, sends each first encrypted DM at the owner's join,
and checks independently decrypted replies and a subsequent round. It covers
ordinary and application-service provisioning with the local Codex profile
and an offline runtime helper.

## Alternatives Considered

- **Plaintext agent DMs with a separate encrypted approval room (TS
  parity).** Removes the race altogether. The operator declined it on
  2026-09-30 and chose to keep agent DMs encrypted.
- **A "ready" message after enrollment.** It tells the owner when writing
  becomes safe, but messages sent before it are still lost.
- **Requesting lost room keys afterwards.** Clients do not answer key
  requests from another user's devices, so this recovers nothing.


## Amendment: bounded steps and recovery before the owner invite (2026-10-06)

Frank-Lee's live provisioning persisted all five key writes and a Complete SDK
ledger, then exhausted the single 60-second enrollment/initial-sync budget.
The effect stayed Uncertain although no runtime or owner invite had started.

Provisioning now budgets each finite enrollment step separately and gives the
initial inbox synchronization its own budget. The ledger's write bound still
caps the sequence. A provisioning census reads agent identity and both rooms,
then checks the application-service authority once, immediately before use;
it does not nest duplicate authority probes around the same identity GET.
Current domain authority and recipient checks still precede each key POST.

The original sender can return an acknowledged Possible write to Prepared only
when its HTTP function was never entered. A lost POST response cannot use this
negative observation. A transient read timeout at a resumable SDK checkpoint
keeps the effect Started and the same owned job resumes on a later pass. Stage
names are logged with errors. Readiness still requires the original first-inbox
cursor and subsequent runtime activation.

A restarted service may inspect an Uncertain Reserved provision with no runtime
session or transport. This narrowly amends ADR-147's blanket refusal to resume
Started/Uncertain effects: it must reopen the exact completed home and account,
validate stored create/invite/join receipts, find completed encryption enrollment,
and prove that no owner-invite record or DM membership for the owner exists.
Inspection emits no Matrix writes. Missing, tampered or partial records, changed
registration, changed resource, revoked allocation, or any admitted runtime refuse.
After fresh identity/room/authority reads, the store records the recovery and
returns the SAME effect, fence, allocation, account, keys and rooms to Started.
The ordinary remaining stages then finish; register, login, room creation and
key uploads are not replayed. An uncertain owner invite or runtime remains outside
this recovery path.

Regressions: `native_provisioning_enrollment_budget_per_step`,
`native_provisioning_enrollment_read_timeout_resumes_without_key_replay`,
`native_provisioning_recovers_completed_keys_before_owner_invite`, and
`native_provision_recovery_scope_is_inspection_only`.
