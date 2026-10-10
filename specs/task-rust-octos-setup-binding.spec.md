spec: task
name: "Set up Octos and bind each Octos resource to one of the user's profiles"
inherits: project
satisfies: [REQ-RUST-MIGRATION-EXECUTION]
tags: [active, rust, octos, setup, fleet, qualification]
---

## Intent

ADR-193 slice 4 (decisions 7 and 8). Setup finds Octos, pins its binary, lists
the user's Octos profiles with their primary models and allows the profiles
whose model Hagency qualifies. `fleet-runtime.json` gains an `octos` block
whose `local_octos` binding names the user's Octos home and those profiles.
The fleet runs Octos agents as it runs Claude Code agents: no warm child, each
task its own `octos serve --stdio` through the follow-up binding, and a restart
re-attaches them. Admission reads the profile a resource names and refuses the
dispatch when that profile's primary model is no longer the resource's. The
console's Setup step shows Octos with its profiles and offers the qualified
ones, in English and Chinese.

## Constraints

### Must
- Run `octos --version` only, and only on an imported fleet; never ask about, check or read Octos's keys.
- Resolve Homebrew's `bin/octos` wrapper and npm's `bin/octos.js` launcher to the native binary beside them, refuse any other script, and pin the binary by path and SHA-256.
- Keep the Octos binary and home the runtime already pins when Setup rewrites it, unless the operator names others.
- Read only a profile's `id`, `parent_id` and primary (`config.llm.primary.family_id` and `model_id`), at most 1 MiB, and never show or keep anything else of it.
- Never run a sub-account (a profile with a `parent_id`): its model is its parent's, which its file does not show.
- Allow and offer only the profiles whose primary model `role-capacity.json` qualifies.
- Make an Octos resource framework `octos`, the primary's family as provider, its model, no reasoning, and the profile's ID; one unpublished source per profile.
- Require the `local_octos` binding in an `octos` block: profile `provider_owned_octos_v1`, 1 to 64 distinct profile IDs, and an owner-private Octos home.
- At provisioning and at every dispatch, refuse a resource whose profile is not allowed, or whose profile file's primary is not the resource's provider and model, reading the file through the retained Octos home.
- Give Octos `HOME`, `USER`, `PATH`, `TMPDIR` and `OCTOS_NO_MODEL_DOWNLOAD=1` only, and `OCTOS_HOME` only for a home other than `~/.octos`; no task helper.
- Give each Octos agent its own private instance directory under `<state>/octos`.
- Keep a resource's Octos profile fixed while agents are engaged on it, as its provider and model are.
- Qualify `zai-coding` / `glm-5.3-flash` at tier `medium`.
- Check the Octos home through every Octos turn, as a sign-in folder is checked for Codex and Claude Code.

### Must Not
- Do not write into the user's Octos home, profiles or binaries, and never read the user's real Octos home in tests.
- Do not change Codex or Claude Code behaviour, or the Claude Code launch sources its live qualification evidence pins (`local_codex.rs` among them).

## Boundaries

### Allowed Changes
- native/hagency-core/role-capacity.json
- native/hagency-runtime/src/octos.rs
- native/hagency-runtime/src/bin/hagency-runtime-probe.rs
- native/hagency-runtime/tests/octos.rs
- native/hagency-execution/src/local_octos.rs
- native/hagency-execution/src/local_binding.rs
- native/hagency-execution/src/lib.rs
- native/hagency-execution/src/host.rs
- native/hagency-execution/src/factory.rs
- native/hagency-execution/src/warm.rs
- native/hagency-execution/src/operation.rs
- native/hagency-store/src/domain.rs
- native/hagency-store/tests/owned_claim.rs
- native/hagency/src/bootstrap/config.rs
- native/hagency/src/setup.rs
- native/hagency/src/main.rs
- native/hagency/src/lib.rs
- native/hagency/src/console/setup.rs
- native/hagency/tests/setup.rs
- native/hagency/tests/console/setup_page.rs
- native/hagency/tests/inline_factory.rs
- native/hagency/tests/inline_factory/mod.rs
- mockup/app/setup/page.jsx
- mockup/app/globals.css
- mockup/lib/i18n.js
- mockup/lib/native-api.js
- specs/task-rust-octos-setup-binding.spec.md

## Acceptance Criteria

Scenario: A profile's primary model is all Hagency reads of it
  Test: native_octos_profile_model_reads_the_primary_only
  Level: unit
  Test Double: fixed profile documents holding a fake key
  Given profiles with a primary, a sub-account, a partial or missing primary, another profile's ID and a file over the bound
  When the primary is read
  Then only a whole primary of the named top-level profile is read, and the key is never kept

