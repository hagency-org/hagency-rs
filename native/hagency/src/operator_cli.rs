//! Operator account/fleet CLI verbs that drive the RUNNING service's own
//! store owner over loopback (task #28), instead of opening the state
//! directory a second time — which is what forced the service to be stopped.
//! Mirrors `inspect.rs`'s client: loopback-only address, the local operator
//! token read privately, one bounded hyper exchange, every refusal named the
//! same way the route names it. When `--listen` is absent, `main` still calls
//! the offline `bootstrap::accounts` / `bootstrap::registration` writers.

use crate::bootstrap::accounts::Command;
use hagency_store::{AccountChoice, AccountReadinessMode, LoginOutcome, LoginVerdict, private};
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, body::Bytes, client::conn::http1};
use hyper_util::rt::TokioIo;
use std::{
    collections::BTreeMap, net::SocketAddr, path::Path, process::Command as StdCommand,
    time::Duration,
};
use tokio::net::TcpStream;

/// Refusal classes with DISTINCT exit codes — the same shape as `inspect`:
/// 3 unreachable, 4 refused, 5 invalid request, 6 busy/unavailable, 7 route
/// not present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Unreachable,
    Refused,
    Invalid,
    Unavailable,
    Missing,
}
impl Error {
    pub fn exit_code(self) -> i32 {
        match self {
            Error::Unreachable => 3,
            Error::Refused => 4,
            Error::Invalid => 5,
            Error::Unavailable => 6,
            Error::Missing => 7,
        }
    }
    pub fn describe(self) -> &'static str {
        match self {
            Error::Unreachable => "service unreachable",
            Error::Refused => "operator authority refused",
            Error::Invalid => "invalid operator request",
            Error::Unavailable => "service busy or unavailable",
            Error::Missing => "route not present",
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.describe())
    }
}
impl std::error::Error for Error {}

pub(crate) fn token(state: &Path) -> Result<String, Error> {
    let bytes = private::read_secret(&state.join("operator.token")).map_err(|_| Error::Refused)?;
    let token = std::str::from_utf8(&bytes).map_err(|_| Error::Refused)?;
    if !(32..=256).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Error::Refused);
    }
    Ok(token.to_owned())
}

fn authorization(token: &str) -> Result<hyper::header::HeaderValue, Error> {
    let mut value = hyper::header::HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| Error::Invalid)?;
    value.set_sensitive(true);
    Ok(value)
}

/// One bounded operator exchange (GET or POST with an optional JSON body).
/// The address must be loopback; the token is read from the private state
/// directory and never logged.
pub(crate) async fn exchange(
    method: Method,
    path: &str,
    address: SocketAddr,
    token: &str,
    body: Option<Vec<u8>>,
) -> Result<(u16, Vec<u8>), Error> {
    if !address.ip().is_loopback()
        || address.port() == 0
        || matches!(address, SocketAddr::V6(v) if v.scope_id() != 0 || v.flowinfo() != 0)
    {
        return Err(Error::Invalid);
    }
    tokio::time::timeout(Duration::from_secs(5), async move {
        let stream = TcpStream::connect(address)
            .await
            .map_err(|_| Error::Unreachable)?;
        let (mut sender, connection) = http1::Builder::new()
            .max_headers(32)
            .max_buf_size(64 * 1024)
            .handshake::<_, Full<Bytes>>(TokioIo::new(stream))
            .await
            .map_err(|_| Error::Unreachable)?;
        let builder = Request::builder()
            .method(method)
            .uri(path)
            .header("host", address.to_string())
            .header("authorization", authorization(token)?);
        let request = if let Some(body) = body {
            builder
                .header("content-type", "application/json")
                .body(Full::<Bytes>::from(body))
                .map_err(|_| Error::Invalid)?
        } else {
            builder
                .body(Full::<Bytes>::default())
                .map_err(|_| Error::Invalid)?
        };
        tokio::pin!(connection);
        let read = async {
            let response = sender
                .send_request(request)
                .await
                .map_err(|_| Error::Unreachable)?;
            let status = response.status();
            let collected = response
                .into_body()
                .collect()
                .await
                .map_err(|_| Error::Unavailable)?;
            let bytes = collected.to_bytes();
            if bytes.len() > 512 * 1024 {
                return Err(Error::Unavailable);
            }
            Ok((status.as_u16(), bytes.to_vec()))
        };
        tokio::pin!(read);
        tokio::select! {
            result = &mut read => result,
            result = &mut connection => { result.map_err(|_| Error::Unreachable)?; read.await }
        }
    })
    .await
    .map_err(|_| Error::Unreachable)?
}

fn decode_status(status: u16) -> Result<(), Error> {
    match status {
        200 => Ok(()),
        400 => Err(Error::Invalid),
        404 => Err(Error::Missing),
        401 | 403 => Err(Error::Refused),
        _ => Err(Error::Unavailable),
    }
}

fn choices_from(body: &[u8]) -> Result<Vec<AccountChoice>, Error> {
    serde_json::from_slice(body).map_err(|_| Error::Invalid)
}

