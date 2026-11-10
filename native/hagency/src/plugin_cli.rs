use clap::Subcommand;
use hagency_plugin::{PluginCommand, PluginSource};
use hyper::Method;
use serde_json::Value;
use std::{net::SocketAddr, path::Path, time::Duration};

#[derive(Debug, Subcommand)]
pub enum Command {
    List,
    Inspect {
        id: String,
    },
    Preview {
        source: String,
    },
    Install {
        source: String,
        trust_digest: String,
    },
    Link {
        path: String,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    Uninstall {
        id: String,
    },
    Action {
        id: String,
        action_id: String,
        #[arg(default_value = "{}")]
        input: String,
    },
    QualifyRuntime {
        id: String,
        adapter_id: String,
        framework: String,
        model: String,
        provider: String,
        #[arg(long)]
        reasoning: Option<String>,
        #[arg(long, value_delimiter = ',')]
        roles: Vec<String>,
    },
    Logs {
        id: String,
    },
    ConfigDir {
        id: String,
    },
}

async fn request(
    state: &Path,
    address: SocketAddr,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value, Box<dyn std::error::Error>> {
    let token = crate::operator_cli::token(state)?;
    let body = body.map(|value| serde_json::to_vec(&value)).transpose()?;
    let (status, bytes) =
        crate::operator_cli::exchange(method, path, address, &token, body).await?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("plugin service returned invalid JSON: {error}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("plugin service refused request: {value}").into());
    }
    Ok(value)
}

pub async fn run(
    state: &Path,
    address: SocketAddr,
    command: Command,
) -> Result<Value, Box<dyn std::error::Error>> {
    match command {
        Command::List => request(state, address, Method::GET, "/api/native/v1/plugins", None).await,
        Command::Inspect { id } => {
            request(
                state,
                address,
                Method::GET,
                &format!("/api/native/v1/plugins/{id}"),
                None,
            )
            .await
        }
        Command::Preview { source } => {
            let source = parse_source(&source)?;
            request(
                state,
                address,
                Method::POST,
                "/api/native/v1/plugins/preview",
                Some(serde_json::json!({"source": source})),
            )
            .await
        }
        Command::Install {
            source,
            trust_digest,
        } => {
            let source = parse_source(&source)?;
            request(
                state,
                address,
                Method::POST,
                "/api/native/v1/plugins/install",
                Some(serde_json::json!({
                    "source": source,
                    "trustDigest": trust_digest
                })),
            )
            .await
        }
        Command::Link { path } => {
            request(
                state,
                address,
                Method::POST,
                "/api/native/v1/plugins/link",
                Some(serde_json::json!({"path": path})),
            )
            .await
        }
        Command::Enable { id } => {
            request(
                state,
                address,
                Method::POST,
                &format!("/api/native/v1/plugins/{id}/enable"),
                None,
            )
            .await
        }
        Command::Disable { id } => {
            request(
                state,
                address,
                Method::POST,
                &format!("/api/native/v1/plugins/{id}/disable"),
                None,
            )
            .await
        }
        Command::Uninstall { id } => {
            request(
                state,
                address,
                Method::POST,
                &format!("/api/native/v1/plugins/{id}/uninstall"),
                None,
            )
            .await
        }
        Command::Action {
            id,
            action_id,
            input,
        } => {
            let input: Value = serde_json::from_str(&input)?;
            let key = format!("cli:{action_id}:{}", serde_json::to_string(&input)?);
            let accepted = request(
                state,
                address,
                Method::POST,
                &format!("/api/native/v1/plugins/{id}/action"),
                Some(serde_json::json!({"actionId": action_id, "input": input, "idempotencyKey": key})),
            )
            .await?;
            let invocation_id = accepted
                .get("invocationId")
                .and_then(Value::as_str)
                .ok_or("plugin action response omitted invocationId")?
                .to_owned();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
            loop {
                let value = request(
                    state,
                    address,
                    Method::GET,
                    &format!("/api/native/v1/plugins/invocations/{invocation_id}"),
                    None,
                )
                .await?;
                match value.get("state").and_then(Value::as_str) {
                    Some("succeeded" | "failed" | "outcome_unknown") => return Ok(value),
                    _ if tokio::time::Instant::now() < deadline => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    _ => return Err("plugin action polling deadline expired".into()),
                }
            }
        }
        Command::QualifyRuntime {
            id,
            adapter_id,
            framework,
            model,
            provider,
            reasoning,
            roles,
        } => {
            let plugin = request(
                state,
                address,
                Method::GET,
                &format!("/api/native/v1/plugins/{id}"),
                None,
            )
            .await?;
            let package_sha256 = plugin
                .get("activePackageSha256")
                .and_then(Value::as_str)
                .or_else(|| plugin.get("packageSha256").and_then(Value::as_str))
                .ok_or("plugin inspect omitted packageSha256")?;
            let settings_sha256 = plugin
                .get("settingsSha256")
                .and_then(Value::as_str)
                .ok_or("plugin inspect omitted settingsSha256")?;
            request(
                state,
                address,
                Method::POST,
                &format!("/api/native/v1/plugins/{id}/qualify"),
                Some(serde_json::json!({
                    "runtime": {
                        "pluginId": id,
                        "adapterId": adapter_id,
                        "packageSha256": package_sha256,
                        "settingsSha256": settings_sha256,
                    },
                    "framework": framework,
                    "model": model,
                    "provider": provider,
                    "reasoning": reasoning,
                    "roles": roles,
                })),
            )
            .await
        }
        Command::Logs { id } => {
            request(
                state,
                address,
                Method::GET,
                &format!("/api/native/v1/plugins/{id}/logs"),
                None,
            )
            .await
        }
        Command::ConfigDir { id } => {
            request(
                state,
                address,
                Method::GET,
                &format!("/api/native/v1/plugins/{id}/config-dir"),
                None,
            )
            .await
        }
    }
}

fn parse_source(value: &str) -> Result<PluginSource, Box<dyn std::error::Error>> {
    if value.starts_with('{') {
        return Ok(serde_json::from_str(value)?);
    }
    Ok(PluginSource::linked(value)?)
}

#[allow(dead_code)]
fn command_name(command: &Command) -> PluginCommand {
    match command {
        Command::List => PluginCommand::List,
        Command::Inspect { id } => PluginCommand::Inspect(id.clone()),
        Command::Preview { source } => PluginCommand::Preview(source.clone()),
        Command::Install { source, .. } => PluginCommand::Install(source.clone()),
        Command::Link { path } => PluginCommand::Link(path.clone()),
        Command::Enable { id } => PluginCommand::Enable(id.clone()),
        Command::Disable { id } => PluginCommand::Disable(id.clone()),
        Command::Uninstall { id } => PluginCommand::Uninstall(id.clone()),
        Command::Action { id, .. } => PluginCommand::Action(id.clone()),
        Command::QualifyRuntime { id, .. } => PluginCommand::Qualify(id.clone()),
        Command::Logs { id } => PluginCommand::Logs(id.clone()),
        Command::ConfigDir { id } => PluginCommand::ConfigDir(id.clone()),
    }
}
