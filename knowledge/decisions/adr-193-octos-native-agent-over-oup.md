---
kind: decision
id: ADR-193
title: "Octos runs as a native agent beside Codex and Claude Code, driven over its own UI Protocol (OUP)"
status: Proposed
satisfies: [REQ-RUST-MIGRATION-EXECUTION]
amends: [ADR-192]
tags: [native, octos, oup, runtime, setup, approvals, usage, qualification]
---

## Context

**What exists.** ADR-192 made the runner a seam with two drivers behind one shape: Codex (`codex app-server`, JSON-RPC over stdio) and Claude Code (stream-json over stdio). Dispatch admission, store claims, workspace leases, owner cards, the usage ledger and settlement are shared. Octos is the third coding agent the operator asked for (2026-10-08), using the latest Octos main and its own protocol, OUP.

**Octos and OUP.** Octos (octos-org/octos, latest main `41ad4911e`, version 2.0.3-rc.13) is a multi-provider agent kernel. OUP, the Octos UI Protocol (`octos-ui/v1alpha1`, `api/OCTOS_UI_PROTOCOL_V1_SPEC_2026-04-24.md`), is what its own clients use: OctosCode, the web app and Octos's own headless `octos chat`. The facts that shape this decision, from the spec and the code at that commit:

- **Transport.** `octos serve --stdio` speaks JSON-RPC 2.0, one JSON object per line, to one trusted local process; stdout carries only protocol, logs go to stderr. Request IDs must be strings. Frames are capped at 1 MiB. The server sends only notifications: approvals and tool calls are notifications the client answers with its own requests. Closing stdin ends the connection and its turns.
- **Handshake and session.** An optional `client_hello` negotiates features (its list replaces the stdio defaults). `session/open` must precede `turn/start` on the same connection. The client names the session (`<profile>:local:<id>`).
- **Turn.** `turn/start {session_id, turn_id, input}` with a client-chosen UUID. Output arrives as canonical `projection/envelope` notifications. The turn ends at a `turn_terminal` payload for that turn ID: `outcome` is `completed`, `errored`, `interrupted` or `rate_limited`, with an optional `error` and exact `token_usage`. Octos's own client takes the reply as the last `assistant_persisted` text after the last tool start. One turn runs per session; `turn/interrupt` stops it.
- **Approvals.** `approval/requested` carries `approval_id`, `turn_id`, `tool_name`, a title and body, and for a shell command typed details: the command line and its working directory. `approval/respond` answers `approve` or `deny`, with an optional scope (`request`, `turn`, `session`, `tool`) that Octos keeps in memory per session. A deny returns a failure to the model, and the turn continues. Octos asks only for a narrow set: `sudo`, `rm -rf`, forced push, hard reset, high-risk plugin tools and gated host tools. Its sandbox contains the rest: writes only inside the workspace, and no network when denied.
- **Usage.** The terminal's `token_usage` is the exact usage of that turn: input, output, reasoning, cache read and cache write. An absent object means unknown.
- **Models.** There is no per-session model. A session runs its profile's primary model (`~/.octos/profiles/<id>.json`); changing it changes the whole profile.
- **Credentials.** Octos resolves its own provider keys: its auth file, the OS keychain through a profile's `env_vars`, or environment variables.
- **Tools.** There is no per-session MCP server. A host may instead register its own tools on a session from its own `serve --stdio` connection, without a credential (`peer/tools/register`, UPCR-2026-035). Calls arrive as `peer/tool/call` and are answered with `peer/tool/result`. The same registration, or `session/tool_list/set`, narrows the session's kernel tools.
- **Traps.**
  - By default Octos stores a coding session under `<cwd>/.octos/sessions/` and creates `<cwd>/.octos-workspace.toml`, inside the project.
  - Background agents (`spawn_agent`, `delegate`) can report after the terminal, and the kernel may start continuation turns.
  - Over stdio, `ask_user_question` blocks the turn unless its feature is left out.
  - One serve runs per data directory unless each has its own `--instance-data-dir`.
  - First use downloads a 334 MB embedding model unless `OCTOS_NO_MODEL_DOWNLOAD=1`.
  - Homebrew's `bin/octos` is a bash wrapper, and npm's is a Node launcher, not the binary.
- **Versions.** OctosCode pins Octos 2.0.3-rc.12. This Mac has rc.11 and rc.2 installed; latest main is rc.13.

**TS parity.** The retained TS product drove Octos through ACP (`octos acp`). That path keeps sessions in memory only, ignores per-session MCP servers and answers approvals with allow-once or deny only (`docs/TASK-octos-acp-parity.md`). The operator chose OUP instead.

