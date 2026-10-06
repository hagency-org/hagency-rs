---
kind: decision
id: ADR-190
title: "Owners request agents from Rinx through a Hagency mini app, not from Palpo's web admin"
status: Superseded
satisfies: [REQ-RUST-MIGRATION-EXECUTION]
tags: [native, onboarding, rinx, miniapp, palpo, requests]
---

## Context

Superseded by [ADR-191](adr-191-server-engagement-coordinator-approvals.md), which
uses an explicit server engagement and a single delegated coordinator approval.
The proposal below is historical and is not the active approval policy.

To get an agent today, an owner leaves their Matrix client and uses Palpo's web admin. That page:

1. creates the project room and writes its `com.hagency.admin.binding.v1` state;
2. creates the encrypted approval room and invites the fleet's approval bot;
3. has the fleet's representative join the project room, using the fleet's own credentials on Palpo's backend;
4. lists the fleet's published resources from Palpo's API;
5. sends `com.hagency.engagement.request.v1` to the fleet's reception room.

Hagency verifies a request only from Matrix state: the rooms, their members and power levels, the binding state and the request event (`hagency-core/src/authority.rs`, `verify_request`). Every step except 3 and 4 is an ordinary Matrix call made with the owner's own session. So the owner's Matrix client can make the request, if the client can do those calls and steps 3 and 4 no longer need Palpo's backend.

Rinx runs Octoscript mini apps that act as the user through an A2App-compatible Matrix API (Rinx ADR 0005). Each instance holds only the services and rooms granted at import, and credentials stay in Rinx. The API has about 45 services (messages, invites, joins, DMs, room info, members, power levels, spaces). It has no service to create a room, read a room's state by type, write a state event, or send a custom event type.

## Decision

1. **A "Hagency agents" Octoscript mini app makes the request.** It is distributed through the App Hub (Rinx ADR 0006) and runs in Rinx and OctoSense. Its flow:
   1. pick a fleet the owner can see (a reception room the owner is in) and one of its published resources;
   2. enter the agent's name, role, requested tokens and daily rate;
   3. the app creates the project room with its binding state and the encrypted approval room, invites the representative and the approval bot, and sends the request event to the reception room;
   4. it shows the request's state. When the operator approves, the agent's DM invitation arrives in Rinx as usual.

   Palpo's web admin keeps its project and request pages; the mini app does not replace them.

2. **Rinx adds four Matrix services to the mini-app API**, each behind the existing grant and lease checks:
   - `matrix.create_room`: name, topic, invites, encryption on or off, initial state;
   - `matrix.room_state`: read state events of one type from a granted room;
   - `matrix.set_state`: write a state event to a granted room;
   - `matrix.send_event`: send a non-message event to a granted room.

   Writing services are limited to the event types the app declares in its package and the user approves at import (for this app: `com.hagency.admin.binding.v1` and `com.hagency.engagement.request.v1`). Rooms an instance creates are granted to that instance. The reception room is granted explicitly. These services join the shared A2App Matrix contract, with argument limits and tests.

3. **Hagency removes the two backend steps.**
   - The fleet's representative accepts an invitation to a project room from the room's owner, the same trust rule agents follow (ADR-187 §A.5). Palpo's backend no longer has to join it.
   - Hagency publishes the fleet's resource catalog as a state event in the reception room, in addition to Palpo's API, so any member's Matrix client can read it. The published data is what Palpo shows today: the resource ID, model, reasoning effort, roles and ceilings. It never includes credentials or private configuration.

4. **Verification does not change.** Hagency accepts a request only if `verify_request` holds, whichever client made it. A mini app gets no authority Hagency did not already grant to the owner's own session.

## Security

- The mini app acts only as the signed-in owner, with the services and rooms the owner granted.
- The new writing services cannot write arbitrary event types; each app declares its types and the user approves them.
- The representative's auto-join is limited to invitations from a project room's owner, for rooms that carry a binding to this fleet.
- The published catalog is the same information Palpo already shows its users.

## Consequences

- Owners request agents without leaving Rinx, and the same app works in OctoSense.
- The request flow evolves with Hagency, through App Hub releases, without Rinx releases.
- Rinx's mini-app API grows four general services that other apps can use under the same limits.
- Palpo's web admin remains a supported path.

## Slices

1. Hagency: representative auto-join for project-room invitations; catalog published as reception-room state.
2. Rinx: the four services, declared event types, the created-room lease rule, and contract tests.
3. The "Hagency agents" mini app and its App Hub entry.
4. A live run: an owner requests an agent from Rinx, the operator approves, and the agent's DM arrives.

## Alternatives considered

- **Build the flow into Rinx.** It works and needs no new mini-app services, but it puts Hagency-specific logic in Rinx's core and ties every change to a Rinx release.
- **Keep Palpo's web admin as the only path.** It keeps owners leaving their Matrix client for a step their client could do.
- **Give mini apps unrestricted state and event writing.** Rejected: declared event types keep a granted app from writing state it was not reviewed for.
