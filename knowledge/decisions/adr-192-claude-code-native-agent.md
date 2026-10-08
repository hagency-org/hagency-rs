---
kind: decision
id: ADR-192
title: "Claude Code runs as a native agent beside Codex, on the existing Claude stream pieces, set up and approved the same way"
status: Accepted
satisfies: [REQ-RUST-MIGRATION-EXECUTION]
amends: [ADR-155, ADR-189]
tags: [native, claude, runtime, setup, approvals, usage, qualification]
---

## Context

**How Codex runs today.** Each Codex agent is a guardian-owned `codex app-server` process speaking JSON-RPC over stdio (`hagency-runtime/src/codex/`, `hagency-runtime/src/owned/session.rs`). It is held warm per agent (`hagency-execution/src/warm.rs`).

A dispatch becomes one Codex turn on an ephemeral thread (`hagency-execution/src/operation.rs`). The turn text is built from the dispatch payload (`host.rs`), and the final agent message becomes the reply unless the agent completed the task itself.

The pieces around the process:

- **Approvals.** Codex's server requests are bound to the thread, turn and item IDs and become owner approval cards (`approval/control.rs`, `hagency-store/src/domain/approvals.rs`).
- **Task tools.** Task and file tools come from `hagency mcp` (`hagency-runtime/src/task_mcp.rs`).
- **Usage.** Usage is read from `thread/tokenUsage/updated` into the ledger (`hagency-execution/src/usage.rs`).
- **Sign-in and pinning.** The agent signs in through the user's own Codex folder. The local Codex binding checks that folder (`local_codex.rs`), and `fleet-runtime.json` pins the executable and its SHA-256 (`bootstrap/config.rs`).
- **Setup.** The Setup page runs `codex --version` and `codex login status` (`setup.rs`, ADR-189). Published resources are `codex`/`openai` pairs qualified in `role-capacity.json` (ADR-140, ADR-144).

**What exists for Claude.** ADR-154 to ADR-158 built the native Claude pieces and tested them against a fake Claude process and two operator-only probes of CLI 2.1.270:

- a codec for the Agent SDK's stream-json envelope and its control requests (`hagency-runtime/src/claude.rs`);
- a guardian-owned, one-prompt session (`claude/session.rs`, `owned/claude.rs`);
- one-shot allow/deny for `can_use_tool` that returns the exact original input (`claude/session/control.rs`);
- usage capture deduplicated by message ID (`claude/session/usage.rs`);
- the scoped task helper, bound through `mcp_set_servers` (`claude/task_mcp.rs`).

The launch arguments are fixed: `--print --verbose --input-format stream-json --output-format stream-json --permission-mode auto|plan --permission-prompt-tool stdio --model=<model>`.

**What blocks Claude.** Nothing in production starts a Claude session. Claude is refused in four places:

- Host admission returns `UnsupportedRunner` (`host.rs`).
- The store claims only dispatches whose resource is `codex`/`openai` (`hagency-store/src/domain/execution.rs`).
- The local binding is Codex-only.
- Managed accounts are Codex-only.

Also missing:

- approval cards for Claude requests (the store binds Codex IDs, and the card text says "codex");
- capture of the final reply text (the result observation carries usage and the error flag only);
- interrupt wiring;
- reuse of a warm session;
- detection in Setup;
- a Claude block in `fleet-runtime.json`;
- a sandbox qualification like ADR-140's.

**TS parity.** The retained TS product ran Claude Code in two ways:

- interactive in tmux, with an MCP channel for permission prompts (ADR-005);
- as one disposable `claude -p --output-format stream-json` process per dispatch.

For the per-dispatch runners (`backend-v2.js` `claudeThreadSessionArgs`, `lib/claude-thread-runtime.js`, `lib/supervisor-lifecycle-manager.js`):

- permission mode was `auto` when the dispatch held the workspace write lease, otherwise `plan`;
- `ask` rules covered `Bash(gh *)` and `Bash(git push *)`;
- model names were validated;
- an ambient `ANTHROPIC_API_KEY` was removed unless the agent's profile named a key.

ADR-154 to ADR-158 already carry these rules into the native code.

**Claude Code's interface.**

- Print mode streams JSON in both directions.
- Permission prompts, interrupt and MCP server changes use the control messages of the official Agent SDK's wire format. That format is defined by the SDK, pinned in ADR-158 to SDK 0.3.270 and CLI 2.1.270, not by a separate published specification.
- A user signs in with `/login` (a Claude subscription), a long-lived token from `claude setup-token`, an API key, or a cloud provider.
- No documented non-interactive command reports the sign-in, unlike `codex login status`.

