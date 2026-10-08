spec: task
name: "Requests never name the agent's workspace folder or bootstrap command"
inherits: project
satisfies: [REQ-RUST-MIGRATION-EXECUTION]
tags: [active, rust, security, provisioning, workspace]
---

## Intent

Board #78 ported TS's per-agent workspace settings (`workspaceMode`,
`worktreesDir`, `worktreeBootstrap`) onto the request's agent definition. TS
kept them on an agent record its operator wrote; here a requester's event
carried them, the host created any folder they named and ran their bootstrap
command outside any sandbox, and the machine owner's approval showed neither.
A request now never carries them, and a stored record that still does keeps
the shared workspace. The machine owner's own setting is a later decision.

## Constraints

### Must
- Refuse a request naming any workspace setting at verification, on every intake path, before admission.
- Refuse it as an invalid request, a durable refusal on the Palpo path rather than a retry.
- Admit every request with the shared workspace and no worktrees folder or bootstrap.
- Run every dispatch of an agent whose stored record carries the settings in its shared workspace: no folder of the requester's is made and no bootstrap of theirs runs.

### Must Not
- Do not make a stored request unreadable or refuse re-attaching an agent admitted before this rule.
- Do not remove the worktree machinery itself; only its requester-supplied input.

## Boundaries

### Allowed Changes
- native/hagency-core/src/authority.rs
- native/hagency-core/src/project.rs
- native/hagency-store/src/domain.rs
- native/hagency-store/src/domain/owned_dispatch.rs
- native/hagency-store/tests/authority.rs
- native/hagency-execution/tests/worktree_dispatch.rs
- native/hagency/tests/bootstrap.rs
- native/hagency/tests/bootstrap/fixture.rs
- specs/task-rust-request-workspace-settings.spec.md

## Acceptance Criteria

Scenario: A request naming a workspace setting is refused
  Test: native_request_cannot_set_the_agent_workspace
  Level: unit
  Test Double: fixed request and matching observation
  Given a request whose agent definition names a workspace mode, worktrees folder or bootstrap
  When it is verified
  Then it is refused as invalid, while the same request without them verifies

Scenario: A stored record's settings never reach the host
  Test: native_stored_workspace_settings_never_reach_the_host
  Level: integration
  Test Double: offline app-server probe through the production owned-dispatch path
  Given an agent whose stored record names a worktrees folder and a bootstrap, on a git workspace
  When two of its threads dispatch
  Then both run in the shared workspace, the folder is never made and the bootstrap never runs

Scenario: The service keeps the shared workspace for such an agent
  Test: native_stored_workspace_settings_keep_the_shared_workspace
  Level: integration
  Test Double: offline service with its SERVE configuration
  Given an agent whose stored record names worktree settings and a bootstrap
  When the service runs two threaded dispatches
  Then each runs in its session's shared workspace and neither the folder nor the bootstrap's output exists