## Decision

1. **A third driver behind the same seam.** `Runner::Octos` sits beside Codex and Claude Code with framework `octos`. Everything above the process stays shared, as ADR-192 decision 1 set out.

2. **One `octos serve --stdio` per dispatch: one session, one turn.**
   - The guardian spawns `octos serve --stdio --cwd <workspace> --no-network --instance-data-dir <short private dir per agent>`.
   - Hagency sends `client_hello` with a fixed feature list: canonical projection v2, typed approvals and the workspace working directory. It never sends `user_question.v1`, so no question blocks a turn.
   - Hagency sets the permission profile, opens a fresh session named for the dispatch, starts one turn with the dispatch payload and reads to that turn's `turn_terminal`.
   - It then closes stdin and stops the process tree.
   - A process kept ready per agent is a later optimisation, as for Claude Code.

3. **The launch profile.**
   - **Permission profile.** `workspace_write` with network denied and approval policy `on-request` while the dispatch holds the write lease; `read_only` without it. `danger_full_access`, `--solo` and approval policy `never` are never used.
   - **Environment.** An allowlisted environment: `HOME`, `USER`, `PATH`, `TMPDIR` and `OCTOS_NO_MODEL_DOWNLOAD=1`. Provider keys (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `DEEPSEEK_API_KEY` and the rest Octos knows) are removed, as for Codex and Claude Code, so Octos uses the keys in its own store.
   - **Tools.** The session's kernel tools are narrowed to the foreground coding set: files, shell, search, planning and checks. There are no sub-agents, delegation, background work, user questions, peers, loops, monitors or cron. The dispatch therefore ends at Octos's own turn end (ADR-183).
   - **Clean workspace.** No Octos file is written into the project: sessions stay in Octos's per-profile store, outside the workspace. The qualification checks this.

