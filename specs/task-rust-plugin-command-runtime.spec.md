spec: task
name: "Run language-neutral plugin commands under bounded guardian custody"
inherits: project
satisfies: [REQ-NATIVE-PLUGIN-SYSTEM]
tags: [active, rust, plugins, security, process]
---

## Intent

Run startup, action, event, tool, runtime, and harness commands as bounded
out-of-process children without leaking Hagency authority or losing cleanup
custody.

## Constraints

### Must
- Enforce sixteen global and two per-plugin slots, bounded logs, fixed
  deadlines, and serial build commands. Excess work returns `plugin_busy` rather
  than blocking an HTTP worker.
- Inject only `PATH`, `HAGENCY_PLUGIN_ID`, `HAGENCY_PLUGIN_ROOT`,
  `HAGENCY_PLUGIN_CONFIG_DIR`, `HAGENCY_PLUGIN_STATE_DIR`,
  `HAGENCY_PLUGIN_ENTRYPOINT_ID`, permitted workspace, and one bounded capability
  JSON value. Clear inherited environment and reject command path traversal,
  symlinks, and capability-incompatible argv at invocation.
- Keep stdout protocol-only, bound JSON/frame sizes, and retain bounded
  stdout/stderr logs without treating event stdout as authority.
- Launch through `hagency-platform::SupervisedProcess`/`Launch`, not a direct
  child. Success requires leader exit and observed whole-tree stop.
- Treat timeout, cancellation, parent exit with a surviving descendant,
  guardian loss, and unreadable exit state as `plugin_outcome_unknown`.
- Run startup once after listener readiness and once only when an exact revision
  becomes active; run actions asynchronously with durable idempotency and poll;
  publish events only after canonical post-commit transitions.
- Deliver redacted events through a 64-entry lossy queue and report
  `events_lost`. Keep harnesses guardian-owned and panes declarative.

### Must Not
- Do not host plugin HTML/JavaScript or a plugin PTY.
- Do not let event results mutate canonical task state or let a process result
  substitute for Hagency settlement.

## Acceptance Criteria

Scenario: Environment and protocol are allowlisted
  Test: native_plugin_command_environment_is_allowlisted
  Given inherited secrets and a valid action
  When the guardian launches it
  Then only the fixed environment and bounded JSON input are visible

Scenario: Scheduler pressure is explicit
  Test: native_plugin_scheduler_enforces_global_and_plugin_limits
  Given commands exceeding both limits
  When they are submitted
  Then excess work receives `plugin_busy` without starting

Scenario: Timeout keeps custody
  Test: native_plugin_timeout_requires_whole_tree_cleanup
  Given a command with a detached child
  When its deadline expires
  Then cleanup is proven or the outcome is `plugin_outcome_unknown`, never success

Scenario: Events are redacted and lossy
  Test: native_plugin_event_queue_redacts_and_reports_loss
  Given more events than the queue can retain
  When they are delivered
  Then only fixed redacted fields are sent and `events_lost` is recorded

## Out of Scope

Approval routing and runtime JSON-RPC semantics are covered by the tools and
runtime-adapter contracts.
