---
kind: requirement
id: REQ-NATIVE-PLUGIN-SYSTEM
title: "Run language-neutral capability plugins under Hagency custody"
status: Accepted
satisfies: [REQ-RUST-MIGRATION-EXECUTION]
tags: [native, plugins, security, runtime, harness]
---

## Problem

Hagency needs implementation-language-neutral extensibility without moving task
authority, owner approvals, usage accounting, workspace custody, process cleanup,
or settlement into third-party code. The topology is Rinx (user interaction) →
Palpo (homeserver and fleet transport) → Hagency (local fleet, plugin host,
execution and settlement authority). Plugins extend Hagency only; they are not a
second Matrix, approval, task, usage, or settlement authority.

## Requirements

## [REQ-NATIVE-PLUGIN-SYSTEM-001] Hagency MUST accept only the closed
`hagency.plugin/v1` manifest schema and capability versions `startup.v1`,
`action.v1`, `event.v1`, `tool.v1`, `runtime.v1`, `harness.v1`, and `pane.v1`.
Startup/action/event are one-shot host dispatches; tools are approval-gated;
runtime adapters own admitted agent turns; harnesses are redacted view/control
processes that MAY prepare or release only a template-bound target and MUST NOT
submit turns; panes are declarative metadata owned by a named harness.

[REQ-NATIVE-PLUGIN-SYSTEM-002] Remote installation MUST use a GitHub repository,
an exact lowercase 40-hex commit, an isolated detached checkout, explicit trust
confirmation, and source/package digests. Branch heads, archives, submodules,
symlinks, and a second downloader are forbidden. Linked packages are
authoring-only and are re-snapshotted before invocation.

[REQ-NATIVE-PLUGIN-SYSTEM-003] Plugin commands MUST run out of process under the
Hagency guardian with bounded concurrency, deadlines, output retention,
cancellation, and a whole-tree cleanup verdict. Hagency MUST NOT expose Matrix,
App Service, operator, provider, runner, or credential material to a plugin.

[REQ-NATIVE-PLUGIN-SYSTEM-004] Hagency MUST remain authoritative for task state,
owner approval, workspace access, usage, canonical results, cleanup, and
settlement. Plugin observations and harness views MUST NOT create, approve,
complete, submit agent turns, or settle a task. A harness MAY prepare/release
only the target in its frozen template; runtime plugins own agent turns. Palpo
supplies homeserver/fleet transport and Rinx supplies user interaction; neither
plugin code nor a plugin route receives their credentials.

[REQ-NATIVE-PLUGIN-SYSTEM-005] Runtime adapters MUST use `hagency.runtime/v1`
and exact qualification evidence for plugin, adapter, package, framework, model,
provider, and reasoning. A stale, disabled, missing, or digest-changed adapter
MUST refuse new resources and dispatches without stopping unrelated built-in
runtimes.

[REQ-NATIVE-PLUGIN-SYSTEM-006] Registry writes MUST be service-owned, atomic,
owner-private, and backed by immutable package revisions. Existing active
installs MUST remain usable during failed preview/build/update/cancellation;
obsolete revisions MUST remain readable until their leases reach zero, then only
the exact obsolete managed revision MAY be collected. A registry/domain revoke
mismatch marks the plugin stale and refuses new launches.

[REQ-NATIVE-PLUGIN-SYSTEM-007] The running Hagency service MUST be the sole plugin
owner. CLI, console, and operator routes use its shared registry, scheduler,
leases, invocation table, event queues, startup sessions, harness sessions, and
shutdown; no caller opens a second registry or writes plugin state offline.

[REQ-NATIVE-PLUGIN-SYSTEM-008] Qualification MUST execute the exact active
revision through the normal guardian/session seam, derive its evidence digest in
Hagency, and persist every observed verdict. Caller-authored evidence files or
digests MUST NOT qualify a runtime. Only a fully passing row may publish or claim
a plugin resource.

## Scenarios

Scenario: A plugin is installed only after an exact trust decision
  Given a valid GitHub checkout at a full commit pin
  When the operator previews and applies the install with a matching source digest
  Then the service builds an isolated copy, reparses and hashes it, and atomically activates one immutable revision

Scenario: All management mutations use one service owner
  Given a running private Hagency service
  When CLI, console, or operator routes list, preview, enable, disable, update, or uninstall a plugin
  Then they use the operator token and shared service state, and no offline registry write occurs

Scenario: A plugin cannot bypass Hagency authority
  Given an enabled action, tool, runtime adapter, harness, or pane
  When its command requests work or reports a result
  Then guardian custody, approval, workspace, usage, cleanup, and settlement remain host-owned

Scenario: A failed update preserves leased work
  Given an active revision with a running lease
  When a replacement fails validation, build, revocation, or cleanup proof
  Then the old revision remains readable for the lease and new work is refused or uses the prior active revision with a stable refusal code

Scenario: A stale runtime qualification cannot publish
  Given an exact qualification bound to package and model identity
  When either digest or model tuple changes, or the adapter is disabled
  Then new resource creation and claims are refused while built-in resources remain available

Scenario: Harness control is a redacted view
  Given a running built-in or plugin-backed dispatch
  When Hagency emits state/events or a harness requests `stop_attempt` or `open_console`
  Then Hagency sends only bounded redacted data or a receipt and remains the source of truth

## Dependencies

- REQ-RUST-MIGRATION-EXECUTION
- REQ-OWNER-UI-APPROVAL
- REQ-THREE-LAYER-TASK-COMPLETION

## Source Trace

- approved plan: Plugin Architecture Hardening
- ADR-195

## Open Questions

None for v1. Plugins remain trusted user-level executables with the OS user's
filesystem/network access. The manifest and guardian are lifecycle and authority
controls, not a sandbox; OS confinement requires a separately approved design.
 
## 2026 trust-flow amendment

Remote plugin sources are GitHub-only and may use canonical HTTPS or SSH transport. They require an exact lowercase 40-hex commit and optional safe relative subdirectory. The service normalizes and validates decomposed source fields at every HTTP boundary; it does not accept arbitrary repository URLs, redirects, credentials, branches, tags, archives, submodules, or symlinks.

Preview is read-only and stateless. It returns the normalized source, source digest, manifest digest, and a trust digest over source identity and content. Install resubmits the source and trust digest, performs a fresh isolated checkout, verifies the complete trust contract before build or registry mutation, and preserves the active record or enters explicit `NotReady` on failed recovery. SSH uses only the existing service agent and `known_hosts`.