4. **Approvals use the same cards.** `approval/requested` becomes an owner approval through the existing store and card pump.
   - The store's request binding takes an Octos form on the same fields: the session ID is the thread, the dispatch's one turn is the turn, and the approval ID is the item. The tool name and the typed details travel in the request, which its digest covers.
   - A shell command derives the same `exact_command` scope as a Codex command, from its command line and working directory, so **Allow for this task** and **Always allow this operation** work as they do for Codex. Other kinds offer **Approve once** and **Deny** only.
   - **Approve once** answers `approve` with scope `request`. **Deny** answers `deny`; Octos returns the denial to the model and the turn continues, as for Codex and Claude Code.
   - Hagency never sends a `turn`, `session` or `tool` scope, so Octos never records a rule of its own. Task and always grants are Hagency's, and Hagency answers matching requests itself.
   - No answer before the card expires is a deny. An approval Octos withdraws (`approval/cancelled`, or a tool's own timeout) closes its card as withdrawn, under the ADR-046 rules for a resolution.

5. **Task tools are host tools.** Hagency registers its task tools on the session from its own stdio connection: the same seven as for Codex and Claude Code (ADR-158), plus the file tools when enabled, named `hagency.get_task` and so on. Octos sends each call as `peer/tool/call` and Hagency answers with `peer/tool/result`. The calls run through the same capability-bound task tools as for the other two agents, so the tools behave the same for all three. Nothing is written into the user's Octos profiles.

6. **The reply and usage are captured like Codex's.**
   - **Reply.** The reply is the turn's last `assistant_persisted` text after its last tool start, as in Octos's own `octos chat`.
   - **Completed.** A `completed` terminal with a reply completes the dispatch.
   - **Failed.** `errored`, `interrupted` and `rate_limited` settle the way a failed Codex turn does, and the error code is kept with the attempt.
   - **Usage.** The terminal's exact `token_usage` goes into the ledger with framework `octos`. An absent total is unknown, never zero.

7. **A resource names an Octos profile.** Octos has no per-session model, so a resource runs the primary model of one of the user's Octos profiles.
   - **The block.** A `local_octos` block in `fleet-runtime.json` names the user's Octos home and the profiles Hagency may run. The block pins the `octos` binary and its SHA-256.
   - **The resource.** An Octos resource is `framework: octos`, the profile's provider family and model, and the profile it runs.
   - **Admission.** Admission checks that the profile's primary still matches the resource, so a profile the user changed refuses that dispatch instead of silently running another model.
   - **Qualification.** `role-capacity.json` gains the Octos models Hagency qualifies, with their tiers, as for Codex and Claude Code.

8. **Setup detects Octos and assumes it is configured.**
   - **Detection.** Setup runs `octos --version` only, on the binary found on `PATH` or chosen by the operator. Homebrew's wrapper and npm's launcher are resolved to the real binary, and that binary is pinned by path and SHA-256.
   - **No credential check.** Hagency neither asks about nor checks Octos's provider keys, as for Claude Code (ADR-192 operator decision 1).
   - **Profiles.** Setup lists the user's Octos profiles with their primary models, and offers those whose model Hagency qualifies.
   - **Unusable profiles.** A profile without a usable key shows up as refused turns on its agent.

9. **Failures follow ADR-183.**
   - A start that fails before the turn, including another serve holding the data directory (`OCTOS_DATA_DIR_LOCKED`), is retried with the launch backoff.
   - Unknown fields and notifications are ignored, as OUP's additive rule requires. An unknown terminal outcome or a malformed frame refuses that turn.
   - Only the turn's own `turn_terminal` ends it. Nothing ends the service.

## Security

- **Credentials.** Hagency never handles Octos credentials, and provider keys are removed from Octos's environment.
- **Fixed launch profile.** `workspace_write` with network denied and on-request approvals. `danger_full_access`, `--solo` and approval policy `never` are never set.
- **Approvals.** Every approval Octos raises reaches the owner. Octos records no approval rule of its own, and grants are Hagency's.
- **Narrower ask set.** Octos asks before fewer commands than Codex. Its sandbox contains the rest: writes only inside the workspace and no network. The qualification proves this per OS before Octos resources are publishable.
- **Task tools.** They are host tools on Hagency's own private connection, bound to the dispatch's capability. Nothing is written into the user's Octos configuration or profiles.
- **Isolation.** Each dispatch gets a fresh session and process, so one owner's context is never visible in another's turn. The project workspace holds no Octos file.

## Consequences

- The runner seam gets its third driver. Most of the work is the OUP codec, the session driver and the host-tool bridge.
- OUP is an alpha protocol (`v1alpha1`) and changes additively. The codec follows that rule, and the qualification is pinned to the Octos version it ran against.
- Owners see fewer approval cards for Octos agents than for Codex agents. Containment rests on Octos's sandbox, which is why the qualification must cover the sandbox as well as approvals.
- An Octos agent's model follows the user's Octos profile. Changing that profile is the user's way to change the model, and it is noticed at admission.
- Octos's background agents are not available to Hagency agents in this version.

## Slices

1. **The runner seam.** The OUP codec and session driver (string IDs, handshake, envelopes, terminal), `Runner::Octos` in Host admission, the operation and the store's claim query, and reply and usage capture. Offline tests use a fake Octos process.
2. **Approvals.** Octos approvals as owner cards: the binding, the `exact_command` scope for shell commands, withdrawals and expiry.
3. **Task tools.** The host-tool bridge.
4. **Setup and binding.** Detection, the `local_octos` binding and profile listing, the `fleet-runtime.json` Octos block, and Octos offer choices (EN/zh).
5. **Qualification.** A live run against the latest Octos main and one of the user's profiles: approve, deny, a write outside the workspace, a clean workspace and recorded usage, with an evidence file as for Claude Code.
6. **Live rig run and docs.**

## Alternatives considered

- **ACP (`octos acp`).** The retained TS path. It keeps sessions in memory, ignores per-session MCP servers and has only allow-once or deny. The operator chose OUP.
- **`octos chat -m … --json`.** One shot with no approval channel: without a terminal, every approval is denied.
- **`octos serve --host-managed`.** The host performs the model calls, so Hagency would handle provider credentials. Rejected.
- **MCP through a profile's `mcp_servers`.** That is per profile, not per dispatch, and it would write a dispatch capability into the user's profile. Host tools are per session and need no write.
- **A long-lived serve per agent with a session per dispatch.** Faster turns, kept as a later optimisation once the one-shot path is qualified.
- **OUP over WebSocket.** It needs an auth token and a port, while stdio is OUP's trusted local transport.

## Questions for the operator

1. **The model.** Should a resource run one of your own Octos profiles, its primary model being the resource's model (recommended), or should Hagency keep its own Octos profile per model?
2. **Provider keys.** Hagency removes provider keys from Octos's environment, so Octos must find its keys in its own store (`octos auth login -p <provider>`, or a profile's keychain entry). Is that right (recommended), or should Hagency forward the provider keys set in your shell?
3. **Background agents.** This version narrows Octos's tools to the foreground set, so a dispatch ends at Octos's own turn end (recommended). The alternative keeps sub-agents and background work, and ends the dispatch only when Octos reports that all of it is idle.
