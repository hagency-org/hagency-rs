spec: task
name: "Apply a Rinx coordinator decision inside a bounded Hagency engagement"
inherits: project
satisfies: [REQ-RUST-MIGRATION-EXECUTION]
tags: [active, rust, palpo, allocation, approvals]
---

## Intent

Implement the native portion of ADR-191 and Rinx ADR 0011. These tests establish
Rust domain, import, console and transport behavior, not a completed mini-app or
live deployment.

## Constraints

- Current registration, delegation, project, resource, Matrix evidence and amount
  are checked under the original domain writer.
- One coordinator decision reserves and provisions; no additional console verdict.
- Parent/engagement/agent commitments are bounded without double charging.
- Retirement never releases unmeasured consumption. Top-ups cannot grow a parent.
- Separate engagements retain independent profile and transport custody.
- Acknowledgment and runtime readiness remain separate observations.

## Acceptance Criteria

Scenario: Owner setup retries its original association and refuses another installation's profile
  Test: native_owner_association_retries_frozen_intent_and_refuses_foreign_profile
  Level: integration
  Test Double: local Matrix and Palpo HTTP fixture; real private native setup state
  Given an authenticated owner initiates an engagement from native Hagency setup
  When the response is lost or an imported profile changes the runtime or coordinator
  Then the request retries unchanged and only the matching local association can be imported

Scenario: One coordinator approval schedules native provisioning
  Test: coordinator_approval_reserves_and_provisions_without_a_console_decision
  Given a verified engagement, eligible manager, ready project and current Matrix proof
  When the authorized coordinator command is replayed across a restart
  Then exactly one allocation and provision effect remain and a console bypass is refused

Scenario: Parent and child capacity cannot be overdrawn
  Test: contribution_and_agent_reservations_cannot_exceed_parent_or_child
  Given an engagement contribution and child allocations
  When another allocation exceeds the contribution or parent ceiling
  Then it is refused without double charging and retirement cannot refund unknown spend

Scenario: A token top-up applies atomically once
  Test: coordinator_top_up_is_atomic_replayable_and_cannot_grow_the_engagement_pool
  Given a current agent and frozen expected allocation
  When a delegated top-up arrives or is replayed
  Then only its bounded increase is committed and stale amounts are refused

Scenario: Profile import cannot claim connection proof
  Test: profile_import_is_atomic_and_native_probe_is_required_before_contribution
  Given an imported registration and delegation
  When a changed coordinator reuses its revision or a connection has not been proven
  Then the import is atomic and resource contribution remains unavailable

Scenario: Two engagements on one server retain independent credentials
  Test: native_palpo_import_route_saves_the_owner_download
  Level: integration
  Test Double: real local Salvo routes and SQLite stores; outbound connection disabled
  Given the authenticated resource owner's console
  When two profiles for one hostname are imported
  Then both registrations and their separate private credentials remain available

Scenario: Owner contribution HTTP calls enforce capacity and binding
  Test: native_console_engagement_contribution_reserves_capacity_and_rejects_cross_binding
  Level: integration
  Test Double: real local console router and writer; seeded owned resource
  Given a verified delegated engagement
  When the owner contributes, retries or requests too much
  Then matching commands are idempotent and cross-binding or excess allocation is refused

Scenario: Lost projection acknowledgment cannot erase a later top-up
  Test: native_coordinator_publication_replays_frozen_grant_and_keeps_a_later_top_up
  Level: integration
  Test Double: local HTTPS peer; real native adapter, domain and custody writers
  Given a frozen resource projection with a lost response
  When a newer grant is queued before retry
  Then the original bytes replay first and the newer projection is subsequently delivered

Scenario: Status paging includes later agents
  Test: fleet_status_pages_reach_agents_after_the_first_hundred
  Given more than one hundred native agents in a registration
  When the publisher enumerates bounded registration-scoped pages
  Then every agent is included exactly once and another engagement has no rows

Scenario: Status pages advance only on matching acknowledgment
  Test: native_palpo_status_pages_wait_for_their_exact_publication_receipt
  Given a bounded current status page
  When an old publication receipt arrives
  Then the current page remains until its own exact receipt is accepted
