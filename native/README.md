# Hagency Rust workspace

This directory holds the Rust workspace that builds `hagency`, the single binary that runs the Hagency service. The [top-level README](../README.md) covers setup, operation and the security posture. The [code walkthrough](../docs/architecture-walkthrough.md) follows each flow through the code. This page is a map of the workspace.

## Build and test

Run these commands from the repository root. The workspace manifest is [Cargo.toml](../Cargo.toml), and [rust-toolchain.toml](../rust-toolchain.toml) pins the toolchain.

```bash
cargo build --release --locked -p hagency
cargo test --workspace --all-targets --locked -- \
  --skip native_codex_real_app_server \
  --skip native_two_agent_qualification_records_its_evidence
```

The two `--skip` flags drop the tests that need a real qualified host (ADR-140, ADR-144). Without them, those two tests fail on an ordinary machine. CI passes the same flags (see below).

This build has no console. To embed one (ADR-189), build the console first and name its absolute path in `HAGENCY_CONSOLE_DIR`:

```bash
(cd mockup && npm ci)
node mockup/scripts/build-native-console.mjs --output /abs/path/console
HAGENCY_CONSOLE_DIR=/abs/path/console cargo build --release --locked -p hagency
```

[hagency/build.rs](hagency/build.rs) includes every file of that directory in the binary and refuses a directory without `manifest.json`. At start the embedded files are checked against their manifest, as a console folder is. [release-native.yml](../.github/workflows/release-native.yml) builds release binaries this way.

To test one crate or one test target:

```bash
cargo test --locked -p hagency-store
cargo test --locked -p hagency --test invites
```

CI ([ci.yml](../.github/workflows/ci.yml)) runs the format and lint gates, the checkers and this test command:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
node native/scripts/check-rust-spec-bindings.mjs
node --test native/scripts/check-production-callers.test.mjs
node native/scripts/check-production-callers.mjs
cargo test --workspace --all-targets --locked --no-fail-fast -- \
  --skip native_codex_real_app_server \
  --skip native_two_agent_qualification_records_its_evidence
