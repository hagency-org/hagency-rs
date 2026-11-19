spec: task
name: "Run a generic Hagency-managed harness view"
inherits: project
satisfies: [REQ-NATIVE-PLUGIN-SYSTEM]
tags: [active, rust, plugins, harness, view]
---

## Intent

Define the generic `hagency.harness/v1` contract. Hagency starts and supervises a
harness plugin that receives redacted state and requests only host-owned
controls. The harness is a constrained UI/view adapter, distinct from the
runtime plugin that executes the agent.

## Constraints

### Must
- Exchange host lifecycle messages `initialize`, `agent/prepare`, `agent/release`,
  `snapshot`, `event`, and `shutdown`.
- Limit harness-originated controls to `stop_attempt` and `open_console`; return
  Hagency receipts and route controls through existing host paths.
- Keep snapshots/events redacted to `{kind, opaqueId, state, atMs}` and bound
  event delivery to 64 entries with `events_lost` on overflow.
- Sequence snapshots/events monotonically, require a fresh snapshot after
  overflow/reconnect, and mark the view unknown on disconnect.
- Keep `pane.v1` declarative metadata owned by its named harness. Hagency sends
  definitions to that harness but never hosts plugin HTML, JavaScript, or a PTY.
- Take `harnessPlugin` only from the resource's exact identity; do not duplicate
  it under `payload.agentDefinition`.
- Start/stop harnesses under the guardian and close them before service/store
  shutdown. The runtime plugin never receives a harness executable/path.

### Must Not

- Do not submit prompts, collect runtime replies/usage, answer approvals, or mark
  tasks complete through a harness.
- Do not infer canonical state from terminal text or let a harness result settle
  work. Managed prepare/release stays bound to the frozen template; runtime
  adapters remain responsible for agent turns.

## Acceptance Criteria

Scenario: Harness protocol frames are typed and bounded
  Test: native_harness_frames_round_trip_view_messages_and_requests
  Given valid view and host-control messages
  When frames are decoded and encoded
  Then only bounded protocol messages are accepted

Scenario: Harness package templates are validated
  Test: native_plugin_harness_templates_resolve_defaults_and_reject_invalid_values
  Given an enabled harness package with declared settings
  When Hagency resolves a per-agent template
  Then only valid values pinned to the selected package are accepted

Scenario: Event delivery is bounded and redacted
  Test: native_plugin_event_queue_redacts_and_reports_loss
  Given more events than the fixed queue capacity
  When the host drains the event queue
  Then it returns only redacted events and reports the lost count

## Out of Scope

Herdr-specific CLI mapping and managed-agent prepare/release are optional adapter
behavior in `hagency-plugin-examples`. The core harness contract cannot submit
agent turns; generic tests require no Herdr installation or provider login.
