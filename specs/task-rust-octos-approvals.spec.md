spec: task
name: "Turn Octos approvals into owner approval cards"
inherits: project
satisfies: [REQ-RUST-MIGRATION-EXECUTION, REQ-EXECUTION-AUTHORIZATION, REQ-OWNER-UI-APPROVAL]
tags: [active, rust, octos, oup, approval]
---

## Intent

ADR-193 slice 2, decision 4: every `approval/requested` from an Octos session
becomes an owner approval through the existing store and card pump, on the
same protocol as Codex and Claude Code. Octos is answered with scope `request`
only, so it never records a rule of its own. Task and always grants are
Hagency's, matched on a shell command's exact line and working directory.

## Constraints

### Must
- Bind the approval context once Octos has accepted the dispatch's turn: the session is the thread, the dispatch's turn is the turn, and the approval ID is the item.
- Carry the tool name, title, body, the Octos turn and, for a shell command, its command line and working directory in the request its digest covers.
- Derive the `exact_command` scope for a shell command from its command line and working directory, with the same key as a Codex command; any other approval offers Approve once and Deny only.
- Answer `approve` or `deny` with scope `request`, from a persisted choice, in exactly one prepared frame that is authorized, begun and rechecked before its first byte.
- Deny at the owner bound when nobody answered, and answer that deny from the persisted choice.
- Treat an approval Octos settles before admission as a cancellation, and one settled after admission or while its answer is in write custody as a quiet drop; a started answer is finished, never cut.
- Consume Octos's answer to each `approval/respond` and its `approval/decided` echo; neither is an event.
- Name the runtime `octos` on the card and show the tool, title, body, command and working directory.
- Keep refusing an approval on a host without approvals, stopping the child.

### Must Not
- Do not send a `turn`, `session` or `tool` scope, or any decision other than `approve` and `deny`.
- Do not change Codex or Claude Code approvals, or decide a dispatch done on the bridge side.
- Do not run a provider or the real Octos in ordinary tests.

## Boundaries

### Allowed Changes
- native/hagency-core/src/execution.rs
- native/hagency-core/tests/execution.rs
- native/hagency-store/src/domain/approvals/card.rs
- native/hagency-matrix/src/approval_delivery/state.rs
- schemas/approval/owner-request-v1.schema.json
- native/hagency-runtime/src/octos/session.rs
- native/hagency-runtime/src/octos/session/**
- native/hagency-runtime/src/owned/octos.rs
- native/hagency-runtime/src/bin/octos_probe/mod.rs
- native/hagency-runtime/tests/octos_control.rs
- native/hagency-runtime/tests/octos_owned.rs
- native/hagency-execution/src/approval.rs
- native/hagency-execution/src/approval/**
- native/hagency-execution/src/operation.rs
- native/hagency-execution/tests/owned.rs
- native/hagency-execution/tests/owned/octos.rs
- native/hagency-execution/tests/owned/octos_approvals.rs
- specs/task-rust-octos-approvals.spec.md

## Acceptance Criteria

Scenario: A shell approval derives the Codex command scope
  Test: native_execution_octos_shell_scope
  Level: unit
  Test Double: none
  Given Octos approvals for a shell command, another tool, an unknown field and a missing directory
  When the scope is derived
  Then the shell command gets the Codex command's exact-command key, binding data stays out of it, and the others get no reusable scope

Scenario: The session answers each approval once with scope request
  Test: native_octos_control_approve_once_reaches_octos
  Test: native_octos_control_deny_lets_octos_continue
  Level: integration
  Test Double: offline OUP peer through the original guardian
  Given an approval on an owned session with approval control
  When the host prepares and sends approve or deny
  Then Octos reads one `approval/respond` with scope `request`, its answer and echo are consumed, and the turn goes on to its reply

Scenario: A withdrawal is a settlement and an answer in custody is never withdrawn
  Test: native_octos_control_withdrawal_is_a_settlement
  Test: native_octos_settlement_never_withdraws_an_answer_in_custody
  Level: integration
  Test Double: offline OUP peer through the original guardian, and the control state alone
  Given an approval Octos cancels, and settlements before and during an answer's write custody
  When the settlement arrives
  Then an unanswered approval is withdrawn and never answered, and an answer in custody is finished

Scenario: The owner bound leaves only the deny
  Test: native_octos_control_owner_bound_leaves_only_the_deny
  Level: integration
  Test Double: offline OUP peer through the original guardian
  Given an approval nobody answers
  When the owner bound passes
  Then the host may answer only deny, and Octos continues

Scenario: An Octos approval becomes an owner card
  Test: native_octos_approval_once_approves_that_request
  Test: native_octos_approval_deny_lets_octos_continue
  Level: integration
  Test Double: offline OUP peer launched by the Host
  Given an Octos dispatch on a host with approvals
  When Octos asks to run a shell command and the owner answers
  Then the card names Octos, the tool and the command with its exact-command scope, the answer reaches Octos once, and the dispatch completes with Octos's reply

Scenario: Hagency's grants answer identical approvals
  Test: native_octos_approval_grant_answers_the_next_identical_request
  Level: integration
  Test Double: offline OUP peer launched by the Host
  Given a task or always grant on a shell command
  When Octos asks for the same command again
  Then Hagency approves it with no second card, and Octos still receives scope `request`

Scenario: Expiry and withdrawal follow the Codex rules
  Test: native_octos_approval_owner_wait_expiry_denies
  Test: native_octos_approval_withdrawn_before_an_answer_cancels
  Level: integration
  Test Double: offline OUP peer launched by the Host
  Given an approval nobody answers, and one Octos withdraws first
  When the owner bound passes or the withdrawal arrives
  Then the first is denied once with no grant and Octos replies, and the second cancels the dispatch with nothing written to Octos
