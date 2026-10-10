#[path = "mcp/stdio.rs"]
mod mcp_stdio;
use clap::{Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(
    version,
    about = "Hagency native migration runtime (isolated development state)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Request a Matrix server association from this authenticated resource owner.
    Association(hagency::bootstrap::association::Args),
    /// Inspect/adopt legacy allocations with the runtime stopped.
    CoordinatorMigration {
        #[arg(long)]
        state_dir: PathBuf,
        #[command(subcommand)]
        command: hagency::bootstrap::coordinator_migration::Command,
    },
    /// Serve scoped task tools over MCP stdio using inherited runner context.
    Mcp {
        /// Expose only the fixed owned-runner maintenance and enabled file tools.
        #[arg(long)]
        owned_task_profile: bool,
    },
    /// Maintain the canonical task from the host-provisioned runner environment.
    Task {
        /// Stable identifier for a mutation; reuse only with identical content.
        #[arg(long, global = true)]
        call_id: Option<String>,
        #[command(subcommand)]
        command: hagency::task_client::Command,
    },
    /// Internal native guardian. Requires a private inherited channel on stdin.
    #[cfg(unix)]
    #[command(hide = true)]
    Guardian,
    /// Initialize fresh state. Requires an empty, private directory or a new path.
    Init {
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Prepare a state directory for an imported Palpo fleet: initialize it
    /// if new, find Codex and Claude Code and write a validated
    /// fleet-runtime.json.
    Setup {
        #[arg(long)]
        state_dir: PathBuf,
        /// The address `serve` will listen on (loopback).
        #[arg(long, default_value = "127.0.0.1:13300")]
        listen: SocketAddr,
        /// The Codex executable; found on PATH when omitted.
        #[arg(long)]
        codex: Option<PathBuf>,
        /// The folder holding the Codex sign-in; $CODEX_HOME or ~/.codex when omitted.
        /// Not used with --no-local-codex.
        #[arg(long)]
        codex_home: Option<PathBuf>,
        /// Run agents with <state>/runtime-home instead of this machine's Codex sign-in.
        #[arg(long)]
        no_local_codex: bool,
        /// Leave Codex out.
        #[arg(long)]
        no_codex: bool,
        /// The Claude Code executable; found on PATH when omitted.
        #[arg(long)]
        claude: Option<PathBuf>,
        /// Claude Code's own folder; $CLAUDE_CONFIG_DIR or ~/.claude when omitted.
        #[arg(long)]
        claude_config_dir: Option<PathBuf>,
        /// Leave Claude Code out.
        #[arg(long)]
        no_claude: bool,
        /// Replace an existing fleet-runtime.json (the old file is kept as a backup).
        #[arg(long)]
        force: bool,
        /// The console build, only to print the exact serve command.
        #[arg(long)]
        console_assets: Option<PathBuf>,
    },
    /// Prepare or inspect fresh host-owned Codex credential namespaces.
    /// With --listen the command drives the RUNNING service's operator API;
    /// without it, the offline store writer is used (service must be stopped).
    Account {
        // clap forbids required global arguments; the account commands accept
        // --state-dir (and --listen) before or after their verb.
        #[arg(long, global = true)]
        state_dir: Option<PathBuf>,
        /// Drive the running service at this loopback address instead of
        /// opening the state directory a second time.
        #[arg(long, global = true)]
        listen: Option<SocketAddr>,
        #[command(subcommand)]
        command: hagency::bootstrap::accounts::Command,
    },
    /// Register the fleet before first serve (G11): the trusted-local writer of
    /// the registrations row the provisioning ingress requires.
    Registration {
        #[arg(long, global = true)]
        state_dir: Option<PathBuf>,
        /// Drive the running service at this loopback address instead of
        /// opening the state directory a second time.
        #[arg(long, global = true)]
        listen: Option<SocketAddr>,
        #[command(subcommand)]
        command: hagency::bootstrap::registration::Command,
    },
    /// Issue a project side's appservice registration (task #13): random
    /// tokens, the YAML under <state>/registrations/, and the stored
    /// credential the appservice profile reads — no hand-placed files.
    SideRegistration {
        #[arg(long)]
        state_dir: PathBuf,
        /// The project side — its Matrix server name.
        #[arg(long)]
        side: String,
        /// The address this side's homeserver reaches Hagency at; cannot be
        /// derived, only asked.
        #[arg(long)]
        url: String,
        #[arg(long)]
        registration_id: Option<String>,
        #[arg(long)]
        sender_localpart: Option<String>,
        #[arg(long)]
        user_namespace: Option<String>,
        /// true unless explicitly false, matching the TS body contract.
        #[arg(long)]
        exclusive: Option<bool>,
    },
    /// Admit an externally created agent only after fresh authenticated Matrix observations.
    Provision {
        #[arg(long, global = true)]
        state_dir: Option<PathBuf>,
        #[command(subcommand)]
        command: hagency::bootstrap::provision::Command,
    },
    /// Explicitly reject exact known pre-session SDK custody; never retry work.
    IntakeRefuseStaleSession {
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long)]
        batch_digest: String,
    },
    /// Print the console link using local operator authority. One link opens
    /// the whole console: the retained `createApiAuthMiddleware` admitted one
    /// credential to every `/api` route, so there is no scope to select.
    ConsoleAccess {
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long, default_value = "127.0.0.1:13300")]
        listen: SocketAddr,
    },
    /// Read-only inspection: open ceiling overrun alerts from the running service.
    Alerts {
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long, default_value = "127.0.0.1:13300")]
        listen: SocketAddr,
        /// Page size; the route refuses out-of-range values, never clamps.
        #[arg(long, default_value_t = 100)]
        limit: u32,
        /// Print the route's body verbatim instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Read-only inspection: engagements from the running service.
    Engagements {
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long, default_value = "127.0.0.1:13300")]
        listen: SocketAddr,
        /// Page size; the route refuses out-of-range values, never clamps.
        #[arg(long, default_value_t = 100)]
        limit: u32,
        /// Print the route's body verbatim instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Read-only inspection: the resource catalog from the running service.
    Resources {
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long, default_value = "127.0.0.1:13300")]
        listen: SocketAddr,
        /// Page size; the route refuses out-of-range values, never clamps.
        #[arg(long, default_value_t = 100)]
        limit: u32,
        /// Print the route's body verbatim instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Snapshot an initialized state directory into a new private directory.
    /// Uses SQLite's online backup, so it runs while `serve` is up.
    Backup {
        #[arg(long)]
        state_dir: PathBuf,
        /// New directory to write; refused if it already exists.
        #[arg(long)]
        out: PathBuf,
    },
    /// Restore a snapshot into an empty state directory. Never overwrites.
    Restore {
        #[arg(long)]
        state_dir: PathBuf,
        /// The snapshot directory to restore from.
        #[arg(long)]
        from: PathBuf,
    },
    /// Mint a replacement for a locally-held credential.
    Rotate {
        // clap forbids required global arguments; like the offline account
        // commands, this accepts --state-dir before or after its verb and
        // refuses without it.
        #[arg(long, global = true)]
        state_dir: Option<PathBuf>,
        #[command(subcommand)]
        command: hagency::ops::rotate::Command,
    },
    /// Run the isolated native API. Does not load .env or any existing Hagency state.
    Serve {
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long, default_value = "127.0.0.1:13300")]
        listen: SocketAddr,
        #[arg(long, default_value_t = 16)]
        queue_capacity: usize,
        /// Execute one configured development attempt after authenticated Matrix refresh.
        #[arg(long, conflicts_with = "agent_driver")]
        development_driver: bool,
        /// Continuously collect Matrix work and execute compatible owned dispatches.
        #[arg(long, conflicts_with = "development_driver")]
        agent_driver: bool,
        /// Run configured Palpo v2 custody lanes and native resource publication.
        #[arg(long)]
        palpo_transport: bool,
        /// Private validated static build of the retained native usage console.
        #[arg(long)]
        console_assets: Option<PathBuf>,
    },
    /// Start Hagency for an imported Palpo fleet (ADR-189): initialize the
    /// state directory if needed, serve the console and print its sign-in link.
    Start {
        /// Defaults to ~/Library/Application Support/Hagency (macOS) or
        /// ~/.local/share/hagency (Linux).
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long, default_value = "127.0.0.1:13300")]
        listen: SocketAddr,
        /// Do not open the console in the browser.
        #[arg(long)]
        no_open: bool,
        /// Serve the console from this folder instead of the embedded build.
        #[arg(long)]
        console_assets: Option<PathBuf>,
    },
    /// Register Hagency as a per-user service that runs `hagency start`.
    Service {
        #[command(subcommand)]
        command: hagency::service::Command,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let command = Cli::parse().command;
    #[cfg(unix)]
    if matches!(command, Command::Guardian) {
        hagency_platform::run_guardian()?;
        return Ok(());
    }
    if let Command::Mcp { owned_task_profile } = command {
        // The helper's exit code names its own refusal class, so a spawning
        // test can attribute a hosted load failure from the status alone,
        // without reading stderr (which is only drained after the exit).
        // Previously `?` propagated the `Err` to Rust's default handler,
        // which printed the same message but always exited 1.
        if let Err(error) = mcp_stdio::run_stdio(owned_task_profile) {
            eprintln!("Error: {error}");
            std::process::exit(error.exit_code());
        }
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    tracing::trace!(target: "hagency_startup_observation", "native startup boundary: runtime_entered");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    tracing::trace!(target: "hagency_startup_observation", "native startup boundary: runtime_ready");
    runtime.block_on(run(command))
}
async fn run(command: Command) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Command::Association(args) => {
            println!("{}", hagency::bootstrap::association::run(args).await?);
        }
        Command::CoordinatorMigration { state_dir, command } => {
            println!(
                "{}",
                hagency::bootstrap::coordinator_migration::run(&state_dir, command)?
            );
        }
        Command::Mcp { .. } => unreachable!("MCP runs on the dedicated main thread"),
        Command::Task { call_id, command } => {
            let context = hagency::task_client::Context::from_env()?;
            let output = hagency::task_client::run(
                &context,
                &command,
                call_id.as_deref(),
                hagency::task_client::DEFAULT_DEADLINE,
            )
            .await?;
            println!("{}", serde_json::to_string(&output)?);
        }
        #[cfg(unix)]
        Command::Guardian => return Err("guardian requires isolated synchronous startup".into()),
        Command::Init { state_dir } => {
            hagency::setup::init_state(&state_dir)?;
            println!(
                "Initialized native state. Operator token is in operator.token; keep it private."
            );
        }
        Command::Setup {
            state_dir,
            listen,
            codex,
            codex_home,
            no_local_codex,
            no_codex,
            claude,
            claude_config_dir,
            no_claude,
            force,
            console_assets,
        } => {
            let report = hagency::setup::run(&hagency::setup::Options {
                state_dir: state_dir.clone(),
                listen,
                codex,
                codex_home,
                no_local_codex,
                no_codex,
                claude,
                claude_config_dir,
                no_claude,
                force,
            })?;
            if report.initialized {
                println!(
                    "Initialized {} (operator token in operator.token; keep it private).",
                    state_dir.display()
                );
            }
            if let (Some(executable), Some(home)) = (&report.executable, &report.codex_home) {
                println!("Codex: {}", executable.display());
                if !report.signed_in {
                    println!(
                        "Codex is not signed in yet. Run:\n  CODEX_HOME={} codex login",
                        home.display()
                    );
                } else {
                    println!("Codex sign-in: {}", home.display());
                }
            }
            if let Some(claude) = &report.claude {
                // ADR-192 decision 6: Claude Code's sign-in is its own.
                println!("Claude Code: {} (uses its own sign-in)", claude.display());
            }
            if let Some(problem) = &report.claude_problem {
                println!("Claude Code left out: {problem}");
            }
            println!("Wrote and validated {}", report.runtime_file.display());
            let assets = console_assets
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<console-build>".into());
            println!(
                "\nNext:\n  1. hagency serve --palpo-transport --state-dir {state} --listen {listen} --console-assets {assets}\n  2. hagency console-access --state-dir {state} --listen {listen}   (open the printed link)\n  3. In the console, import the configuration you downloaded from Palpo.",
                state = state_dir.display(),
            );
        }
        Command::Account {
            state_dir,
            command,
            listen,
        } => {
            let state_dir = state_dir.ok_or("account commands require --state-dir")?;
            let result = match listen {
                // Task #28: drive the RUNNING service's operator API over
                // loopback (the service stays up); offline fallback otherwise.
                Some(address) => {
                    match hagency::operator_cli::accounts(&state_dir, address, command).await {
                        Ok(choices) => choices,
                        Err(error) => {
                            eprintln!("hagency: {}", error.describe());
                            std::process::exit(error.exit_code());
                        }
                    }
                }
                None => hagency::bootstrap::accounts::run(&state_dir, command)?,
            };
            println!("{}", serde_json::to_string(&result)?);
        }
        Command::Registration {
            state_dir,
            command,
            listen,
        } => {
            let state_dir = state_dir.ok_or("registration commands require --state-dir")?;
            match command {
                hagency::bootstrap::registration::Command::Register { file } => {
                    match listen {
                        Some(address) => {
                            if let Err(error) =
                                hagency::operator_cli::registration(&state_dir, address, &file)
                                    .await
                            {
                                eprintln!("hagency: {}", error.describe());
                                std::process::exit(error.exit_code());
                            }
                        }
                        None => {
                            hagency::bootstrap::registration::run(
                                &state_dir,
                                hagency::bootstrap::registration::Command::Register { file },
                            )?;
                        }
                    }
                    println!("{}", serde_json::json!({"ok": true}));
                }
                // integ's connection probe (#51): a local offline check —
                // no running-service route exists for it, --listen or not.
                hagency::bootstrap::registration::Command::Probe(args) => {
                    hagency::bootstrap::registration::run(
                        &state_dir,
                        hagency::bootstrap::registration::Command::Probe(args),
                    )?;
                    println!("{}", serde_json::json!({"ok": true}));
                }
                // Offline like `register`: the service must be stopped.
                command @ hagency::bootstrap::registration::Command::Import { .. } => {
                    hagency::bootstrap::registration::run(&state_dir, command)?;
                }
            }
        }
        Command::SideRegistration {
            state_dir,
            side,
            url,
            registration_id,
            sender_localpart,
            user_namespace,
            exclusive,
        } => {
            hagency::console::side_registration::run_cli(
                &state_dir,
                hagency_store::IssueSideRegistrationRequest {
                    side,
                    url,
                    registration_id,
                    sender_localpart,
                    user_namespace,
                    exclusive,
                },
            )?;
        }
        Command::Provision { state_dir, command } => {
            let state_dir = state_dir.ok_or("provision commands require --state-dir")?;
            let result = hagency::bootstrap::provision::run(&state_dir, command).await?;
            println!("{}", serde_json::to_string(&result)?);
        }
        Command::IntakeRefuseStaleSession {
            state_dir,
            batch_digest,
        } => {
            let rejected =
                hagency::bootstrap::intake_refusal::run(&state_dir, batch_digest).await?;
            println!(
                "{}",
                serde_json::json!({"rejected":rejected,"effects_retried":false})
            );
        }
        Command::Backup { state_dir, out } => {
            let manifest = hagency::ops::backup::snapshot(&state_dir, &out)?;
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "out": out,
                    "created_at_ms": manifest.created_at_ms,
                    "files": manifest.entries.len(),
                })
            );
        }
        Command::Restore { state_dir, from } => {
            let manifest = hagency::ops::backup::restore(&state_dir, &from)?;
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "from": from,
                    "created_at_ms": manifest.created_at_ms,
                    "files": manifest.entries.len(),
                })
            );
        }
        Command::Rotate { state_dir, command } => {
            let state_dir = state_dir.ok_or("rotate commands require --state-dir")?;
            let receipt = hagency::ops::rotate::run(&state_dir, command)?;
            println!("{}", serde_json::to_string(&receipt)?);
        }
        Command::Serve {
            state_dir,
            listen,
            queue_capacity,
            development_driver,
            agent_driver,
            palpo_transport,
            console_assets,
        } => {
            serve(
                &state_dir,
                listen,
                queue_capacity,
                hagency::bootstrap::Options {
                    development_driver,
                    agent_driver,
                    palpo_transport,
                },
                console_assets.as_deref(),
                None,
            )
            .await?;
        }
        Command::Start {
            state_dir,
            listen,
            no_open,
            console_assets,
        } => {
            let state_dir = match state_dir {
                Some(dir) => dir,
                None => hagency::service::default_state_dir()?,
            };
            if console_assets.is_none() && !hagency::console::Console::embedded_available() {
                return Err(
                    "this build has no embedded console; pass --console-assets <console-build>"
                        .into(),
                );
            }
            if !state_dir.join("operator.token").is_file() {
                hagency::setup::init_state(&state_dir)?;
                println!("Initialized {}", state_dir.display());
            }
            serve(
                &state_dir,
                listen,
                16,
                hagency::bootstrap::Options {
                    development_driver: false,
                    agent_driver: false,
                    palpo_transport: true,
                },
                console_assets.as_deref(),
                Some((state_dir.clone(), listen, !no_open)),
            )
            .await?;
        }
        Command::Service { command } => {
            println!("{}", hagency::service::run(command)?);
        }
        Command::ConsoleAccess { state_dir, listen } => {
            // One link, one login: every variant issued the same full-access
            // ticket (TS `createApiAuthMiddleware`), and the session it opens
            // carries every console action.
            println!(
                "{}",
                hagency::console::client::access(&state_dir, listen).await?
            );
        }
        // Read-only inspection (brief 22): each arm renders exactly what its
        // operator route returns and exits distinctly on refusal — never a
        // silent 0. No arm creates, verdicts, revokes or resolves anything.
        Command::Alerts {
            state_dir,
            listen,
            limit,
            json,
        } => {
            hagency::inspect::run(
                hagency::inspect::Kind::Alerts,
                &state_dir,
                listen,
                limit,
                json,
            )
            .await;
        }
        Command::Engagements {
            state_dir,
            listen,
            limit,
            json,
        } => {
            hagency::inspect::run(
                hagency::inspect::Kind::Engagements,
                &state_dir,
                listen,
                limit,
                json,
            )
            .await;
        }
        Command::Resources {
            state_dir,
            listen,
            limit,
            json,
        } => {
            hagency::inspect::run(
                hagency::inspect::Kind::Resources,
                &state_dir,
                listen,
                limit,
                json,
            )
            .await;
        }
    }
    Ok(())
}

