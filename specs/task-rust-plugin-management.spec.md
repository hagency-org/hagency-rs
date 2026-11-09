spec: task
name: "Manage native plugins through the operator service and console"
inherits: project
satisfies: [REQ-NATIVE-PLUGIN-SYSTEM]
tags: [active, rust, plugins, console, cli]
---

## Intent

Expose plugin lifecycle through one running Hagency service, its authenticated
loopback CLI, operator routes, and a read-only host-authored console.

## Constraints

### Must
- Provide list, inspect, preview, install, link, enable, disable, uninstall,
  action, invocation polling, logs, config-dir, and service-run qualification.
- Route every mutation through the operator bearer-token service. CLI commands
  MUST NOT instantiate `PluginManager`, `DomainRepository`, or another registry.
- Return HTTP 202 for accepted asynchronous work and stable codes including
  `plugin_busy`, `plugin_outcome_unknown`, `plugin_restart_required`,
  `plugin_stale`, `plugin_disabled`, `plugin_invalid`, and `plugin_unavailable`.
- Show source commit/digest, active and retained revisions, capabilities,
  readiness, leases, stale/restart state, qualification verdicts, invocation and
  bounded log state, and the config directory.
- Require structured GitHub `{owner, repo, subdir?, commit}` input and exact
  trust confirmation before installation.
- Never render plugin HTML/JavaScript or host a plugin PTY.

### Must Not
- Do not write plugin state offline or merge it into `fleet-runtime.json`.
- Do not let plugin metadata disable or hide built-in resource editing.

## Acceptance Criteria

Scenario: CLI mutations use service authority
  Test: native_plugin_cli_uses_operator_routes_and_exact_subcommands
  Given a running private service and its operator token
  When list/inspect/preview/install/link/enable/disable/uninstall/config-dir are invoked
  Then requests use the token, the service owns the mutation, and an offline registry writer is not opened

Scenario: Asynchronous actions are durable and pollable
  Test: native_plugin_action_returns_accepted_invocation_and_poll_state
  Given an enabled action entry
  When the CLI submits the action and polls its invocation
  Then it receives HTTP 202 followed by a durable terminal state or `plugin_outcome_unknown`

Scenario: Trust confirmation is exact
  Test: native_plugin_install_requires_exact_trust_confirmation
  Given interactive and non-TTY install requests
  When the source digest is absent or mismatched
  Then no build starts and non-TTY apply requires explicit `--yes`

Scenario: Console renders data only
  Test: native_console_plugins_renders_bounded_state_without_plugin_markup
  Given plugin records, preview, logs, leases, stale state, and restartNeeded
  When `/plugins` is opened
  Then only validated host-authored data is rendered

Scenario: Runtime changes request restart
  Test: native_plugin_runtime_changes_return_restart_needed
  Given a warm host using an enabled runtime adapter
  When the registry switches its active revision
  Then the response reports `restartNeeded` and built-in agents remain ready

## Out of Scope

Runtime protocol, tools, harness control, and qualification execution are bound
by the other plugin contracts.
