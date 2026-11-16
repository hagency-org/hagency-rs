---
kind: decision
id: ADR-196
title: "Offer coordination tools to plugin runtimes only through an owner gate"
status: Accepted
satisfies: [REQ-NATIVE-PLUGIN-SYSTEM, REQ-OWNER-UI-APPROVAL]
amends: [ADR-180, ADR-195]
tags: [native, plugins, coordination, approval, runtime]
---

## Context

ADR-195 lets a plugin runtime that advertises `tool` receive the Octos task
catalog (`owned_task_tools`) and relay `tool/call` through a host-owned task
helper. The runtime adapter spec keeps the coordination group (`delegate_task`
and the rest of `COORDINATION_TOOLS`) out of that catalog, because the host
relay has no owner approval: `PluginToolHost::call` executes a call with no
gate, and plugin `approval/request` frames are denied unconditionally.
Relaying coordination tools today would let a third-party runtime create work
for another engagement without the owner.

Two further facts block a host-only flag. The owned helper profile
(`hagency mcp --owned-task-profile`) serves only `owned_task_tools` and refuses
every other name in both `tools/list` and `tools/call`. Codex gets the group
through ADR-180 because its MCP elicitation becomes an owner card; plugins have
no such path.

The operator needs an orchestrator, a coder and a reviewer, each an OMP agent in
a Herdr pane driven by the `herdr-runtime` plugin, where the orchestrator
delegates by `delegate_task` and not by Matrix mention.

## Decision

1. **Owner gate on the relayed call.** A plugin `tool/call` for any coordination
   name except `comment_task` raises an owner approval before the task helper is
   called. `comment_task` stays pre-approved as in ADR-180 (ADR-021). The other
   seven names (`delegate_task`, `open_conversation`, `get_conversation`,
   `update_conversation_members`, `close_conversation`, `send_peer_message`,
   `read_peer_inbox`) are never pre-approved.
2. **Approval method.** `hagency-core` adds `PLUGIN_TOOL_METHOD = "plugin/tool"`
   with params `threadId`, `turnId`, `itemId`, `toolName`, `input`. It carries no
   reusable scope, so the owner card offers `Approve once` and `Deny` only; there
   is no task-level or always button.
3. **Gate sequence.** The existing owner-approval store protocol is reused
   without a runtime frame: `request_owner_approval`, owner card,
   `authorize_approval_response`, `begin_approval_responses`, the helper call,
   then `observe_approval_response` and `observe_approval_application`. Deny and
   owner-wait expiry (`deny_for_owner_wait_expiry`) return `tool/result`
   `ok:false` and never reach the helper. A helper answer lost after approval is
   recorded as `OutcomeUnknown` and surfaces as the existing outcome-unknown
   error. A repeated `call_id` is refused by the existing per-turn `call_id` set
   before any gate, so it raises no second card and never redoes work silently.
4. **Helper owned-profile marker.** The helper adds
   `Profile::COORDINATION_TOOLS_ENV = "HAGENCY_COORDINATION_TOOLS"` (value `1` or
   absent, anything else is invalid, like `HAGENCY_FILE_TOOLS`) and
   `owned_plugin_tools(send, receive, coordination)`, which is `owned_task_tools`
   plus the eight coordination names when true. `owned_task_tools` is unchanged,
   so Octos and Claude profiles are unchanged. The host reserves the marker in
   the helper environment so a plugin can never set it. This amends ADR-180's
   "no environment marker" for the owned profile only: that profile hides the
   group by default, which is what the marker reveals.
5. **Fail closed.** The host offers coordination to a plugin only when all three
   hold: the host `coordination_tools` option (ADR-180, `agent-driver.json`) is
   on, an approval host is configured, and the runtime advertised `tool`. If the
   gate cannot be built, the host omits the coordination names and the core task
   catalog is unchanged. A forged `tool/call` for a gated name with no gate is
   refused with "needs owner approval, which is not available" and never reaches
   the helper.
6. **Capability secrecy is unchanged.** The plugin never receives
   `HAGENCY_RUNNER_*`. The host keeps the helper as its own child; the runtime
   only sends and receives `hagency.runtime/v1` frames.
7. **Graph tools stay out.** As in ADR-180, `create_graph` and the other graph
   tools are not offered. The two-agent proof needs only `delegate_task`.
   Graph delegation is deferred to a later decision.
8. **Peer engagement IDs.** No tool tells an agent a peer's engagement ID
   (ADR-180). The operator names the exact IDs in the instruction.
9. **Runtime-side approvals stay denied.** Plugin `approval/request` frames are
   still denied; this ADR adds a host-side gate, not a runtime approval channel.

## Consequences

Good, because a plugin runtime can delegate with the same owner control a Codex
agent has, no capability reaches the plugin, and every default is unchanged
(option off means the old catalog).
Good, because the gate reuses the store approval protocol, so expiry, restart and
unknown-outcome behavior follow existing rules.
Bad, because every delegation needs an owner click, and the owner wait (up to the
approval policy wait) holds the plugin turn; the runtime must tolerate a long
`tool/call`. The host does not read frames while it waits, so a runtime must not
send a per-second `progress` heartbeat during a pending call or its pipe can fill.
Bad, because a delegated task reaches a plugin assignee only if delegated-session
delivery starts that engagement's plugin dispatch. ADR-180 proves inline factory
agents. This is not verified for plugin assignees and must be observed live.

Not verified by this ADR: any live run, how OMP reaches the host tools (a
runtime-side bridge, decided in the roadmap and implemented in the plugin), and
OMP's MCP call timeout.

Rollback: set `coordination_tools` false; plugins return to the core catalog.

## Alternatives Considered

- Offer the coordination names ungated: rejected, bypasses the owner.
- Reuse the runtime `approval/request` frame: rejected, the host would still need
  a gate for a runtime that never sends it, and runtimes could skip it.
- Pre-approve `delegate_task` for plugins: rejected, creating work for another
  engagement is the action the owner approves.
- Give the plugin a runner capability or direct MCP: rejected by ADR-195 and the
  spec's Must Not.
- Include graph tools: deferred, ADR-180 kept them out and the proof does not
  need them.
