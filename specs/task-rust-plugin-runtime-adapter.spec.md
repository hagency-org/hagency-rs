spec: task
name: "Run qualified language-neutral plugin runtimes behind the native runner seam"
inherits: project
satisfies: [REQ-NATIVE-PLUGIN-SYSTEM]
tags: [active, rust, plugins, runtime, qualification]
---

## Intent

Generalize the runner seam with exact plugin runtime identity and a bounded
newline-delimited `hagency.runtime/v1` protocol while retaining Hagency
approval, workspace, usage, cleanup, and settlement authority.

## Constraints

### Must
- Add exact `runtimePlugin` identity to resources, conflicts, claims, catalog,
  publication, and dispatch checks.
- Admit only an enabled plugin/package/model/framework/provider/reasoning row
  whose qualification was executed and passed by Hagency.
- Use string IDs, 1 MiB frames, 64 pending requests, protocol-only stdout, and
  bounded stderr/exit identity.
- Support only `initialize`, `turn/start`, `approval/respond`, `tool/result`,
  `turn/cancel`, and `shutdown`, plus fixed plugin notifications.
- Send `turn/cancel` on cancellation, wait for `shutdown`, and treat response or
  cleanup uncertainty as `plugin_outcome_unknown`.
- Route approval requests through the existing approval coordinator, tool calls
  through the current task-helper catalog, progress through the activity path,
  and usage through `usage.rs` with exact-or-unknown counters.
- Accept final text only from one matching `turn/result`; progress text is never
  a final result. Map the result and observed cleanup into ordinary Hagency
  settlement.
- Qualify a fresh temporary workspace with initialization, marker turn,
  approval, host-tool round trip, usage/unknown usage, cancellation, whole-tree
  cleanup, and fresh-process restart evidence. Derive the evidence digest in
  Hagency.

### Must Not
- Do not expose operator tokens, provider keys, runner HTTP secrets, store/API
  access, or direct task/file authority.
- Do not silently convert old built-in runtime blocks or replace a changed
  adapter. A stale qualification refuses only plugin-backed resources.

## Acceptance Criteria

Scenario: Runtime frames are bounded and typed
  Test: native_plugin_runtime_frames_validate_protocol_and_ids
  Given valid, malformed, oversized, unknown, and duplicate frames
  When they are decoded
  Then only the fixed protocol forms pass and invalid attempts fail closed

Scenario: Resource identity and qualification are exact
  Test: native_plugin_runtime_qualification_gates_resource_claims
  Given passing, stale, disabled, missing, and digest-changed rows
  When resources are offered or claimed
  Then only the exact enabled passing row is admitted and built-ins remain available

Scenario: A qualified runtime settles normally
  Test: native_plugin_runtime_dispatch_settles_through_existing_path
  Given a reference runtime accepting `PLUGIN_INPUT`, emitting usage, and returning `PLUGIN_OK`
  When a dispatch runs
  Then the ordinary reply, usage, cleanup, and settlement records complete

Scenario: Cancel and unknown cleanup stay unknown
  Test: native_plugin_runtime_cancel_and_cleanup_unknown_refuse_settlement
  Given a cancellation, timeout, lost response, or surviving child
  When the runtime attempt ends
  Then the host sends shutdown, observes cleanup, and refuses success when proof is incomplete

## Out of Scope

The separate `hagency.harness/v1` contract defines host-managed views and
controls. Herdr is not required to qualify or run a plugin runtime.
