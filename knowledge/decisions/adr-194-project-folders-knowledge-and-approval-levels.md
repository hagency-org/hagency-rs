---
kind: decision
id: ADR-194
title: "Project folders, project knowledge and approval levels: the machine owner decides, the project names"
status: Accepted
satisfies: [REQ-RUST-MIGRATION-EXECUTION]
amends: [ADR-028, ADR-036, ADR-043, ADR-046, ADR-192, ADR-193]
tags: [native, workspace, provisioning, approvals, security, palpo]
---

## Context

**Two owners.** Every engagement involves two people. The machine owner's computer runs the agent and publishes the resource. The project owner's room asks for the agent. Through Palpo they can be strangers. Whatever the agent touches on the machine is the machine owner's risk; whatever it produces belongs to the project owner.

**Folders today.**
- Each agent gets a private home on the machine and works in that home's `workdir`.
- The project's code appears inside it as `projects/<project id>` only if the machine owner mapped the project to a folder in `fleet-runtime.json` (`home.projects`, as a copy or a link). Setup writes no mapping, and neither the console nor a room can add one. So a new agent usually starts with no code.
- A request names a role and a token budget, and the console approval sets the budget. Neither names a folder.

**A hole, closed first.**
- Board #78 put TS's per-agent workspace settings (`workspaceMode`, `worktreesDir`, `worktreeBootstrap`) on the request's agent definition.
- TS kept them on an agent record its operator wrote. Here a requester's event carried them. The host then created any folder they named and ran their bootstrap command outside any sandbox, and the approval showed neither.
- Requests no longer carry them (branch `fix/request-workspace-settings`, `specs/task-rust-request-workspace-settings.spec.md`).

**Knowledge today.** An agent's notes and plans live in its private home, on the machine owner's disk. The conversation lives in the project room.

**Approvals today.**
- Codex and Octos run sandboxed, with writes only in the workspace and the network off, and ask the owner before risky actions.
- Claude Code has no sandbox in Hagency's setup; its permission prompts are its only gate (ADR-192).
- ADR-028 let a contributor turn on YOLO for Codex: `danger-full-access` with approval policy `never`, only for dispatches that hold the write lease. The native port refused it (ADR-036, ADR-043, ADR-046). The console stores `executionPolicy.yolo`, but nothing reads it.

## Decision

1. **The machine owner gives each resource a base folder.** Creating or editing a resource names one existing folder on the machine. Every project folder for that resource's agents is made inside it, and Hagency makes or opens no project folder anywhere else. Until a base folder is set, the resource's agents work in an empty folder, as today.

2. **The project owner names the project folder.**
   - A request may carry a folder name: letters, digits, `-` and `_`, at most 64 characters, never a path. Without one, the project's ID is used.
   - Within one base folder, a project has one folder. Its later requests share that folder, and a name another project already uses there is refused.

3. **The folder starts empty or as a public repository.** A request may also name a public Git repository by `https` URL. Hagency clones it once, without credentials, into the new folder. Private repositories and keys are not supported (operator decision 2).

4. **The machine owner sees it at approval.** The approval shows the project, the folder name, the full path the folder will have, and the repository. The machine owner can refuse, and nothing is made before approval.

5. **The agent works in the project folder.**
   - The project folder is the agent's working folder.
   - Agents of the same project on the same resource share it, and their writes take turns under the existing workspace lease.
   - Hagency's own files for the agent stay in its private home, outside the project. Its instructions to the agent travel with each task, as the task helper's guidance already does.

6. **Knowledge lives in the project** (operator decision 3).
   - Agents keep their notes, plans and skills in `.hagency/` inside the project folder, where the next agent on the project finds them.
   - Agents have no write access to the project's repository, so they send results and knowledge to the project owner in the room, as files and messages.

7. **Requests never name paths or commands.** A request that names a workspace mode, a worktrees folder or a bootstrap command is refused. When wanted, worktree mode and a bootstrap are machine-owner settings on the resource, with worktrees inside the base folder.