/// The account verbs, driven through the running service's operator routes.
/// The return shape matches the offline writer exactly — a vector of choices.
pub async fn accounts(
    state: &Path,
    address: SocketAddr,
    command: Command,
) -> Result<Vec<AccountChoice>, Error> {
    let token = token(state)?;
    match command {
        Command::Prepare { profile } => {
            let (status, body) = exchange(
                Method::POST,
                "/api/native/v1/accounts",
                address,
                &token,
                Some(
                    serde_json::json!({"profile": profile})
                        .to_string()
                        .into_bytes(),
                ),
            )
            .await?;
            decode_status(status)?;
            Ok(vec![
                serde_json::from_slice(&body).map_err(|_| Error::Invalid)?,
            ])
        }
        Command::Inspect => {
            let (status, body) = exchange(
                Method::GET,
                "/api/native/v1/accounts",
                address,
                &token,
                None,
            )
            .await?;
            decode_status(status)?;
            choices_from(&body)
        }
        Command::Retire { id } => {
            let (status, body) = exchange(
                Method::POST,
                &format!("/api/native/v1/accounts/{id}/retire"),
                address,
                &token,
                None,
            )
            .await?;
            decode_status(status)?;
            Ok(vec![
                serde_json::from_slice(&body).map_err(|_| Error::Invalid)?,
            ])
        }
        Command::Login {
            id,
            login_binary,
            device_auth,
        } => {
            // Begin through the service: it allocates the attempt and hands
            // back the retained HOME/CODEX_HOME the provider child runs under.
            let (status, body) = exchange(
                Method::POST,
                &format!("/api/native/v1/accounts/{id}/login-begin"),
                address,
                &token,
                None,
            )
            .await?;
            decode_status(status)?;
            let begin: serde_json::Value =
                serde_json::from_slice(&body).map_err(|_| Error::Invalid)?;
            let attempt = begin.get("attempt").cloned().ok_or(Error::Invalid)?;
            let home = begin.get("home").and_then(|v| v.as_str());
            let codex_home = begin.get("codexHome").and_then(|v| v.as_str());
            // Spawn the provider child with HOME/CODEX_HOME set exactly as the
            // service resolved them — nothing else reaches the child, the same
            // environment discipline as the offline writer.
            let mut environment: BTreeMap<std::ffi::OsString, std::ffi::OsString> = BTreeMap::new();
            if let Some(home) = home {
                environment.insert("HOME".into(), home.into());
            }
            if let Some(codex_home) = codex_home {
                environment.insert("CODEX_HOME".into(), codex_home.into());
            }
            let mut child = StdCommand::new(&login_binary);
            child.arg("login");
            if device_auth {
                child.arg("--device-auth");
            }
            let exit = child.env_clear().envs(&environment).status();
            let verdict = match exit {
                Ok(status) if status.success() => LoginVerdict {
                    mode: AccountReadinessMode::Subscription,
                    provider_state: "logged-in-subscription".into(),
                    outcome: LoginOutcome::Observed,
                    expires_at_ms: None,
                },
                Ok(status) if status.code() == Some(1) => LoginVerdict {
                    mode: AccountReadinessMode::Unknown,
                    provider_state: "login-refused".into(),
                    outcome: LoginOutcome::Refused,
                    expires_at_ms: None,
                },
                _ => LoginVerdict {
                    mode: AccountReadinessMode::Unknown,
                    provider_state: "login-unknown".into(),
                    outcome: LoginOutcome::Uncertain,
                    expires_at_ms: None,
                },
            };
            let settle = serde_json::json!({"attempt": attempt, "verdict": verdict}).to_string();
            let (status, _) = exchange(
                Method::POST,
                &format!("/api/native/v1/accounts/{id}/login-settle"),
                address,
                &token,
                Some(settle.into_bytes()),
            )
            .await?;
            decode_status(status)?;
            // The offline writer returns the post-settle choices; mirror that.
            let (status, body) = exchange(
                Method::GET,
                "/api/native/v1/accounts",
                address,
                &token,
                None,
            )
            .await?;
            decode_status(status)?;
            choices_from(&body)
        }
    }
}

/// The fleet registration verb, driven through the running service's
/// `POST /api/native/v1/project-sides` route (the same store contract the
/// offline `registration register --file` writer and the console route use).
pub async fn registration(state: &Path, address: SocketAddr, file: &Path) -> Result<(), Error> {
    let token = token(state)?;
    let raw = if file.as_os_str() == "-" {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
            .map_err(|_| Error::Invalid)?;
        buf
    } else {
        std::fs::read_to_string(file).map_err(|_| Error::Invalid)?
    };
    // Validate the document shape client-side before POSTing, so a malformed
    // file is refused with a distinct invalid-request class — but the store's
    // own contract (generation, identical-content no-op) still runs server-side.
    let registration: hagency_core::authority::Registration =
        serde_json::from_str(&raw).map_err(|_| Error::Invalid)?;
    let (status, _) = exchange(
        Method::POST,
        "/api/native/v1/project-sides",
        address,
        &token,
        Some(serde_json::to_vec(&registration).map_err(|_| Error::Invalid)?),
    )
    .await?;
    decode_status(status)
}
