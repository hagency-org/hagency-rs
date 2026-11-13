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
- Bound waits in two separate clocks: a 2 s `response` timeout for each request
  (`initialize`, `turn/start`, `turn/cancel`, `shutdown`) and a 60 s `turn_idle`
  timeout between frames while a turn runs. A runtime keeps a turn alive with
  `progress` notifications at least once per second; progress text is never a
  result. The dispatch budget and deadline still bound the whole turn.
- Give the runtime a non-secret `HOME` (the host process's own) and nothing else
  ambient; the guardian still clears the rest of the environment.
- Send `turn/start.tools` (name, description, inputSchema, readOnly) only when
  the runtime advertised the `tool` capability at `initialize` and the host
  serves a task helper. The catalog is the Octos set (`owned_task_tools`: core
  task tools plus file tools when granted). Coordination tools (`delegate_task`
  and the rest of `COORDINATION_TOOLS`) are NOT offered to plugins until plugin
  approval routing exists, because relaying them would bypass owner approval.
- Execute a `tool/call` once per `call_id` through the same task helper Octos
  uses, on the dispatch's task and capability, and answer with `tool/result`
  `{ok:true, output}` or `{ok:false, error}`. Refuse a duplicate `call_id`, a
  tool outside the offered catalog, and any `tool/call` when no catalog was
  offered. A helper that stopped answering makes later calls fail as outcome
  unknown. Plugin approval requests are still denied.
- Run a runtime's optional manifest `readiness` probe (package-relative argv,
  bounded, read-only, the runtime's own environment, no workspace) before the
  dispatch starts. `{"ready":false,"reason":...}`, malformed output, or a
  timeout requeues the dispatch as never started (`fail_before_start` with a
  backoff, a launch-retry notice); it never fences the dispatch or
  quarantines its session. A runtime without a probe starts as before.
- When a plugin turn completes with plain `finalText` and no
  `complete_task_with_reply` call, the host moves the task to Done and sends the
  text as the room reply (only when the task has a Matrix route).
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

Scenario: A quiet turn is bounded by turn idle, not by the response timeout
  Test: a_quiet_turn_is_bounded_by_turn_idle_not_by_the_response_timeout
  Given a runtime that accepts a turn and stays silent longer than the response timeout
  When the turn runs with a larger turn idle timeout
  Then it is not failed at the response timeout and ends only at turn idle

Scenario: Host tools run once and only from the offered catalog
  Test: host_executes_task_tools_once_and_refuses_duplicates_and_unknown_tools
  Given a runtime that advertised `tool` and received the catalog
  When it repeats a `call_id` and calls a tool outside the catalog
  Then the host executes the first call once and refuses the other two

Scenario: No tool capability means no host tools
  Test: a_runtime_without_the_tool_capability_never_starts_host_tools
  Given a runtime that did not advertise `tool`
  When a turn starts
  Then the host never starts the task helper and sends no catalog

Scenario: Plugin tools are the task helper's own catalog
  Test: native_plugin_tools_relay_to_the_task_helper
  Given the offline task helper
  When a plugin asks for its catalog and calls tools
  Then it gets the Octos set without coordination tools, helper answers and refusals relay unchanged, and outside names never reach the helper

Scenario: The runtime receives HOME and tools only when due
  Test: native_plugin_command_environment_carries_home_but_no_secret
  Given a guardian command environment
  When it is built
  Then `HOME` is present and no secret or ambient variable is

Scenario: A runtime that is not ready defers the dispatch without starting it
  Test: native_plugin_runtime_not_ready_requeues_without_starting
  Given a qualified plugin runtime whose readiness probe answers not ready
  When a dispatch is claimed for it
  Then nothing is launched, the dispatch is queued behind a backoff, the session is not quarantined, and a later ready probe launches it

Scenario: Readiness probes are package-relative and fail closed
  Test: native_plugin_readiness_verdict_is_typed_and_fails_closed
  Given ready, not-ready, malformed and oversized probe answers
  When the host reads them
  Then only an explicit ready starts the dispatch and reasons are bounded

## Out of Scope

The separate `hagency.harness/v1` contract defines host-managed views and
controls. Herdr is not required to qualify or run a plugin runtime.
