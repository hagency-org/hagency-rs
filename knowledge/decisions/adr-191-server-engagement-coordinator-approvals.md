---
kind: decision
id: ADR-191
title: "A delegated Matrix coordinator approves projects, agents and top-ups within a Hagency engagement"
status: Accepted
satisfies: [REQ-RUST-MIGRATION-EXECUTION]
tags: [native, rinx, palpo, engagements, allocation, approvals]
---

## Context

[Rinx ADR 0011](https://github.com/hagency-org/Rinx/blob/main/docs/adr/0011-hagency-server-engagements.md)
defines the operator's revised workflow. Hagency owns the resources. Its owner
may establish several independent engagements with project Matrix servers,
including several on one hostname. The designated Matrix administrator approves
the association and issues the profile. A coordinator assigned to that engagement
then approves project requests. That coordinator or the resource owner approves
agent and token requests in Rinx. The Rinx approval must be sufficient: asking
for another approval in the Hagency console would split one decision across apps.

This supersedes ADR-190's proposed direct-request flow and its implicit local
console approval. It narrows ADR-186's console allocation route only for explicitly
configured coordinator engagements; existing legacy registrations retain their
existing workflow until reconciled.

## Decision

1. A server engagement is a registration ID, not a hostname. Its owner,
   coordinator, registration generation, delegation revision and expiry are
   explicit. An owner import atomically binds the registration and delegation.
   A downloaded `verified` label is insufficient: this installation's native
   Matrix probe must establish the connection before resources can be allocated.
2. Keep the first imported profile at its legacy location. Additional profiles
   live under `palpo-engagements/<fleet-id>/`, with independent machine tokens,
   App Service credentials, transport custody bindings, SDK stores and workers.
   Replacing one engagement closes its original workers before replacing them;
   it does not replace another engagement on the same server.
3. The owner reserves a bounded part of an owned resource for an engagement.
   Only its eligible project managers may request use. The contribution is
   charged once at the parent; agent allocations draw from that reserved pool.
   Increasing a contribution is allowed within current parent headroom. An
   agent approval or token top-up never enlarges its contribution implicitly.
4. A project command must come from its engagement coordinator. An agent or
   top-up command may come from that coordinator or the resource owner. Palpo
   authenticates the human and emits the exact command; Hagency validates its
   transport, current delegation, generations, frozen request, project, resource,
   amount and fresh Matrix evidence before committing.
5. The one accepted agent command atomically reserves capacity, writes the native
   agent record and schedules normal provisioning. Replays return its original
   receipt. Console approval and allocation mutations cannot bypass the delegated
   workflow. Project readiness requires actual room membership and privacy facts.
6. Resource grants, project state and execution receipts use the durable outbound
   publisher. Lost responses replay the original frozen bytes. Acknowledging an
   older projection never deletes a newer queued version. Delivery acknowledgment,
   committed approval, provisioning and a live joined agent remain separate states.
7. Native agent rows are the source for the Hagency portal and Palpo observations.
   Status publication pages through each engagement and advances only after the
   exact page is acknowledged. Token consumption carries its measurement time,
   evidence and completeness. Unknown consumption is not zero; ending an agent
   cannot release unknown or unsettled spend. Job summaries are future work.

## Implemented integration boundary

The current Rust slice adds schema version 63, typed coordinator commands,
transactional reservations/top-ups, owner contribution HTTP routes, independent
profile import/launch, native Matrix verification and outbound projections.
`GET /console/api/server-engagements` and its `/{id}/resources` routes expose the
safe profiles and contributions; authenticated `PUT` reserves or increases one.
The resource configuration permission is retained through the writer commit.

The Palpo counterpart is [PR #508](https://github.com/palpo-im/palpo/pull/508).
It uses the same pinned Rust contract; the Hagency binary does not invoke a
JavaScript backend. Build and test scripts ending in `.mjs` remain development
tools. Existing web presentation assets are not execution or approval authority.

## Native owner association setup

An initialized installation can initiate an association with the owner's existing
Matrix account. The token stays in an owner-private file; it is not stored in the
intent or returned to the mini app.

```sh
hagency association --state-dir /private/hagency \
  --palpo-origin https://operations.example.org \
  --homeserver https://matrix.example.org \
  --matrix-token-file /private/owner-matrix.token \
  --request-id association_one --name "Owner Hagency" \
  --coordinator @coordinator:example.org \
  --export-mxid @owner:example.org \
  --delegation-expires-at-ms <explicit-owner-deadline-ms>
```

The native setup checks the Matrix owner and coordinator account, persists its
runtime identity and frozen intent, then submits the request. An identical retry
recovers the same request; changed content needs another request ID. The
designated admin decides once in Rinx. Import of the versioned approved profile
must match this installation's pending runtime, server/origin, owner, coordinator
and delegation policy. Legacy downloads remain supported during migration.
The runtime advertises its implemented coordinator capability; importing a
profile never manufactures a successful connection proof.

The Rinx `native_palpo_association.py` harness passed eight checks using actual
Rinx, Rust Palpo and Hagency processes against an isolated Matrix HTTP fixture.
It drives the native admin decision and owner connection button, imports through
the Hagency CLI and waits for the real worker's authenticated probe receipt.
It does not claim live Matrix, agent execution or native save-dialog acceptance.

## Remaining acceptance work

- Semantic legacy reconciliation and production cutover.
- Live Matrix notification acceptance and final command outcomes
  for authority expiry or permanently invalid room bindings.
- Isolated combined Palpo/Rinx/Hagency tests, real Makepad instrumentation and
  live chat/usage evidence before production cutover.

Passing domain, HTTP and transport tests does not establish those user-facing
acceptance results. Existing production state and services are unchanged.

The native portal now has Server engagements and Agent allocations pages.
Resources and Server engagements edit the same contribution record and revision.
The delivered-decision registry precedes Matrix admission, preserves refusals and
replays after restart, and exposes no private agent instructions. The Chrome
acceptance test operates the actual native console, edits one allocation twice,
checks the shared resource view and observes a refused delivered approval.

The owner can now change the coordinator, expiry, self-approval policy and explicit
profile recipients, or suspend/revoke the delegation. The console checks a new
active coordinator against Matrix; suspension/revocation also work while Matrix
is offline. A finite owner permission survives queueing through the commit.
The authority revision and publication commit together, fencing old commands
locally immediately. Palpo accepts that revision only from the engagement's
authenticated transport, refreshes reviewers/notices and preserves existing
grants. It cannot use this publication to assert connection proof or rotate
registration identity. Reimport must match the locally accepted delegation;
an old profile cannot restore a removed coordinator. Browser acceptance covers
the owner suspension form, pending synchronization and unchanged allocation.
