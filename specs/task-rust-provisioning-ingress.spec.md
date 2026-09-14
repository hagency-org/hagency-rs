spec: task
name: "Wire the production provisioning ingress to admit engagements"
inherits: project
satisfies: [REQ-RUST-MIGRATION-EXECUTION]
tags: [active, rust, provisioning, ingress, admission, matrix]
---

## Intent

Bind the production provisioning ingress the store already assumes: the
native product must be able to provision an engagement the way the retained
product does. Today `DomainRepository::admit` is the only engagement-minting
write, but no production code calls it or `verify_request` — every native
agent in every test was admitted by a fixture. This slice wires the Matrix
intake admission chain (owner message → intake admit → provider approval →
effect observed) to `verify_request` and `admit`, exactly as ADR-095's
2026-09-14 amendment decides.

## Constraints

### Must
- Observe the provisioning request through the production Matrix ingress: the `com.hagency.engagement.request.v1` event carried by the fake peer as the retained product sends it (the bridge passes the Matrix event id as the request id).
- Verify it with the same `verify_request` (`hagency-core/src/authority.rs:196`) — the event-type, source-event, room and sender checks already there — so an unverified request is refused **before** `admit`.
- Call `DomainRepository::admit` exactly once for a verified request — the single minting write — and observe the provider approval as the separate `approve` verdict, never folded into the mint.
- Refuse a duplicate request (same `source_event_id`) by the same id before any second INSERT.

### Must Not
- Do not mint an engagement outside `admit` — no second INSERT, no direct roster write, no fixture-style seeding in the production path.
- Do not change `admit`, `verify_request`, the `VerifiedRequest` shape or any store schema; this slice wires the production caller, it does not move the write.
- Do not add a console create-agent HTTP route — the ingress is Matrix intake, not a REST endpoint (the console `agents.rs` stays GET + `{id}/start|stop|preset`).
- Do not gate any scenario by OS or feature in the binding set.

## Boundaries

### Allowed Changes
- native/hagency-matrix/src/intake.rs
- native/hagency-matrix/src/collector.rs
- native/hagency-matrix/src/collector/observation.rs
- native/hagency-matrix/src/lib.rs
- native/hagency/src/bootstrap.rs
- native/hagency/src/lib.rs
- native/hagency-store/src/domain/verified_ingress.rs
- native/hagency-store/src/domain_worker.rs
- native/hagency-matrix/tests/
- native/hagency/tests/
- native/fixtures/
- specs/task-rust-provisioning-ingress.spec.md
- knowledge/decisions/adr-095-native-state-ownership.md
- docs/progress.md

### Forbidden
- Live services, live homeservers, credentials, deployed state.
- native/hagency-store/src/domain.rs (admit is unchanged); native/hagency-core/src/authority.rs (verify_request is unchanged); native/hagency/src/console/**.

## Acceptance Criteria

Scenario: A provider-approved request provisions an engagement through the production ingress
  Owed Selector: native_provisioning_ingress_admits_a_provider_approved_request (parked — the name is owed by the implementing slice and binds only when it lands; no Test: line here yet)
  Level: integration
  Test Double: the shared fake peer delivering the com.hagency.engagement.request.v1 event as the retained product sends it
  Given a fake peer delivering the request event through the Matrix intake chain
  When the collector verifies the event and the provider approval is observed
  Then admit runs exactly once and the engagement exists with its minted id

Scenario: A duplicate provisioning request is refused by the same id
  Owed Selector: native_provisioning_ingress_refuses_a_duplicate_by_the_same_id (parked — binds with this slice; no Test: line here yet)
  Level: integration
  Test Double: the same event delivered twice with the same source_event_id
  Given a request already admitted for a source_event_id
  When the same event is delivered again
  Then the ingress refuses it by the same id and no second engagement row is minted

Scenario: An unverified provisioning request is refused before admit
  Owed Selector: native_provisioning_ingress_refuses_unverified_before_admit (parked — binds with this slice; no Test: line here yet)
  Level: integration
  Test Double: a request event whose observation fails the verify_request checks
  Given a request whose event, room or sender does not satisfy verify_request
  When the ingress observes it
  Then it is refused before admit runs and no engagement row exists

## Decisions

**The ingress is Matrix intake, not a console route.** There is no native
create-agent HTTP route, and this slice adds none: the product provisions
through the Matrix admission chain, and the store's own `admit`/`approve`
pair is the write surface — this slice only gives them their production
caller.

## Out of Scope

The provider-approval verdict surface itself (already owned by the
approval ADRs), resource/seat allocation decisions (ADR-121/122/123), the
console roster read, and any change to `admit`/`verify_request`.