/// The `serve` body shared by `serve` and `start`. With `announce`, the
/// console sign-in link is printed (and opened) once the service answers.
async fn serve(
    state_dir: &std::path::Path,
    listen: SocketAddr,
    queue_capacity: usize,
    options: hagency::bootstrap::Options,
    console_assets: Option<&std::path::Path>,
    announce: Option<(PathBuf, SocketAddr, bool)>,
) -> Result<(), Box<dyn std::error::Error>> {
    // The graph routes persist `task_graphs.json` beside `domain.sqlite3`.
    let console = match console_assets {
        Some(assets) => Some(
            hagency::console::Console::load_with_state(assets, Some(state_dir)).map_err(|_| {
                hagency::bootstrap::Failure::Config {
                    field: "--console-assets",
                    fix: "the directory must be the bundle built by mockup/scripts/build-native-console.mjs, owner-private (0700), not itself a symlink and with no symlinks inside it, with a manifest.json whose entries all match the files",
                }
            })?,
        ),
        None if hagency::console::Console::embedded_available() => Some(
            hagency::console::Console::embedded_with_state(Some(state_dir)).map_err(|_| {
                hagency::bootstrap::Failure::Config {
                    field: "embedded console",
                    fix: "this binary's embedded console does not match its manifest; rebuild it",
                }
            })?,
        ),
        None => None,
    };
    let mut bootstrap = hagency::bootstrap::Bootstrap::open_with_options(
        state_dir,
        listen,
        queue_capacity,
        options,
    )?;
    if let Some(console) = console {
        bootstrap = bootstrap.with_console(console);
    }
    let cancel = hagency_matrix::CancellationToken::new();
    let signal = cancel.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut termination =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("install SIGTERM handler");
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = termination.recv() => {} }
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
        signal.cancel();
    });
    if let Some((state, address, open)) = announce {
        tokio::spawn(hagency::service::announce(state, address, open));
    }
    if let Err(error) = bootstrap.serve(&cancel).await {
        tracing::error!("native server stopped; closing retained owners");
        if bootstrap.close().await.is_err() {
            // ADR-182: what could not be proven is in the store (an agent
            // fence, an unknown verdict); the process exits.
            tracing::error!(
                "native shutdown ended with an unknown verdict; see the agent fences and status"
            );
        }
        return Err(error.into());
    }
    Ok(())
}
