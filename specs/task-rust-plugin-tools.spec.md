spec: task
name: "Expose approved language-neutral plugin tools through the task runner"
inherits: project
satisfies: [REQ-NATIVE-PLUGIN-SYSTEM]
tags: [active, rust, plugins, tools, approvals]
---

## Intent

Route enabled `tool.v1` entries through the existing owner approval and runner
capability path while preserving single-launch, durable custody, and unknown
outcomes.

## Constraints

### Must
- Advertise tools as `plugin.<plugin_id>.<tool_id>` beside the existing task
  tools without changing those tools.
- Create an owner approval containing framework, plugin ID, active package
  digest, tool ID, and canonical input digest. Offer only Approve once or Deny.
- Create a DomainStore custody row keyed by dispatch/call ID before launch.
  Replay returns the retained result or unknown verdict and never launches twice.
- After approval acquire the exact revision lease and scheduler slot, then run
  once in the retained dispatch workspace under the guardian.
- Require exactly `{content:[{type:"text",text}],isError}` within the bounded
  response frame and return `plugin_tool_outcome_unknown` on response or cleanup
  uncertainty.
- Denial, expiry, revocation, stale digest, or missing workspace MUST create no
  process and no marker file.

### Must Not
- Do not grant task-scoped or always grants to arbitrary plugin code.
- Do not settle a task from plugin output or retry an uncertain launch.

## Acceptance Criteria

Scenario: A tool is listed and approved through the existing path
  Test: native_plugin_tool_catalog_and_owner_card
  Given an enabled tool entry
  When the runtime requests it
  Then the catalog name and owner card carry exact plugin identity and input digest

Scenario: Denial and expiry do not launch
  Test: native_plugin_tool_denial_and_timeout_do_not_launch
  Given a denied or expired request
  When the runtime waits for the result
  Then no plugin process or marker file is created

Scenario: Replay launches once
  Test: native_plugin_tool_approved_replay_launches_once
  Given an identical approved call envelope
  When the runtime retries it
  Then one process runs and one custody record retains the result

Scenario: Malformed or lost results stay unknown
  Test: native_plugin_tool_malformed_or_lost_result_is_unknown
  Given malformed output, timeout, or a surviving child
  When the tool attempt ends
  Then the stable unknown refusal is returned and the task is not marked done

## Out of Scope

Runtime adapters are covered by the runtime-adapter contract.
