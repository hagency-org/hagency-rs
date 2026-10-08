spec: task
name: "Run Octos dispatches over OUP behind the native runner seam"
inherits: project
satisfies: [REQ-RUST-MIGRATION-EXECUTION]
tags: [active, rust, octos, oup, runtime, metering]
---

## Intent

ADR-193 slice 1: a third driver behind the runner seam. One `octos serve
--stdio` per dispatch speaks OUP: a fixed handshake, the permission profile, a
fresh session on the workspace and one turn, then every event until Octos
reports the session idle. The reply is the last turn's text and usage reaches
the ledger with framework `octos`. Approvals, host task tools, Setup and the
live qualification are later slices.

## Constraints

### Must
- Send string request IDs only and refuse any other answer ID or a server request.
- Negotiate projection v2, typed approvals and the workspace cwd, never user questions.
- Set `workspace_write`, network denied and on-request approvals before `session/open`; never `danger_full_access` or `never`.
- Require Octos to bind exactly the dispatch's workspace and the profile the resource names.
- Track the dispatch's turn and every turn Octos announces; a background child stream is not a turn.
- End only when Octos is idle: every known turn ended and no work reported, or a quiet window with no report.
- Drop redelivered envelopes by thread and sequence; a second terminal never replaces the first.
- Take the reply as the last turn's last persisted text after its last tool start.
- Record each terminal's usage as it arrives and the session totals at idle; absent usage is unknown, never zero.
- Remove provider keys from the launch, set `OCTOS_NO_MODEL_DOWNLOAD=1`, and give each agent its own private instance directory.
- Refuse an Octos approval or host tool call until those slices land, stopping the child.

### Must Not
- Do not handle Octos credentials, write into the user's Octos profiles or run a provider in ordinary tests.
- Do not change Codex or Claude Code behaviour, or decide a dispatch done on the bridge side.

## Boundaries