8. **Approval levels belong to the machine owner, per resource** (operator decision 4).
   - **`ask`** (the default) is today's behaviour.
   - **`no prompts`**: the agent never asks. Its sandbox stays on, and whatever it would have asked about is refused.
     - Codex: `workspace-write`, network off, approval policy `never`.
     - Octos: `workspace_write`, network denied, approval policy `never`.
     - Claude Code: not offered. It has no sandbox in Hagency's setup, and its only no-prompt mode that still refuses (`dontAsk`) would refuse the edits and commands a coding agent needs.
   - **`full access`**: no sandbox and no prompts; the agent can do whatever the machine owner's account can.
     - Codex: `danger-full-access` with `never` (ADR-028).
     - Claude Code: `bypassPermissions`.
     - Octos: `danger_full_access` with `never`.
   - The machine owner sets the level when creating or editing a resource, and can change it for one agent in the console. A change applies from the next task.
   - Only a dispatch that holds the workspace write lease runs at `no prompts` or `full access`.
   - The offer shows the level, and so does the agent, in the room and in the console. No room command or request sets it.

9. **A project can refuse agents that do not ask.** A request may say approvals are required. Hagency then provisions it only from an `ask` resource, and never raises that agent's level later.

## Security

- Only the machine owner decides what touches the machine: the base folder, worktree settings, bootstrap commands and the approval level.
- A requester names a folder and, optionally, a public repository, and nothing else. Hagency checks the name and makes the folder only inside the base folder.
- `full access` removes every protection Hagency adds. The console says so when the level is set, and the offer says so to every project.
- Clones run without credentials, so none of the project owner's secrets reach the machine.

## Consequences

- New agents get the project's code instead of an empty folder.
- The project owner keeps the knowledge. Changing machines or agents loses nothing written to `.hagency/` and sent to the room.
- YOLO, which the native port has refused so far (ADR-036, ADR-043, ADR-046), returns for all three agents as levels only the machine owner sets.
- This amends:
  - ADR-028: one switch for Codex becomes levels for all agents;
  - ADR-192 decision 3: `bypassPermissions` is allowed at `full access`;
  - ADR-193 decision 3: `danger_full_access` and `never` are allowed at the machine owner's levels.
- Worktree mode (board #78) is off until it becomes a machine-owner setting.

## Slices

1. Requests never name workspace settings (done, `fix/request-workspace-settings`).
2. A base folder on resources, the folder name in requests, the approval display, folder creation, and the agent working in the folder.
3. Public repository clone.
4. Knowledge in `.hagency/`, and results sent to the room.
5. Approval levels: the resource setting, the per-agent change, the mapping for each runner, and the display in offers and on agents.
6. A project's refusal of agents that do not ask.
7. Live run and docs (EN/zh).

## Alternatives considered

- **One folder per resource.** A resource serves many projects, and one folder would mix them.
- **One folder per agent.** Agents of one project would each hold a separate copy of the work, and the project's notes would split between them.
- **The project owner names a path.** A stranger would choose a place on another person's disk.
- **Private repositories with a deploy key.** The project owner cannot check a stranger's machine. Deferred (operator decision 2).
- **The approval level as a project option.** A project would lower the protection of a machine it does not own.

## Operator decisions (2026-10-08)

1. The project owner names the folder, and Hagency makes it inside the location the machine owner set on the resource.
2. No private repositories for now: a project owner has no way to trust a machine they do not control.
3. Knowledge lives in the project.
4. Working without asking for approval is the machine owner's choice, set on the resource at two levels and shown in offers. A project can only refuse such agents.
5. Requests never name paths or commands; the hole they opened was closed first.
6. As proposed:
   - one folder per project within a base folder, shared by its agents;
   - an empty folder until a base folder is set;
   - no `no prompts` level for Claude Code.