ADR-189 says Claude Code is added through its own ADR. This is that ADR.

## Decision

1. **One shape, two drivers.** Everything above the coding-agent process stays shared:

   - dispatch claiming and guardian custody;
   - the warm runtime lifecycle;
   - the owner approval store and Matrix cards;
   - the task MCP descriptor;
   - the usage ledger;
   - fleet provisioning;
   - ADR-183's failure rules.

   A resource's `framework` (`codex` or `claude`) selects the driver, and one Hagency may run Codex and Claude agents side by side. A runner seam, an enum over the two drivers, replaces the points now typed on Codex: Host admission, the operation, the approval binding, the warm runtime and the store's claim query. No other framework is added here.

2. **A fresh Claude session per dispatch, kept ready in advance.** The Claude driver keeps ADR-155's one-prompt session. The warm runtime holds one spawned, initialized and tool-bound Claude session per agent, waiting for its prompt.

   A dispatch uses it for exactly one turn, its result ends the turn, and the runtime starts the next session for the next dispatch. A session that fails, is dropped or reaches its lifetime is replaced; one with an unknown outcome is never reused.

   This matches Codex's ephemeral thread per dispatch. No conversation memory crosses dispatches, rooms or owners. Starting the next session ahead of time keeps Claude Code's start-up and tool binding off the reply path.

   This amends ADR-155 only by naming the warm runtime as the session's owner before its prompt arrives.

3. **The launch profile stays ADR-158's.**

   - The MCP configuration is strict and empty, plus the one scoped task helper, bound after `initialize`.
   - No filesystem setting sources, hooks off, no session persistence, no slash-command skills.
   - Permission mode is `auto` with the write lease and `plan` without it, and the `Bash(gh *)` and `Bash(git push *)` ask rules stay.
   - Permission prompts always go to the owner (`--permission-prompt-tool stdio`). `bypassPermissions` and `dontAsk` are never used.
   - The process gets an allowlisted environment.
   - Provider keys such as `ANTHROPIC_API_KEY` are removed, so the agent draws on the sign-in its binding names (TS ADR-005 point 5).
   - The service's Claude processes run with Claude Code's auto-updater off; updates by the user are noticed through decision 7.

   An owner stop sends the SDK `interrupt`. Under ADR-183 decision D the budget only notifies; only the agent's own turn end or a human ends the work.

4. **Approvals use the same cards.** A `can_use_tool` request becomes an owner approval through the existing store and card pump.

   - The store's request binding gains a Claude form: session ID, control request ID, tool name and a digest of the input, in place of Codex's thread, turn and item IDs.
   - The card names the agent's framework instead of a fixed "codex".
   - **Approve once** answers allow with the exact original input (ADR-156).
   - **Deny** answers deny with `interrupt`.
   - **Allow for this task** and **Always allow this operation** are Hagency-side grants matched on tool name and canonical input. Hagency answers later matching requests itself and never sends Claude `updatedPermissions` (ADR-156).
   - No answer before the card expires is a deny, as for Codex.
   - A turn waiting on its owner keeps its session and leases.

5. **The reply is captured like Codex's.** The success result's final text becomes the reply when the agent did not complete the task itself. An error result settles the dispatch the way a failed Codex turn does. Usage follows ADR-157 into the ledger with framework `claude`. An incomplete snapshot counts as unknown, never zero, so quotas hold conservatively.

6. **Setup detects Claude Code and assumes it is signed in (amends ADR-189 §5.1).**

   - **Detection.** Setup runs `claude --version` only, on a binary found on `PATH` or chosen by the operator, and records its resolved path and SHA-256. If Claude Code is not installed, the page asks the user to install it.
   - **No sign-in check.** Signing in is the user's, and Hagency neither asks about it nor checks it. Setup configures Claude Code as soon as it finds it.
   - **Any sign-in may be offered.** A personal Claude subscription is an accepted sign-in for an agent offered to others, so the page shows no subscription note.
   - **A signed-out Claude Code** shows up as refused turns on that agent (decision 9), not on the Setup page.