Scenario: The Octos binding passes the allowlist only and admits only a matching profile
  Test: native_local_octos_binding
  Test: native_local_binding_serves_its_own_runner
  Level: unit
  Test Double: temporary user home and Octos home
  Production caller: hagency_execution::factory::WarmHostPlan::octos_runtime
  Given an Octos home with an allowed and a disallowed profile
  When the binding builds the environment and admits resources
  Then only HOME, PATH, USER and TMPDIR are set, OCTOS_HOME only for another home, and provider keys are refused
  And only a resource on the binding's seat whose allowed profile still names its provider and model is admitted
  And a changed, missing or out-of-home profile file refuses the resource
  And an Octos home binds only an Octos host, and a Codex or Claude Code folder never binds one

Scenario: fleet-runtime.json names an Octos runtime with its binding
  Test: native_fleet_runtime_may_run_octos_only
  Test: native_fleet_runtime_refuses_an_unbound_or_malformed_octos_block
  Level: unit
  Test Double: fixture executables and folders
  Production caller: hagency::bootstrap::config::load_fleet_runtime
  Given an `octos` block alone or beside Codex and Claude Code, and blocks with no binding, another binding profile, no or invalid or repeated profile IDs, a missing home, a wrong digest or an unknown field
  When the fleet runtime loads
  Then a whole block loads with a private instances root, and every other block is refused

Scenario: Setup finds Octos, lists its profiles and writes the octos block
  Test: native_setup_writes_an_octos_runtime_serve_accepts
  Test: native_setup_follows_the_homebrew_octos_wrapper
  Test: native_setup_needs_a_qualified_octos_profile
  Test: native_setup_rewrite_keeps_the_chosen_octos
  Test: octos_wrappers_resolve_to_the_native_binary
  Test: octos_profiles_list_their_primary_models
  Level: integration
  Test Double: fake home with fake Octos profiles and binaries, hermetic PATH
  Production caller: hagency::setup::configure
  Given an Octos with a qualified and an unqualified profile, a Homebrew wrapper, an npm launcher and a release binary
  When Setup runs
  Then the block pins the native binary, names the home and allows the qualified profile only, and serve's loader accepts it
  And every profile is listed with its model and tier, and no key is printed
  And a named Octos without a qualified profile is refused, one found on PATH is left out with the reason, and a rewrite keeps the chosen binary

Scenario: The console's Setup step shows Octos and offers its allowed profiles
  Test: native_setup_offers_the_allowed_octos_profiles
  Test: native_setup_status_reports_agents_and_steps
  Test: octos_staleness_covers_the_binary_and_the_allowed_profiles
  Level: integration
  Test Double: console fixture on an imported fleet, fixture Octos binary and home
  Production caller: hagency::console::setup::offer
  Given a runtime that allows one of three profiles
  When the page reads its status and offers sources
  Then Octos shows its keys as assumed and every profile with its model and tier, and only the allowed qualified profile is offered
  And an offer creates one unpublished source that runs the profile, and any other profile, model, reasoning or framework is refused
  And outside a fleet Octos is only located, never run, and its profiles are not read

Scenario: The fleet runs, re-attaches and refuses Octos agents
  Test: native_provisioning_octos_agent_dispatch_and_reattach
  Level: integration
  Test Double: offline OUP peer launched by the fleet's Octos runtime, fake homeserver
  Production caller: hagency_execution::factory::WarmHostPlan::octos_runtime
  Given an Octos resource whose profile is allowed and matches
  When the agent is provisioned, runs a task, is re-attached after a restart and runs another
  Then no warm child starts, each task launches its own serve on the agent's workspace and instance directory with the allowlisted environment, and completes with Octos's reply
  And after the user changes the profile's model, the next task is refused before anything is launched

Scenario: A resource's Octos profile is fixed under its agents
  Test: native_octos_resource_profile_is_fixed_under_its_agents
  Level: integration
  Test Double: actual domain repository
  Given an Octos resource with an active agent
  When the operator edits it
  Then a profile change is refused and a ceiling change is kept

## Decisions

ADR-193 decisions 7 and 8 define this slice. Where the ADR is silent, the
slice takes the narrower choice:

- The `local_octos` binding is required: admission cannot check a profile
  without the user's Octos home.
- Sub-accounts are never allowed or run.
- Each profile gets its own source, preset `local_octos_<profile>`, on the
  binding's seat; claims match the seat, as for Codex and Claude Code.
- Setup keeps an Octos binary and home the runtime already pins, so a stable
  install the operator chose is not replaced by another `octos` on `PATH`.
- Octos agents get the fleet's file limit but no file tools: those need the
  task helper, and Octos's task tools are host tools (slice 3).
- `zai-coding` / `glm-5.3-flash` is qualified at `medium` for the operator's
  confirmation.
