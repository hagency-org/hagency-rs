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

Scenario: Scoped agent controls cannot bypass cleanup inspection or replay an old pause
  Test: scoped_agent_controls_recheck_authority_and_preserve_cleanup_and_capacity_on_replay
  Given a coordinated agent controlled by its project owner or current delegate
  When pause, resume or retirement retries after a restart or an uncertain cleanup
  Then the original result survives, unknown cleanup cannot be retried and capacity remains held

Scenario: Final account reconciliation releases only unused capacity
  Test: final_account_settlement_refunds_only_unused_capacity_and_late_usage_remains_charged
  Given a retired agent with measured usage and completed runtime cleanup
  When the resource owner records final account usage then a later usage observation arrives
  Then only unused capacity is returned, late usage remains charged and replay cannot refund twice

Scenario: Project and top-up terminal receipts cannot be revived by later capacity or expiry
  Test: project_and_top_up_refusals_are_terminal_and_applied_commands_replay_after_expiry
  Given an applied top-up and refused project and top-up commands
  When capacity changes or the writer restarts after command expiry
  Then their original outcomes survive without a second reservation

Scenario: The actual native worker reports project and top-up refusals without Matrix availability
  Test: native_palpo_worker_publishes_terminal_project_and_top_up_refusals
  Level: integration
  Test Double: actual native executable and SQLite stores; isolated HTTPS Palpo peer and unreachable Matrix origin
  Given a stale delegation decision and a top-up targeting a missing project
  When the native worker receives their durable deliveries
  Then it persists and publishes the terminal refusals before completing custody

Scenario: Owner delegation revisions fence old commands and survive restart
  Test: owner_delegation_changes_fence_old_commands_and_publish_before_resources
  Given the owner changes the coordinator through a revocable configuration permission
  When an old decision or changed retry arrives
  Then the new revision remains authoritative and is durably published before resource changes

Scenario: Native owner suspension requires authentication and retains contributed capacity
  Test: native_console_delegation_suspend_is_authorized_idempotent_and_does_not_release_capacity
  Level: integration
  Test Double: real native console router and domain writer; isolated registration
  Given a verified association with an engagement resource allocation
  When the owner suspends delegation and retries the same revision
  Then anonymous or conflicting changes are refused and capacity is not released

Scenario: Private protocol JSON is not truncated at the bearer-token file limit
  Test: native_palpo_private_observation_files_survive_beyond_secret_token_size
  Given enough recorded request and probe IDs to exceed 512 bytes
  When the native process reloads its private observation files
  Then old and recent request/probe bindings and a longer appservice credential survive

Scenario: Delivered approvals remain visible before Matrix admission
  Test: delivered_approval_is_visible_before_matrix_admission_and_terminal_refusal_survives_restart
  Given a delivered command whose Matrix admission has not completed
  When the store restarts or its delegation changes
  Then the pending decision remains visible and an authority refusal is durable

Scenario: Capacity refusals and successful commands retain their original result
  Test: capacity_refusal_has_a_durable_receipt_and_applied_delivery_replays_without_reserving_again
  Given concurrent demand for the last available capacity
  When a successful or refused command replays after expiry or an allocation increase
  Then its terminal outcome remains unchanged and no reservation is repeated

Scenario: The actual portal shares one ledger across both resource views
  Test: native_coordinator_ledger_browser_shares_resource_allocation_and_shows_delivered_refusals
  Level: integration
  Test Double: real native console and SQLite writer, isolated seeded registration; Chrome browser
  Given a verified server engagement and a delivered approval refused before provisioning
  When the owner edits its allocation through the server-engagement page
  Then the resource page displays the same ledger and Agent allocations displays the refusal

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