### Allowed Changes
- native/hagency-runtime/src/octos.rs
- native/hagency-runtime/src/octos/**
- native/hagency-runtime/src/owned.rs
- native/hagency-runtime/src/owned/octos.rs
- native/hagency-runtime/src/lib.rs
- native/hagency-runtime/src/bin/hagency-runtime-probe.rs
- native/hagency-runtime/src/bin/octos_probe/mod.rs
- native/hagency-runtime/tests/octos.rs
- native/hagency-runtime/tests/octos_owned.rs
- native/hagency-metering/src/lib.rs
- native/hagency-metering/src/observation.rs
- native/hagency-metering/src/octos_usage.rs
- native/hagency-metering/tests/octos_usage.rs
- native/hagency-core/src/project.rs
- native/hagency-store/src/domain.rs
- native/hagency-store/src/domain/accounts.rs
- native/hagency-store/src/domain/execution.rs
- native/hagency-store/src/domain/usage.rs
- native/hagency-store/src/migrations/069-octos-usage-sources.sql
- native/hagency-store/tests/owned_claim.rs
- native/hagency-store/tests/usage.rs
- native/hagency-execution/src/host.rs
- native/hagency-execution/src/lib.rs
- native/hagency-execution/src/operation.rs
- native/hagency-execution/src/usage/capture.rs
- native/hagency-execution/tests/owned.rs
- native/hagency-execution/tests/owned/octos.rs
- specs/task-rust-octos-runner-seam.spec.md

## Acceptance Criteria

Scenario: The OUP codec answers string IDs and reads only what the driver needs
  Test: native_octos_frames_answer_string_ids_only
  Level: unit
  Test Double: fixed OUP frames
  Given answers with string, numeric and empty IDs and a server-sent request
  When frames decode
  Then only string IDs answer, and a server request or malformed frame is refused

Scenario: Envelopes keep their routing, terminals their outcome and usage
  Test: native_octos_frames_read_the_projection_the_driver_needs
  Level: unit
  Test Double: fixed OUP frames
  Given persisted text, terminals with and without usage, an unknown outcome and future payloads
  When frames decode
  Then usage is exact or unknown, an unknown outcome refuses the turn and other payloads are ignored

Scenario: Requests and the launch arguments are bounded
  Test: native_octos_requests_and_launch_arguments_are_bounded
  Level: unit
  Test Double: none
  Given request IDs, parameters, profile IDs, session keys and paths
  When requests and serve arguments are built
  Then only Hagency's own forms pass and the argv is the fixed private stdio serve

Scenario: The handshake is fixed
  Test: native_octos_owned_session_sends_the_fixed_handshake
  Level: integration
  Test Double: offline OUP peer through the original guardian
  Given an owned session on the offline peer
  When it says hello, opens and runs one turn to idle
  Then every request carries a string ID, no user question is offered and the permission profile precedes the session

Scenario: Usage is recorded per terminal and closed by the session totals
  Test: native_octos_owned_session_reports_usage_per_turn_and_at_idle
  Level: integration
  Test Double: offline OUP peer through the original guardian
  Given one turn with exact usage and session totals that cover it
  When the session runs to idle
  Then one usage observation follows the terminal and the idle observation carries the totals

Scenario: Background work runs until Octos is idle
  Test: native_octos_owned_background_work_runs_until_octos_is_idle
  Level: integration
  Test Double: offline OUP peer with a sub-agent and a continuation turn
  Given a sub-agent that outlives the turn and a kernel continuation turn
  When the session runs
  Then the child stream is not a turn, the continuation is, and the reply and totals are the last turn's and the session's

Scenario: A quiet session ends after the quiet window
  Test: native_octos_owned_quiet_session_is_idle_after_the_quiet_window
  Level: integration
  Test Double: offline OUP peer that reports no orchestration
  Given a turn too short for any report
  When the session runs
  Then it is idle only after the quiet window

Scenario: Redelivery and a second terminal change nothing
  Test: native_octos_owned_redelivery_and_a_second_terminal_change_nothing
  Level: integration
  Test Double: offline OUP peer redelivering envelopes
  Given a redelivered tool start and a second terminal for the same turn
  When the session runs
  Then the reply and the first terminal stand

Scenario: Errors, unknown usage and lagging totals are kept as they are
  Test: native_octos_owned_errored_terminal_keeps_its_code
  Test: native_octos_owned_absent_usage_stays_unknown
  Test: native_octos_owned_lagging_totals_keep_the_turn_sums
  Level: integration
  Test Double: offline OUP peer
  Given an errored terminal, absent usage and totals behind the turns
  When the session runs to idle
  Then the error code is kept, unknown stays unknown and the turn sums are kept

Scenario: Approvals reach the host and bad peers close the session
  Test: native_octos_owned_approval_reaches_the_host
  Test: native_octos_owned_refusals_and_bad_peers_close_the_session
  Level: integration
  Test Double: offline OUP peer
  Given an approval request, a refused open, a malformed and a stalled peer
  When the session runs
  Then the approval is an event and every failure closes the session

Scenario: An Octos dispatch completes at idle with its reply and usage
  Test: native_octos_dispatch_completes_at_idle_with_its_reply
  Test: native_octos_background_work_settles_with_the_last_turns_reply
  Level: integration
  Test Double: offline OUP peer launched by the Host and the actual domain writer
  Given an Octos resource naming its profile and a host environment with provider keys
  When the dispatch runs
  Then the launch is the fixed serve with no provider key, the reply settles the task and usage is recorded as octos

Scenario: Failures and refusals settle as they do for the other runners
  Test: native_octos_errored_idle_settles_as_a_protocol_failure
  Test: native_octos_approval_without_cards_refuses_the_dispatch
  Test: native_octos_host_and_codex_host_refuse_each_others_dispatch
  Test: native_octos_resource_names_its_profile
  Level: integration
  Test Double: offline OUP peer launched by the Host
  Given an errored turn, an approval, another runner's dispatch and resources with and without a profile
  When the dispatch runs or the resource is checked
  Then each is a failure, a refusal by name or an invalid resource

Scenario: The ledger and the claim take Octos
  Test: native_metering_octos_runtime
  Test: native_usage_octos_runtime_source
  Test: native_usage_migration_admits_octos_sources
  Test: native_owned_claim_profile_claims_an_octos_dispatch
  Level: integration
  Test Double: actual domain repository
  Given Octos runtime evidence, a v68 ledger and an Octos provision
  When usage is recorded, the store migrates and an agent claims
  Then evidence is octos-only, every source and receipt survives and the dispatch is claimed

## Decisions

ADR-193 defines this slice. Octos approvals become owner cards in slice 2, task
tools are host tools in slice 3, and Setup, the `local_octos` binding and the
fleet's Octos runtime come in slice 4.