7. **A local Claude binding, beside the Codex one.**

   - **The block.** A `local_claude` block in `fleet-runtime.json` mirrors `local_codex`: preset `local_claude`, seat `local_claude_seat`, and the user's Claude folder (`CLAUDE_CONFIG_DIR`, by default `~/.claude`). The block pins the `claude` executable and its SHA-256.
   - **Checks.** The folder gets the same private-mode checks and the same periodic re-check. A failing check refuses that dispatch or parks that agent, never the service (ADR-183; PR #34 does the same for Codex admission).
   - **Setup writes.** The file may hold a Codex block, a Claude block or both. Setup writes the block for each coding agent it finds: Codex when signed in (ADR-189), Claude Code when installed.
   - **Updates.** A changed binary makes its block stale; Setup rewrites it and asks for a restart, as for Codex.

8. **Resources come from qualification.** Claude resources are `claude`/`anthropic` with a model from `role-capacity.json`, which already lists Claude models. They carry no reasoning setting, because print mode has no documented effort flag.

   A Claude resource is published to Palpo only after an operator-run qualification of the pinned CLI version passes, like ADR-140 and ADR-144 for Codex: a real sign-in, a sandboxed workspace, an approval round trip and recorded usage. Setup's **Offer a resource** step lists the qualified Claude choices when a Claude agent is configured.

9. **Failures follow ADR-183.**

   - A Claude start that fails before the prompt is retried with the launch backoff.
   - A lost warm session is replaced.
   - An unknown event refuses that turn (ADR-154) and the session is replaced.
   - A turn Claude Code refuses for its sign-in is shown on that agent and retried with backoff, so the agent resumes once the user signs in again.
   - Nothing ends the service.

## Security

- Hagency never handles or checks Claude credentials, and provider keys are stripped from the agent environment.
- The launch profile is fixed:
  - every permission prompt reaches the owner;
  - protected Git hosting commands always ask;
  - without the write lease the agent runs in `plan` mode;
  - `bypassPermissions` and `dontAsk` are never set.
- The operator's own Claude settings, hooks, MCP servers and skills never load into an agent session: there are no setting sources and the MCP configuration is strict.
- Control requests are untrusted input with ADR-154's and ADR-156's bounds. Grants are recorded and checked by Hagency, never handed to Claude.
- Each dispatch gets a fresh session, so one owner's context is never visible in another's turn.

## Consequences

Good, because:

- owners get a second coding agent with the same rooms, cards, tokens and Setup flow;
- the tested ADR-154 to ADR-158 pieces carry the work, and the TS rules come along unchanged;
- a per-dispatch session closes the cross-room context-bleed class that a long session would open.

Bad, because:

- there are two drivers to keep in step behind one seam;
- Claude's control protocol is defined by the SDK, not by a published specification, so every Claude Code version is pinned, checked by Setup and qualified before resources publish;
- a fresh session per dispatch costs a process start, hidden by starting it early;
- agents run on the user's own sign-in, a personal subscription included, with its limits; because Hagency does not check the sign-in, a signed-out Claude Code shows up only as refused turns.

## Slices

1. **The runner seam.** Claude in Host admission, the operation, the warm runtime and the store's claim query, with one ready session per agent, reply capture and interrupt. Offline tests use the fake Claude process. Reconcile ADR-158 (four pre-approved task tools) with the code (seven).
2. **Approvals.** The Claude request binding in the store, card text by framework, once/task/always grants, and expiry as deny.
3. **Setup and binding.** The local Claude binding, the `fleet-runtime.json` Claude block, Setup detection, and Claude offer choices (EN/zh).
4. **Qualification.** The operator-run live qualification for the pinned CLI version: sandbox, approvals and usage. Only then are Claude resources publishable.
5. **Live run and docs.**
   - On the rig: a Claude agent in a project room, then a mixed Codex and Claude fleet.
   - Update the README, the user guide and the walkthrough.

## Alternatives considered

- **Keep the TS tmux agent with its MCP channel (ADR-005).** Rejected: a background terminal prompt is an invisible blocking state, and the native control channel already exists.
- **Drive Claude Code through ACP, using a third-party adapter.** Rejected for now: it adds a Node runtime and a wrapper around the same SDK to a single-binary product. ADR-158 already rejects an SDK or JavaScript bridge.
- **Run the Agent SDK as a sidecar process (TypeScript or Python).** Rejected for the same reason.
- **One long Claude session per agent, with every dispatch as a new user turn.** Rejected:
  - conversation memory would cross dispatches, rooms and owners;
  - an unknown outcome would carry into later turns;
  - Codex already uses a fresh thread per dispatch.
- **A disposable process per dispatch, started on demand (TS parity).** Simpler, but every reply would wait for Claude Code to start and bind its tool. It is the fallback if starting the session early proves unsound.

## Operator decisions (2026-10-07)

1. Claude Code is assumed to be signed in. Setup does not ask or check.
2. An agent signed in with a personal Claude subscription may be offered through Palpo.
3. One Hagency may run Codex and Claude agents side by side.
