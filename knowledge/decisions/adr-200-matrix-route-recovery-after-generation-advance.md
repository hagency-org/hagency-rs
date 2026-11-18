---
kind: decision
id: ADR-200
title: "Matrix route recovery after a room-generation advance"
status: Accepted
satisfies: [REQ-NATIVE-PLUGIN-SYSTEM]
amends: [ADR-188, ADR-184]
tags: [matrix, routing, recovery]
---

## Context

Two live agents stopped receiving Matrix traffic after a room generation advanced, and neither recovered without a restart.

`current_matrix_routes` (migration 015) only lists a route when `route.room_generation` and the agent's `membership.room_generation` both equal the room's current generation. When any agent's observation advances a shared room, every other agent's route retires in `reconcile`. An agent regains a route only if its own `collect` re-observes the room and `inboxes()` then resolves a session id it has never used, because `resolve` never rebinds a retired id (it returns `Conflict`).

**qore-reviewer, DM.** Two defects. `reattach_factory` named the DM session `session_{eng}` or `session_{eng}_{tgen}` and never included the room generation. After the DM scope advanced, the old id existed with a retired route, so `resolve` returned `Conflict`. In a running worker the DM `HostRoom.generation` was the static configured value, so `observe_room` returned `Error::Generation` on every pass once the scope was ahead. The driver parked with `Matrix refresh refused error=Generation` once a minute and `run()` returned before `inboxes()`. `observed_room_generation` also returned the static value for Direct rooms, so `intake::targets` would have refused a session at the new generation.

**qore-dev, project room.** A lagging collector normally recovers through `refresh_matrix_group_room`. Three gaps left it without a route: the compare-and-swap `expected` was read before the HTTP GET, so a writer outside the coordinator lock could make it stale. A failing DM aborted the whole `collect` pass before `inboxes()` ran. And `reattach_known_agents` logged `Busy` at `debug!`, so a worker that never started left no line.

None of the refusal lines carried the engagement, so the looping agent could not be identified from the log.

## Decision

### D1. DM session naming includes the room generation after the first

`dm_session_id(engagement, transport_generation, room_generation)` in `hagency-matrix/src/provisioning/factory.rs`, next to `joined_session_id`. Room generation 1 keeps today's names: `session_{eng}` for transport generation 1, `session_{eng}_{tgen}` otherwise. Room generation above 1 is `session_{eng}_{tgen}_{rgen}`. Names of healthy agents and existing rows stay valid, a bumped DM always gets an unused id, and the store rule that a retired id is never rebound is untouched. Existing `session_{eng}` rows stay retired. There is no migration and no rewrite. `finish_factory` keeps `session_{eng}`, which equals the helper at (1, 1).

### D2. Recovery lives in the store rule and the collector; the driver only reports

- Store: one private helper holds the generation decision of `refresh_matrix_group_room`. `refresh_matrix_direct_room(input, expected, now)` applies the same two arms. An available snapshot that is identity-equal keeps the generation if the members are unchanged and advances it by one otherwise. A room unavailable only because of `OBSERVATION_FAILED`, with a matching `expected`, advances by one. Everything else returns `Error::Generation` and stays retired. The new generation still goes through `observe_room`, the single validator. `resolve`, `reconcile`, the view and the migrations do not change.
- Collector: a DM with an existing scope is published through `refresh_matrix_direct_room`. The first observation of a DM keeps `observe_matrix_room`. On `Error::Generation` from either refresh, the collector re-reads `prior` and retries once in the same call. `observed_room_generation` returns the verified observed generation for factory Direct rooms, with the same checks as groups.
- Factory: `inboxes()` re-resolves the DM session on every pass through `dm_session_id`, like the project session. `reattach_factory` uses the same helper with the DM's current store generation.
- Driver: no recovery logic. The parked loop already retries with backoff and each retry runs `collect`. Every refusal line now carries the engagement and the error: "Matrix refresh refused", "factory inbox observation refused", "Matrix crypto enrollment refused", the parked retry line, the `Busy` re-attach line, and "re-attached factory agent did not start". `Busy` moves from `debug!` to `warn!`, and the agent is still left to discovery.

### D3. A running worker recovers without a restart

Both the DM and the group loop were seen in running workers, so a restart-only fix would leave the failure in place. Restart-time DM re-attach is fixed by D1. `reattach_collector` still seeds an unavailable DM at generation + 1 for any reason. That is existing behavior, it passes through the validator, and it is not changed here.

### Unchanged safety rules

A room retired because the owner left, the snapshot is unsafe, the privacy settings do not match, or "Matrix current recipient proof changed" stays retired and the pass reports `Error::Generation`. The transport identity check in `collect` and the `registration_generation` check are not loosened. `ingress_since`, `visibility_since`, `own_direct_room_created`, the ADR-184 and ADR-199 owner-DM rules, key creation, invites and joins are unchanged. Recovery makes no outbound Matrix write.

## Consequences

- Good: a lagging agent heals on its next collector pass, with no restart and no data rewrite. Every refusal names its engagement. A refused refresh in a running worker logs two lines on purpose: `Matrix refresh refused` carries the error, and `Matrix refresh refused; the worker retries` carries the park state, the error class and the backoff.
- Bad: the store has one more entry point, `refresh_matrix_direct_room`. After a restart, a DM retired for any reason is still re-observed at generation + 1, as before. A DM's session id now changes when its room generation does, so readers must not assume `session_{eng}` is current.

## Alternatives considered

- Recover in the driver's parked loop. Rejected: it puts the compare-and-swap rule outside the validator.
- Rebind the retired session id. Rejected: it breaks the store rule that a retired id is never reused.
- Always put the room generation in the DM name. Rejected: it renames every healthy agent.