```

- `check-rust-spec-bindings.mjs` checks that every selector in [specs/](../specs/) names a real test.
- `check-production-callers.mjs` checks that every `Production caller:` line resolves in the production call graph. `check-production-callers.test.mjs` tests the checker itself.
- The two `--skip` flags drop the host-qualified tests. Their selectors stay in the specs, so the binding gate still checks them.

## The `hagency` binary

[hagency/src/main.rs](hagency/src/main.rs) defines every subcommand. Run `hagency --help` for the full list.

| Subcommand | Who runs it | What it does |
| --- | --- | --- |
| `start` | The operator, or the per-user service | Starts an imported fleet (ADR-189): uses the default state directory (`~/Library/Application Support/Hagency` on macOS, `${XDG_DATA_HOME:-~/.local/share}/hagency` on Linux) unless `--state-dir` is given, initializes it if new, and runs `serve --palpo-transport` with the embedded console. It prints a console sign-in link to an interactive terminal and opens it unless `--no-open`. `--console-assets` serves a console folder instead; without an embedded console it is required. |
| `service install`, `service uninstall` | The operator | Registers and starts a per-user service that runs `start --no-open` ([hagency/src/service.rs](hagency/src/service.rs)): a LaunchAgent `io.hagency` on macOS, logging to `~/Library/Logs/Hagency/hagency.log`, or a `systemd --user` unit `${XDG_CONFIG_HOME:-~/.config}/systemd/user/hagency.service` on Linux. It records the binary's resolved (canonical) path and the current `PATH`, so re-run it after replacing the binary. Re-running it restarts the service so the new binary runs: on macOS it boots the agent out and bootstraps it again; on Linux it runs `enable --now` and then `restart`. `uninstall` removes the service and keeps the state directory. |
| `serve` | The operator, or install-native.sh's units | The service with explicit flags. It listens on `127.0.0.1:13300` by default and refuses any address that is not loopback. [install/install-native.sh](../install/install-native.sh) installs it as a system unit: `--mode fleet` (the default) or `--mode coordinator`. Coordinator installs use `serve`. |
| `init` | The operator | Requires an empty or new directory; writes `operator.token` and creates both databases. |
| `setup` | The operator, or the installer in fleet mode | The CLI form of the console's Setup page, step 1. Prepares a state directory for an imported fleet ([hagency/src/setup.rs](hagency/src/setup.rs)): initializes it if new, finds Codex and its sign-in folder, writes `fleet-runtime.json` and validates it with `serve`'s loader. A running service loads the file only when it starts, so restart it after rewriting the file. Options: `--codex`, `--codex-home`, `--no-local-codex`, `--force` (keeps the old file as `.bak-<seconds>`), `--listen`, `--console-assets`. |
| `console-access` | The operator | Prints a console link that stays valid until a new one is printed |
| `engagements`, `resources`, `alerts` | The operator | Read-only views from the running service |
| `backup`, `restore`, `rotate` | The operator | Online backup, restore into an empty directory, and operator-token rotation |
| `account`, `registration`, `side-registration`, `provision`, `intake-refuse-stale-session` | The operator | Lower-level setup and repair commands. `account` and `registration register` drive a running service with `--listen`, or write offline without it. `registration probe` and `registration import` always run offline; stop the service before `import`. |
| `mcp` | Codex, as an MCP server | Gives an agent its task tools |
| `task` | An agent, from a shell | The same task operations as a CLI |
| `guardian` (hidden, Unix only) | `serve` | Owns one runner's process tree |

`serve` flags:

| Flag | Meaning |
| --- | --- |
| `--state-dir` | The state directory. Required. |
| `--listen` | The loopback address. The default is `127.0.0.1:13300`. |
| `--palpo-transport` | Run the Palpo transport and resource publication. With an imported fleet, also run the fleet service (ADR-187). With `--agent-driver`, the fleet service does not run. |
| `--agent-driver` | Run a coordinator install from `agent-driver.json` |
| `--development-driver` | Run one development attempt from `development-driver.json`. It cannot be combined with `--agent-driver`. |
| `--console-assets` | Optional. Serves this console directory instead of the embedded console. Without either, `serve` runs with no console. The directory is the one built by `mockup/scripts/build-native-console.mjs`. It must be owner-private (0700). The directory must not itself be a symlink and must have no symlinks inside it (no symlinked file or subdirectory); a symlinked ancestor, such as macOS `/tmp`, is accepted. Every file must match its `manifest.json` entry. Otherwise `serve` refuses to start. |
| `--queue-capacity` | The queue size of both writer threads, custody and domain. The default is 16. |

## Crates

| Crate | Role |
| --- | --- |
| [hagency](hagency/) | The binary: startup, fleet service, console routes, operator API, runner API, MCP helper |
| [hagency-core](hagency-core/) | Domain types and rules, with no IO |
| [hagency-store](hagency-store/) | SQLite storage, each database on its own writer thread. Holds every durable rule and the schema migrations. |
| [hagency-matrix](hagency-matrix/) | Matrix sync, encryption, sends, provisioning, invites and the approval bot |
| [hagency-matrix-format](hagency-matrix-format/) | Matrix message formatting, with no IO |
| [hagency-palpo](hagency-palpo/) | The outbound Palpo fleet API client and resource publication |
| [hagency-execution](hagency-execution/) | The dispatch host: Codex runs, approvals and usage binding |
| [hagency-runtime](hagency-runtime/) | The Codex app-server protocol and owned child process IO |
| [hagency-platform](hagency-platform/) | The guardian, process groups and Windows Job Objects |
| [hagency-files](hagency-files/), [hagency-media](hagency-media/), [hagency-media-store](hagency-media-store/) | Workspace file snapshots, attachment encryption, and private media storage |
| [hagency-metering](hagency-metering/) | Normalizes runtime usage reports |
| [hagency-permissions](hagency-permissions/), [hagency-progress](hagency-progress/), [hagency-progress-runtime](hagency-progress-runtime/), [hagency-crypto-proof](hagency-crypto-proof/) | Not used by the binary. Only their own tests run them. |

Most library crates state their role and limits in a `//!` comment at the top of `src/lib.rs`.

[fixtures/](fixtures/) holds shared JSON test fixtures. [scripts/](scripts/) holds the CI checkers and Linux qualification helpers.

## Where to start reading

| Area | Files |
| --- | --- |
| Startup and shutdown | `Bootstrap::open_with_options`, `Bootstrap::serve` and `Bootstrap::close` in [hagency/src/bootstrap.rs](hagency/src/bootstrap.rs) |
| Start and service (ADR-189) | `Command::Start` and `serve` in [hagency/src/main.rs](hagency/src/main.rs); `default_state_dir`, `announce` and the service units in [hagency/src/service.rs](hagency/src/service.rs); the embedded console in [hagency/build.rs](hagency/build.rs) and [hagency/src/console/assets.rs](hagency/src/console/assets.rs) |
| Configuration files | [hagency/src/bootstrap/config.rs](hagency/src/bootstrap/config.rs): `FleetRuntimeConfig` (`fleet-runtime.json`) and `Config` (`agent-driver.json`). [hagency/src/setup.rs](hagency/src/setup.rs) writes `fleet-runtime.json` and detects Codex (`detect_codex`). |
| Setup page (ADR-189) | [hagency/src/console/setup.rs](hagency/src/console/setup.rs) serves `GET /console/api/setup`, `POST /console/api/setup/check` and `POST /console/api/setup/resource`. The page is [mockup/app/setup/page.jsx](../mockup/app/setup/page.jsx); [mockup/components/SetupBanner.jsx](../mockup/components/SetupBanner.jsx) links every other console page to it until setup is done. |
| Palpo import | [hagency/src/bootstrap/palpo_import.rs](hagency/src/bootstrap/palpo_import.rs) parses independent engagement profiles. [hagency/src/console/palpo_import.rs](hagency/src/console/palpo_import.rs) serves `POST /console/api/palpo/import`. The first profile keeps its legacy location; additional profiles use `palpo-engagements/<fleet-id>/`. |
| Delegated coordinator decisions (ADR-191) | [hagency-store/src/domain/coordinator.rs](hagency-store/src/domain/coordinator.rs) reserves contributions and applies project, agent and top-up decisions. [hagency/src/console/server_engagements.rs](hagency/src/console/server_engagements.rs) lists profiles/contributions and accepts authenticated owner contributions. |
| Fleet service (ADR-187) | [hagency/src/bootstrap/fleet_service.rs](hagency/src/bootstrap/fleet_service.rs) runs the stages and the provisioning loop. [hagency/src/bootstrap/fleet_identity.rs](hagency/src/bootstrap/fleet_identity.rs) creates the fleet's accounts, keys and per-owner approval devices, and pins owner keys. |
| Invites and joined rooms (ADR-188) | [hagency/src/bootstrap/invites.rs](hagency/src/bootstrap/invites.rs) polls each agent's invites. [hagency-matrix/src/provisioning/factory.rs](hagency-matrix/src/provisioning/factory.rs) evaluates joined rooms and posts the encrypted-room notice. |
| Message admission | `admit_matrix_input` in [hagency-store/src/domain/verified_ingress.rs](hagency-store/src/domain/verified_ingress.rs) decides which messages wake an agent |
| Approvals | [hagency/src/bootstrap/approval.rs](hagency/src/bootstrap/approval.rs) and [hagency-execution/src/approval/](hagency-execution/src/approval/) |
| Console routes | [hagency/src/console.rs](hagency/src/console.rs) and [hagency/src/console/](hagency/src/console/) |

## Storage

`hagency init` creates two SQLite databases in the state directory:
- `domain.sqlite3` holds domain state: resources, engagements, tasks, approvals, rooms and usage.
- `custody.sqlite3` holds Palpo transport custody.

The domain schema version is `DOMAIN_SCHEMA_VERSION` in [hagency-store/src/domain.rs](hagency-store/src/domain.rs), currently 61. Migrations live in [hagency-store/src/migrations/](hagency-store/src/migrations/). A file's number is not always its schema version. Version 60 applies `074-joined-rooms.sql`; version 61 applies `075-coordinator-engagements.sql`. The list in `domain.rs` maps each version to its file.

Recent tables:
- `owner_anchors` (version 59) holds each owner's pinned cross-signing master key (ADR-187).
- `joined_rooms` (version 60) holds the rooms an agent joined by invitation, with their state: `working`, `encrypted_shared` or `retired` (ADR-188).
- The six `coordinator_*` tables (version 61) hold delegation, bounded contributions, approved projects, child allocations, replay receipts and durable projections (ADR-191). This is an integration slice; see ADR-191's remaining acceptance work before enabling it for existing users.

## Decisions and contracts

- [knowledge/decisions/](../knowledge/decisions/) holds the architecture decision records. ADR-187, ADR-188 and ADR-189 cover the newest behaviour.
- [specs/](../specs/) holds the task contracts that bind behaviour to tests.
- Code comments that cite `backend-v2.js`, `bridge-matrix.js` or `lib/*.js` refer to the earlier JavaScript implementation that this service replaced. Those files are in git history.
