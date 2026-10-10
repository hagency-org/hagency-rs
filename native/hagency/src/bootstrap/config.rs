use super::{DriverMode, Failure};
use hagency_core::replies::{MatrixTransportObservation, RoomPrivacy};
use hagency_execution::{Host, Limits};
use hagency_matrix::{HostConfig, HostIdentity, HostRoom};
use hagency_store::{DomainRepository, OwnedClaimProfile, OwnedClaimRoom, private};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
};

const CONFIG_BYTES: usize = 16 * 1024;
const EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    profile: String,
    #[serde(default)]
    managed_account: Option<String>,
    #[serde(default)]
    local_codex: Option<LocalCodex>,
    #[serde(default)]
    send_file: bool,
    #[serde(default)]
    receive_file: bool,
    /// ADR180: let owned Codex dispatches call the coordination tools.
    #[serde(default)]
    coordination_tools: bool,
    #[serde(default)]
    receive_inbox: Option<hagency_core::received_files::ReceiveInboxPlan>,
    /// Existing verified Matrix session IDs whose timelines the continuous
    /// driver polls. An empty list permits host-queued dispatches only.
    #[serde(default)]
    intake_sessions: Vec<String>,
    /// Continuous agent sessions whose verified wake messages become owned
    /// tasks. Each route names the retained workspace used for its dispatch.
    #[serde(default)]
    agent_inboxes: Vec<hagency_core::agent_inbox::AgentInboxPlan>,
    executable: PathBuf,
    executable_sha256: String,
    #[serde(deserialize_with = "workspace_map")]
    workspaces: BTreeMap<String, PathBuf>,
    file_limit: usize,
    operation_ms: u64,
    response_ms: u64,
    #[serde(default = "default_approval_wait")]
    approval_owner_wait_ms: u64,
    #[serde(default)]
    matrix_request_interval_ms: Option<u64>,
    /// Original SDK/enrollment budget, selected before any Matrix owner exists.
    #[serde(default)]
    matrix_sdk_timeout_ms: Option<u64>,
    matrix: Matrix,
    #[serde(default)]
    approval: Option<ApprovalMatrix>,
    #[serde(default)]
    factory_service: Option<FactoryService>,
}
fn default_approval_wait() -> u64 {
    1000
}
fn matrix_limits(
    origin: &str,
    interval: Option<u64>,
    sdk_ms: Option<u64>,
) -> Result<hagency_matrix::Limits, Failure> {
    let mut limits = hagency_matrix::Limits {
        request_pacing: interval
            .map(|ms| {
                hagency_matrix::RequestPacing::new(origin, std::time::Duration::from_millis(ms))
                    .map(std::sync::Arc::new)
                    .map_err(|_| Failure::Config {
                        field: "agent-driver.json: matrix.pacing_ms",
                        fix: "the request pacing interval must be positive and fit the origin's budget",
                    })
            })
            .transpose()?,
        ..hagency_matrix::Limits::default()
    };
    if let Some(ms) = sdk_ms {
        if !(10..=60_000).contains(&ms) {
            return Err(Failure::Config {
                field: "agent-driver.json: matrix.sdk_ms",
                fix: "the SDK budget must be between 10 and 60000 milliseconds",
            });
        }
        limits.sdk = std::time::Duration::from_millis(ms);
    }
    Ok(limits)
}
fn approval_host(wait: u64, limits: Limits) -> Result<hagency_execution::ApprovalHost, Failure> {
    // After an unanswered owner wait the host still has to record the expiry
    // and send the decline (ADR046 amendment): one durable write, the existing
    // authorize/begin/check round trips and the frame all fit in this reserve.
    let reserve = limits.response_ms.max(5000);
    if wait
        .checked_add(reserve)
        .is_none_or(|n| n > limits.operation_ms)
    {
        return Err(Failure::Config {
            field: "agent-driver.json: approval.wait_ms",
            fix: "the owner wait plus the reply reserve must fit inside the operation budget",
        });
    }
    hagency_execution::ApprovalHost::new(8, 2, wait, reserve).map_err(|_| Failure::Config {
        field: "agent-driver.json: approval block",
        fix: "the approval host must construct from the wait and reserve budgets",
    })
}

#[cfg(test)]
mod approval_wait_tests {
    use super::*;
    #[test]
    fn native_matrix_ca_is_private_bounded_and_profile_scoped() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        private::directory(&first).unwrap();
        private::directory(&second).unwrap();
        assert!(matrix_root(&first).unwrap().is_none());
        let pem = include_bytes!("../../../hagency-matrix/tests/fixtures/ca.pem");
        let path = first.join("matrix.ca.pem");
        private::write_new(&path, pem).unwrap();
        assert_eq!(matrix_root(&first).unwrap().unwrap(), pem);
        assert!(matrix_root(&second).unwrap().is_none());
        private::replace(&path, b"not a certificate").unwrap();
        assert!(matrix_root(&first).is_err());
        private::replace(&path, &vec![b'x'; 16385]).unwrap();
        assert!(matrix_root(&first).is_err());
        #[cfg(unix)]
        {
            std::fs::remove_file(&path).unwrap();
            let target = second.join("matrix.ca.pem");
            private::write_new(&target, pem).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(matrix_root(&first).is_err());
            assert_eq!(matrix_root(&second).unwrap().unwrap(), pem);
        }
    }
    #[test]
    fn native_matrix_pacing_configuration() {
        assert!(
            matrix_limits("https://example.test/", None, None)
                .unwrap()
                .request_pacing
                .is_none()
        );
        for ms in [0, 9, 1001, u64::MAX] {
            assert!(matrix_limits("https://example.test/", Some(ms), None).is_err());
        }
        let limits = matrix_limits("https://example.test/", Some(250), None).unwrap();
        let clone = limits.clone();
        assert!(std::sync::Arc::ptr_eq(
            limits.request_pacing.as_ref().unwrap(),
            clone.request_pacing.as_ref().unwrap()
        ));
    }
    #[test]
    fn native_matrix_sdk_budget_configuration() {
        let original = matrix_limits("https://example.test/", None, None).unwrap();
        assert_eq!(original.sdk, std::time::Duration::from_secs(20));
        for ms in [0, 9, 60_001, u64::MAX] {
            assert!(matrix_limits("https://example.test/", None, Some(ms)).is_err());
        }
        for ms in [10, 20_000, 60_000] {
            let limits = matrix_limits("https://example.test/", Some(1000), Some(ms)).unwrap();
            assert_eq!(limits.clone().sdk, std::time::Duration::from_millis(ms));
            assert_eq!(limits.connect, original.connect);
            assert_eq!(limits.headers, original.headers);
            assert_eq!(limits.request, original.request);
            assert_eq!(limits.body_idle, original.body_idle);
        }
    }
    #[test]
    fn native_bootstrap_approval_wait_bound() {
        let limits = Limits {
            operation_ms: 30000,
            response_ms: 2000,
        };
        assert_eq!(default_approval_wait(), 1000);
        assert!(approval_host(default_approval_wait(), limits).is_ok());
        assert!(approval_host(10000, limits).is_ok());
        assert!(approval_host(25000, limits).is_ok());
        for wait in [0, 25001, u64::MAX] {
            assert!(approval_host(wait, limits).is_err());
        }
        assert!(
            approval_host(
                1000,
                Limits {
                    operation_ms: 2500,
                    response_ms: 1500
                }
            )
            .is_err()
        );
        let long = Limits {
            operation_ms: hagency_core::tasks::MAX_OWNED_OPERATION_MS,
            response_ms: 2000,
        };
        assert!(long.validate());
        assert_eq!(
            long.capability_ms().unwrap(),
            hagency_core::tasks::MAX_OWNED_CAPABILITY_MS
        );
        for wait in [1000, 60_000, 595_000] {
            assert!(approval_host(wait, long).is_ok());
        }
        for wait in [0, 595_001, u64::MAX] {
            assert!(approval_host(wait, long).is_err());
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FactoryService {
    profile: String,
    idle_ms: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalCodex {
    profile: String,
    preset: String,
    seat: String,
    home: PathBuf,
    codex_home: PathBuf,
}
/// ADR-192: the Claude Code runtime of an imported fleet, beside the Codex one.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaudeRuntimeConfig {
    executable: PathBuf,
    executable_sha256: String,
    #[serde(default)]
    local_claude: Option<LocalClaude>,
}
/// The user's own Claude Code sign-in folder (ADR-192 decision 7).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalClaude {
    profile: String,
    preset: String,
    seat: String,
    home: PathBuf,
    config_dir: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Matrix {
    #[serde(default)]
    token_provisioning: Option<TokenProvisioning>,
    #[serde(default)]
    crypto_enrollment: Option<CryptoEnrollment>,
    origin: String,
    server_name: String,
    registration_fingerprint: String,
    engagement_id: String,
    registration_generation: u64,
    transport_generation: u64,
    sender_mxid: String,
    device_id: String,
    rooms: Vec<Room>,
}
#[derive(Deserialize)]
#[serde(tag = "profile", deny_unknown_fields)]
enum TokenProvisioning {
    #[serde(rename = "registration_token_account_step_v1")]
    Account {},
    #[serde(rename = "registration_token_rooms_enrollment_step_v1")]
    Rooms { peer_masters: Vec<PeerMaster> },
    #[serde(rename = "registration_token_home_rooms_enrollment_step_v1")]
    HomeRooms {
        peer_masters: Vec<PeerMaster>,
        home: HomeConfiguration,
    },
    #[serde(rename = "appservice_login_home_rooms_enrollment_step_v1")]
    AppserviceHomeRooms {
        peer_masters: Vec<PeerMaster>,
        home: HomeConfiguration,
        namespace_prefix: String,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HomeConfiguration {
    root: PathBuf,
    task_client: PathBuf,
    projects: Vec<hagency_store::agent_home::HomeProject>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CryptoEnrollment {
    profile: String,
    peer_masters: Vec<PeerMaster>,
}
/// The approval bot's own credential set (PC-C0, plan v4 Q3): a SECOND
/// identity, token, device and SDK root, never the pooled ordinary
/// `HostConfig` (which `Collector::new` refuses for `approval == true`).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalMatrix {
    origin: String,
    server_name: String,
    registration_fingerprint: String,
    engagement_id: String,
    registration_generation: u64,
    transport_generation: u64,
    sender_mxid: String,
    device_id: String,
    rooms: Vec<Room>,
    peer_masters: Vec<PeerMaster>,
}
/// What `bootstrap::approval` builds the pump's collector from.
pub(super) struct Approval {
    pub config: HostConfig,
    pub engagement_id: String,
    pub anchors: Vec<(String, String)>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerMaster {
    user_id: String,
    master_key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Room {
    id: String,
    generation: u64,
    privacy: RoomPrivacy,
}
pub(super) struct Prepared {
    pub host: Host,
    pub managed_account: Option<String>,
    pub matrix: Option<HostConfig>,
    pub approval: Option<Approval>,
    pub provisioning: Option<hagency_matrix::TokenProvisioningHost>,
    pub warm: Option<hagency_execution::WarmHostPlan>,
    pub fleet: Option<super::fleet::Setup>,
    pub files: Option<crate::file_service::Setup>,
    pub receives: Option<crate::receive_service::Setup>,
    pub enrollment: bool,
    pub receive_inbox: Option<hagency_core::received_files::ReceiveInboxPlan>,
    pub intake_sessions: Vec<String>,
    pub agent_inboxes: Vec<hagency_core::agent_inbox::AgentInboxPlan>,
    pub claim: OwnedClaimProfile,
    pub limits: Limits,
    pub max_live: u32,
    #[cfg(test)]
    pub discard_claim_reply: bool,
}
pub(super) fn read(path: &Path, limit: usize, field: &'static str) -> Result<Vec<u8>, Failure> {
    // Every caller passes the literal file name it already used in the path,
    // so the refusal names the exact file an operator can act on (TS parity
    // names the file/setting, install-full.sh:292).
    let refuse = || Failure::Config {
        field,
        fix: "the file must be present, owner-private (0600), a regular non-symlink file within its byte limit",
    };
    let file = private::open(path, false).map_err(|_| refuse())?;
    let length =
        usize::try_from(file.metadata().map_err(|_| refuse())?.len()).map_err(|_| refuse())?;
    if length > limit {
        return Err(Failure::Config {
            field,
            fix: "the file is larger than its byte limit; trim it to the documented size",
        });
    }
    let mut bytes = Vec::with_capacity(length);
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| refuse())?;
    if bytes.len() > limit {
        return Err(Failure::Config {
            field,
            fix: "the file grew past its byte limit while being read; retry with a stable file",
        });
    }
    Ok(bytes)
}
/// An operator-installed root belongs to this engagement's Matrix connection.
/// An invalid or unreadable file must not silently fall back to public roots.
pub(super) fn matrix_root(state: &Path) -> Result<Option<Vec<u8>>, Failure> {
    let path = state.join("matrix.ca.pem");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => {
            let bytes = read(&path, 16 * 1024, "matrix.ca.pem")?;
            reqwest::Certificate::from_pem_bundle(&bytes)
                .ok()
                .filter(|roots| !roots.is_empty())
                .ok_or(Failure::Config {
                    field: "matrix.ca.pem",
                    fix: "the engagement's Matrix CA must be a usable PEM root certificate",
                })?;
            Ok(Some(bytes))
        }
    }
}
/// Fixed-memory digest under trusted executable/ancestor provisioning. This is
/// not handle-based executable launch or a claim of hostile namespace isolation.
fn verify_executable(path: &Path, expected: &str) -> Result<(), Failure> {
    if !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().len() > 4096
        || path.canonicalize().ok().as_deref() != Some(path)
        || expected.len() != 64
        || !expected
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Failure::Config {
            field: "agent-driver.json: executable_sha256",
            fix: "the digest must be 64 lowercase hex characters naming an absolute, canonicalized executable path",
        });
    }
    let before = std::fs::symlink_metadata(path).map_err(|_| Failure::Config {
        field: "agent-driver.json: executable",
        fix: "the executable path must be statable by the service owner",
    })?;
    if !before.is_file()
        || before.file_type().is_symlink()
        || before.len() == 0
        || before.len() > EXECUTABLE_BYTES
    {
        return Err(Failure::Config {
            field: "agent-driver.json: executable",
            fix: "the executable must be a non-empty regular non-symlink file within the byte ceiling",
        });
    }
    let mut file = File::open(path).map_err(|_| Failure::Config {
        field: "agent-driver.json: executable",
        fix: "the executable must be readable by the service owner",
    })?;
    let actual = file.metadata().map_err(|_| Failure::Config {
        field: "agent-driver.json: executable",
        fix: "the executable metadata must be readable after open",
    })?;
    if !actual.is_file() || actual.len() != before.len() {
        return Err(Failure::Config {
            field: "agent-driver.json: executable",
            fix: "the executable changed between stat and open; restart with a stable binary",
        });
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 32 * 1024];
    let mut total = 0u64;
    tracing::trace!(target: "hagency_startup_observation", "native startup boundary: executable_hash_entered");
    loop {
        let count = file.read(&mut buffer).map_err(|_| Failure::Config {
            field: "agent-driver.json: executable",
            fix: "hashing the executable failed mid-read; restart with a stable binary",
        })?;
        if count == 0 {
            break;
        }
        total = total.checked_add(count as u64).ok_or(Failure::Config {
            field: "agent-driver.json: executable",
            fix: "the executable byte count overflowed while hashing",
        })?;
        if total > EXECUTABLE_BYTES || total > actual.len() {
            return Err(Failure::Config {
                field: "agent-driver.json: executable",
                fix: "the executable grew past the byte ceiling while hashing",
            });
        }
        hasher.update(&buffer[..count]);
    }
    if total != actual.len()
        || file
            .metadata()
            .map_err(|_| Failure::Config {
                field: "agent-driver.json: executable",
                fix: "the executable metadata must be re-readable after hashing",
            })?
            .len()
            != total
    {
        return Err(Failure::Config {
            field: "agent-driver.json: executable",
            fix: "the executable changed size while hashing; restart with a stable binary",
        });
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect();
    tracing::trace!(target: "hagency_startup_observation", "native startup boundary: executable_hash_completed");
    if digest != expected {
        return Err(Failure::Config {
            field: "agent-driver.json: executable_sha256",
            fix: "the running binary's digest does not match; reinstall the binary or update the digest",
        });
    }
    Ok(())
}
impl Prepared {
    pub(super) fn attach_factory(
        &mut self,
        approvals: Option<std::sync::Arc<hagency_matrix::ApprovalCollector>>,
    ) -> Result<(), Failure> {
        if let Some(mut provisioning) = self.provisioning.take() {
            if let Some(warm) = self.warm.take() {
                provisioning = provisioning
                    .with_warm_runtime(
                        warm,
                        approvals.ok_or(Failure::Config {
                            field: "agent-driver.json: factory_service",
                            fix: "the warm runtime needs its approval collector built before attach",
                        })?,
                    )
                    .map_err(|_| Failure::Config {
                        field: "agent-driver.json: factory_service",
                        fix: "the warm runtime must accept the provisioning host",
                    })?;
            }
            self.matrix = Some(
                self.matrix
                    .take()
                    .ok_or(Failure::Config {
                        field: "agent-driver.json: matrix block",
                        fix: "token provisioning requires a configured matrix host",
                    })?
                    .with_token_account_provisioning(provisioning)
                    .map_err(|_| Failure::Config {
                        field: "agent-driver.json: matrix.token_provisioning",
                        fix: "the matrix host must accept the token provisioning service",
                    })?,
            );
        } else if self.warm.is_some() || self.fleet.is_some() {
            return Err(Failure::Config {
                field: "agent-driver.json: factory_service",
                fix: "a warm runtime or fleet without token provisioning is not runnable; configure matrix.token_provisioning",
            });
        }
        Ok(())
    }
    pub(super) fn load(
        state: &Path,
        address: SocketAddr,
        mode: DriverMode,
    ) -> Result<Self, Failure> {
        let (file, profile) = match mode {
            DriverMode::OneAttempt => {
                ("development-driver.json", "codex_app_server_development_v1")
            }
            DriverMode::Continuous => ("agent-driver.json", "codex_app_server_agent_v1"),
            DriverMode::Disabled => {
                return Err(Failure::Config {
                    field: "serve --agent-driver/--development-driver",
                    fix: "a driver configuration requires one of the driver flags; run serve with --agent-driver or --development-driver",
                });
            }
        };
        let bytes = read(&state.join(file), CONFIG_BYTES, file)?;
        let mut config: Config = serde_json::from_slice(&bytes).map_err(|error| {
            // This document contains paths and public Matrix identities only;
            // credentials remain in separate private files. Retain serde's
            // location so an operator can repair malformed deployment input
            // without weakening the fail-closed error at this boundary.
            tracing::error!(
                line = error.line(),
                column = error.column(),
                "invalid agent driver configuration"
            );
            Failure::Config {
                field: "agent-driver.json / development-driver.json",
                fix: "repair the JSON at the logged line and column; the document must match the driver profile",
            }
        })?;
        let enrollment = config.matrix.crypto_enrollment.is_some();
        if let Some(local) = &config.local_codex
            && (local.profile != "provider_owned_codex_v1" || config.managed_account.is_some())
        {
            return Err(Failure::Config {
                field: "agent-driver.json: local_codex",
                fix: "local_codex requires profile provider_owned_codex_v1 and excludes managed_account",
            });
        }
        if let Some(factory) = &config.factory_service
            && (mode != DriverMode::Continuous
                || factory.profile != "inline_factory_service_checkpoint_v1"
                || !(100..=1_200_000).contains(&factory.idle_ms)
                || config.approval.is_none()
                || !matches!(
                    &config.matrix.token_provisioning,
                    Some(
                        TokenProvisioning::HomeRooms { .. }
                            | TokenProvisioning::AppserviceHomeRooms { .. }
                    )
                ))
        {
            return Err(Failure::Config {
                field: "agent-driver.json: factory_service",
                fix: "the inline factory requires --agent-driver, profile inline_factory_service_checkpoint_v1, idle_ms 100-1200000, an approval block and HomeRooms/AppserviceHomeRooms provisioning",
            });
        }
        if config.profile != profile
            || config.workspaces.is_empty()
            || config.workspaces.len() > 16
            || config.matrix.rooms.is_empty()
            || config.matrix.rooms.len() > 16
            || !config.matrix.origin.starts_with("https://")
        {
            return Err(Failure::Config {
                field: "agent-driver.json: profile, workspaces, matrix.rooms, matrix.origin",
                fix: "profile must match the driver mode; 1-16 workspaces and rooms; origin must start with https://",
            });
        }
        if mode == DriverMode::Continuous && config.receive_inbox.is_some() {
            // The fixed receive-inbox plan is deliberately one-dispatch-only;
            // it cannot be replayed as a scheduler input.
            return Err(Failure::Config {
                field: "agent-driver.json: receive_inbox",
                fix: "receive_inbox is a one-attempt development input; remove it for --agent-driver",
            });
        }
        if mode == DriverMode::OneAttempt && !config.agent_inboxes.is_empty() {
            return Err(Failure::Config {
                field: "agent-driver.json: agent_inboxes",
                fix: "agent_inboxes is a continuous-driver input; remove it for --development-driver",
            });
        }
        if config.agent_inboxes.len() > 16 {
            return Err(Failure::Config {
                field: "agent-driver.json: agent_inboxes",
                fix: "at most 16 agent inbox plans are supported",
            });
        }
        let mut inbox_sessions = std::collections::BTreeSet::new();
        for plan in &config.agent_inboxes {
            plan.validate().map_err(|_| Failure::Config {
                field: "agent-driver.json: agent_inboxes",
                fix: "each plan needs a valid session id, workspace id and dispatch constraint",
            })?;
            if !config.workspaces.contains_key(&plan.workspace_id)
                || !inbox_sessions.insert(&plan.session_id)
            {
                return Err(Failure::Config {
                    field: "agent-driver.json: agent_inboxes",
                    fix: "each plan's workspace must be declared and session ids must be unique",
                });
            }
        }
        let mut combined_sessions = config.intake_sessions.clone();
        for plan in &config.agent_inboxes {
            if !combined_sessions.contains(&plan.session_id) {
                combined_sessions.push(plan.session_id.clone());
            }
        }
        if !combined_sessions.is_empty() {
            hagency_matrix::HostIntakePlan::new(combined_sessions.clone()).map_err(|_| {
                Failure::Config {
                    field: "agent-driver.json: intake_sessions/agent_inboxes",
                    fix: "every combined session id must be 1-128 chars of [A-Za-z0-9_-]",
                }
            })?;
        }
        config.intake_sessions = combined_sessions;
        if config.factory_service.is_some() && config.intake_sessions.is_empty() {
            // Reception provisioning is read by the coordinator's actual
            // intake. A fleet with no verified intake session cannot run it.
            return Err(Failure::Config {
                field: "agent-driver.json: intake_sessions",
                fix: "the factory service needs at least one verified intake session",
            });
        }
        tracing::trace!(target: "hagency_startup_observation", "native startup boundary: executable_verify_entered");
        verify_executable(&config.executable, &config.executable_sha256)?;
        tracing::trace!(target: "hagency_startup_observation", "native startup boundary: executable_verify_completed");
        let own = std::env::current_exe()
            .map_err(|_| Failure::Config {
                field: "running executable path",
                fix: "the process executable path must be resolvable",
            })?
            .canonicalize()
            .map_err(|_| Failure::Config {
                field: "running executable path",
                fix: "the process executable path must canonicalize (no broken symlink chain)",
            })?;
        let mut environment = BTreeMap::new();
        if config.local_codex.is_none() {
            let runtime_home = state.join("runtime-home");
            private::directory(&runtime_home).map_err(|_| Failure::Config {
                field: "state-dir runtime-home",
                fix: "the runtime home must exist, be owner-private (0700) and writable",
            })?;
            environment.insert("HOME".into(), runtime_home.clone().into_os_string());
            environment.insert("CODEX_HOME".into(), runtime_home.into_os_string());
        }
        if let Some(system) = std::env::var_os("SystemRoot") {
            environment.insert("SystemRoot".into(), system);
        }
        let transport = MatrixTransportObservation {
            engagement_id: config.matrix.engagement_id,
            registration_generation: config.matrix.registration_generation,
            generation: config.matrix.transport_generation,
            sender_mxid: config.matrix.sender_mxid,
            device_id: config.matrix.device_id,
        };
        let mut claim_rooms = Vec::new();
        let mut rooms = Vec::new();
        for room in config.matrix.rooms {
            claim_rooms.push(
                OwnedClaimRoom::new(room.id.clone(), room.generation, room.privacy.clone())
                    .map_err(|_| Failure::Config {
                        field: "agent-driver.json: matrix.rooms",
                        fix: "each room needs a valid id, generation and privacy",
                    })?,
            );
            rooms.push(HostRoom {
                room_id: room.id,
                generation: room.generation,
                privacy: room.privacy,
            });
        }
        let mut claim = OwnedClaimProfile::new(
            transport.clone(),
            claim_rooms,
            config.workspaces.keys().cloned().collect(),
        )
        .map_err(|_| Failure::Config {
            field: "agent-driver.json: matrix block",
            fix: "the owned claim profile must construct from the transport observation, rooms and workspaces",
        })?;
        if let Some(plan) = &config.receive_inbox {
            plan.validate().map_err(|_| Failure::Config {
                field: "agent-driver.json: receive_inbox",
                fix: "the plan needs a valid session id, workspace id and dispatch constraint",
            })?;
            if !config.workspaces.contains_key(&plan.workspace_id) {
                return Err(Failure::Config {
                    field: "agent-driver.json: receive_inbox.workspace_id",
                    fix: "the plan's workspace must be declared in workspaces",
                });
            }
            claim = claim
                .restrict_dispatch(plan.dispatch_id.clone())
                .map_err(|_| Failure::Config {
                    field: "agent-driver.json: receive_inbox.dispatch_id",
                    fix: "the dispatch constraint must apply to the claim profile",
                })?;
        }
        let mut host = Host::new(
            own.clone(),
            config.executable.clone(),
            environment.clone(),
            config.workspaces,
        )
        .and_then(|h| h.with_file_limit(config.file_limit))
        .and_then(|h| h.with_task_helper(own.clone(), address))
        .map_err(|_| Failure::Config {
            field: "agent-driver.json: executable/workspaces/file_limit",
            fix: "the execution host must construct from the executable, workspaces and file limit",
        })?;
        let uses_local_codex = config.local_codex.is_some();
        if let Some(local) = config.local_codex {
            claim = claim
                .restrict_resource(local.preset.clone(), local.seat.clone())
                .map_err(|_| Failure::Config {
                    field: "agent-driver.json: local_codex",
                    fix: "the preset/seat restriction must apply to the claim profile",
                })?;
            let local = hagency_execution::LocalCodex::new(
                local.preset,
                local.seat,
                local.home,
                local.codex_home,
            )
            .map_err(|_| Failure::Config {
                field: "agent-driver.json: local_codex",
                fix: "preset, seat, home and codex_home must form a valid local codex binding",
            })?;
            host = host.with_local_codex(local).map_err(|_| Failure::Config {
                field: "agent-driver.json: local_codex",
                fix: "the execution host must accept the local codex binding",
            })?;
        }
        if config.send_file {
            host = host.with_file_tools().map_err(|_| Failure::Config {
                field: "agent-driver.json: send_file",
                fix: "the file tools must install on the execution host",
            })?;
        }
        if config.receive_file {
            hagency_core::received_files::receive_limit(config.file_limit).map_err(|_| {
                Failure::Config {
                    field: "agent-driver.json: file_limit",
                    fix: "the receive limit must be valid for the received-files contract",
                }
            })?;
            host = host.with_receive_tools().map_err(|_| Failure::Config {
                field: "agent-driver.json: receive_file",
                fix: "the receive tools must install on the execution host",
            })?;
        }
        if config.coordination_tools {
            host = host
                .with_coordination_tools()
                .map_err(|_| Failure::Config {
                    field: "agent-driver.json: coordination_tools",
                    fix: "the coordination tools must install on the execution host",
                })?;
        }
        let namespace = hagency_core::canonical::digest(&serde_json::json!([
            "native_file_storage_v1",
            config.matrix.origin,
            config.matrix.server_name,
            config.matrix.registration_fingerprint,
            transport.engagement_id,
            transport.registration_generation,
            transport.sender_mxid,
            transport.device_id
        ]))
        .map_err(|_| Failure::Config {
            field: "agent-driver.json: file storage namespace",
            fix: "the canonical digest must compute; keep matrix fields ASCII",
        })?;
        let files = config.send_file.then(|| crate::file_service::Setup {
            directory: state.join("file-media"),
            namespace,
            limit: config.file_limit,
        });
        let token = read(
            &state.join("matrix.access_token"),
            4096,
            "matrix.access_token",
        )?;
        let token = std::str::from_utf8(&token).map_err(|_| Failure::Config {
            field: "matrix.access_token",
            fix: "the token must be valid UTF-8",
        })?;
        let key: [u8; 32] = read(&state.join("matrix.sdk_key"), 32, "matrix.sdk_key")?
            .try_into()
            .map_err(|_| Failure::Config {
                field: "matrix.sdk_key",
                fix: "the SDK key must be exactly 32 bytes",
            })?;
        let matrix_limits = matrix_limits(
            &config.matrix.origin,
            config.matrix_request_interval_ms,
            config.matrix_sdk_timeout_ms,
        )?;
        let mut matrix = HostConfig::new(
            HostIdentity {
                server_name: config.matrix.server_name,
                registration_fingerprint: config.matrix.registration_fingerprint,
                transport,
            },
            &config.matrix.origin,
            token,
            state.join("sdk"),
            key,
            rooms,
            matrix_limits.clone(),
        )
        .map_err(|_| Failure::Config {
            field: "agent-driver.json: matrix block",
            fix: "the matrix host must construct from the identity, origin, token, key, rooms and limits",
        })?;
        // Fail-closed: the pre-project reception room comes from the store's
        // recorded registration for this host's engagement. A host whose
        // engagement names no registration (or a registration with no
        // reception room) refuses to start rather than observing nothing and
        // silently dropping the provisioning ingress.
        let mut provisioning = None;
        {
            let engagement_id = matrix.engagement_id().to_owned();
            let registration = DomainRepository::open(state)
                .map_err(|_| Failure::Config {
                    field: "state-dir domain store",
                    fix: "the domain repository must open (check ownership and WAL files)",
                })?
                .provisioning_registration_for_engagement(&engagement_id)
                .map_err(|_| Failure::Config {
                    field: "registrations row",
                    fix: "register the fleet first: hagency registration register --file <six-field JSON> (or POST /api/native/v1/project-sides)",
                })?;
            matrix
                .with_reception_room(HostRoom {
                    room_id: registration.reception_room_id.clone(),
                    generation: registration.generation,
                    privacy: RoomPrivacy::Group {},
                })
                .map_err(|_| Failure::Config {
                    field: "registrations row: reception_room_id",
                    fix: "the recorded reception room must bind to the matrix host",
                })?;
            if let Some(profile) = config.matrix.token_provisioning {
                let (peer_masters, home, as_namespace) = match profile {
                    TokenProvisioning::Account {} => (None, None, None),
                    TokenProvisioning::Rooms { peer_masters } => (Some(peer_masters), None, None),
                    TokenProvisioning::HomeRooms { peer_masters, home } => {
                        (Some(peer_masters), Some(home), None)
                    }
                    TokenProvisioning::AppserviceHomeRooms {
                        peer_masters,
                        home,
                        namespace_prefix,
                    } => (Some(peer_masters), Some(home), Some(namespace_prefix)),
                };
                let token = if as_namespace.is_some() {
                    read(
                        &state.join("matrix.appservice_token"),
                        4096,
                        "matrix.appservice_token",
                    )?
                } else {
                    read(
                        &state.join("matrix.registration_token"),
                        64,
                        "matrix.registration_token",
                    )?
                };
                let token = std::str::from_utf8(&token).map_err(|_| Failure::Config {
                    field: "matrix.appservice_token / matrix.registration_token",
                    fix: "the provisioning token must be valid UTF-8",
                })?;
                let key: [u8; 32] = read(
                    &state.join("matrix.provisioning_key"),
                    32,
                    "matrix.provisioning_key",
                )?
                .try_into()
                .map_err(|_| Failure::Config {
                    field: "matrix.provisioning_key",
                    fix: "the provisioning key must be exactly 32 bytes",
                })?;
                let mut host = if let Some(namespace) = as_namespace {
                    hagency_matrix::TokenProvisioningHost::application_service(
                        registration.clone(),
                        &config.matrix.origin,
                        hagency_matrix::ApplicationServiceCredential::new(token, &namespace).map_err(
                            |_| Failure::Config {
                                field: "agent-driver.json: namespace_prefix",
                                fix: "the namespace must be lowercase letters, digits or underscore, max 128 chars",
                            },
                        )?,
                        state.to_owned(),
                        key,
                        matrix_limits.clone(),
                    )
                } else {
                    hagency_matrix::TokenProvisioningHost::new(
                        registration.clone(),
                        &config.matrix.origin,
                        token,
                        state.to_owned(),
                        key,
                        matrix_limits.clone(),
                    )
                }
                .map_err(|_| Failure::Config {
                    field: "agent-driver.json: matrix.token_provisioning",
                    fix: "the provisioning host must construct from the registration, origin, token and key",
                })?;
                if let Some(peer_masters) = peer_masters {
                    let token = read(
                        &state.join("matrix.representative_token"),
                        4096,
                        "matrix.representative_token",
                    )?;
                    let token = std::str::from_utf8(&token).map_err(|_| Failure::Config {
                        field: "matrix.representative_token",
                        fix: "the token must be valid UTF-8",
                    })?;
                    host = host
                        .with_agent_rooms_enrollment(
                            token,
                            peer_masters
                                .into_iter()
                                .map(|p| (p.user_id, p.master_key))
                                .collect(),
                        )
                        .map_err(|_| Failure::Config {
                            field: "agent-driver.json: token_provisioning peer_masters",
                            fix: "the representative token must enroll the declared peer master keys",
                        })?;
                }
                if let Some(home) = home {
                    let plan = hagency_store::agent_home::ManagedHomePlan::new(
                        home.root,
                        home.projects,
                        home.task_client,
                    )
                    .map_err(|_| Failure::Config {
                        field: "agent-driver.json: token_provisioning home",
                        fix: "root, projects and task_client must form a valid managed home plan",
                    })?;
                    host = host.with_managed_homes(plan).map_err(|_| Failure::Config {
                        field: "agent-driver.json: token_provisioning home",
                        fix: "the provisioning host must accept the managed home plan",
                    })?;
                }
                let ca = state.join("matrix.ca.pem");
                if ca.try_exists().map_err(|_| Failure::Config {
                    field: "matrix.ca.pem",
                    fix: "the CA file must be statable (permission or path error)",
                })? {
                    host = host
                        .with_root_pem(&read(&ca, 16 * 1024, "matrix.ca.pem")?)
                        .map_err(|_| Failure::Config {
                            field: "matrix.ca.pem",
                            fix: "the CA bundle must be a usable PEM root (max 16 KiB)",
                        })?;
                }
                provisioning = Some(host);
            }
        }
        if let Some(profile) = config.matrix.crypto_enrollment {
            if profile.profile != "fresh_own_account_v1" {
                return Err(Failure::Config {
                    field: "agent-driver.json: matrix.crypto_enrollment.profile",
                    fix: "the only supported enrollment profile is fresh_own_account_v1",
                });
            }
            matrix = matrix
                .with_fresh_account_enrollment(
                    profile
                        .peer_masters
                        .into_iter()
                        .map(|p| (p.user_id, p.master_key))
                        .collect(),
                )
                .map_err(|_| Failure::Config {
                    field: "agent-driver.json: matrix.crypto_enrollment",
                    fix: "the matrix host must accept the fresh-account enrollment",
                })?;
        }
        let ca = state.join("matrix.ca.pem");
        if ca.try_exists().map_err(|_| Failure::Config {
            field: "matrix.ca.pem",
            fix: "the CA file must be statable (permission or path error)",
        })? {
            matrix = matrix
                .with_root_pem(&read(&ca, 16 * 1024, "matrix.ca.pem")?)
                .map_err(|_| Failure::Config {
                    field: "matrix.ca.pem",
                    fix: "the CA bundle must be a usable PEM root (max 16 KiB)",
                })?;
        }
        let limits = Limits {
            operation_ms: config.operation_ms,
            response_ms: config.response_ms,
        };
        if !limits.validate() {
            return Err(Failure::Config {
                field: "agent-driver.json: operation_ms / response_ms",
                fix: "the operation budget must be positive and dominate the response budget",
            });
        }
        // The approval bot's own credential and the host's approval capacity
        // (PC-C0, plan v4 Q3): a SECOND identity set, never the pooled
        // ordinary `HostConfig`, and the `ApprovalHost` without whose
        // attachment `Operation::start_mode` never creates the notices
        // channel at all. `ApprovalHost::new` values must `fits(limits)` or
        // every start refuses with `Failure::Admission`.
        let mut runtime_approvals = None;
        let owner_wait_ms = config.approval_owner_wait_ms;
        let approval = match config.approval {
            Some(approval) => {
                if approval.rooms.is_empty()
                    || approval.rooms.len() > 16
                    || !approval.origin.starts_with("https://")
                    || !approval
                        .rooms
                        .iter()
                        .all(|r| matches!(r.privacy, RoomPrivacy::Direct { .. }))
                {
                    return Err(Failure::Config {
                        field: "agent-driver.json: approval.rooms",
                        fix: "1-16 direct-privacy rooms and an https:// origin are required",
                    });
                }
                let transport = MatrixTransportObservation {
                    engagement_id: approval.engagement_id.clone(),
                    registration_generation: approval.registration_generation,
                    generation: approval.transport_generation,
                    sender_mxid: approval.sender_mxid.clone(),
                    device_id: approval.device_id.clone(),
                };
                let rooms = approval
                    .rooms
                    .into_iter()
                    .map(|room| HostRoom {
                        room_id: room.id,
                        generation: room.generation,
                        privacy: room.privacy,
                    })
                    .collect();
                let token = read(
                    &state.join("approval.access_token"),
                    4096,
                    "approval.access_token",
                )?;
                let token = std::str::from_utf8(&token).map_err(|_| Failure::Config {
                    field: "approval.access_token",
                    fix: "the approval bot token must be valid UTF-8",
                })?;
                let key: [u8; 32] = read(&state.join("approval.sdk_key"), 32, "approval.sdk_key")?
                    .try_into()
                    .map_err(|_| Failure::Config {
                        field: "approval.sdk_key",
                        fix: "the approval SDK key must be exactly 32 bytes",
                    })?;
                let mut config = HostConfig::new(
                    HostIdentity {
                        server_name: approval.server_name,
                        registration_fingerprint: approval.registration_fingerprint,
                        transport,
                    },
                    &approval.origin,
                    token,
                    state.join("approval-sdk"),
                    key,
                    rooms,
                    matrix_limits.clone(),
                )
                .map_err(|_| Failure::Config {
                    field: "agent-driver.json: approval block",
                    fix: "the approval host must construct from the identity, origin, token, key, rooms and limits",
                })?;
                let ca = state.join("approval.ca.pem");
                if ca.try_exists().map_err(|_| Failure::Config {
                    field: "approval.ca.pem",
                    fix: "the CA file must be statable (permission or path error)",
                })? {
                    config = config
                        .with_root_pem(&read(&ca, 16 * 1024, "matrix.ca.pem")?)
                        .map_err(|_| Failure::Config {
                            field: "approval.ca.pem",
                            fix: "the CA bundle must be a usable PEM root (max 16 KiB)",
                        })?;
                }
                // The capacity must fit the operation limits exactly as
                // `ApprovalHost::fits` checks them, or `start_mode` refuses.
                let host_approvals = approval_host(owner_wait_ms, limits)?;
                host =
                    host.with_approvals(host_approvals.clone())
                        .map_err(|_| Failure::Config {
                            field: "agent-driver.json: approval block",
                            fix: "the execution host must accept the approval host attachment",
                        })?;
                runtime_approvals = Some(host_approvals);
                Some(Approval {
                    config,
                    engagement_id: approval.engagement_id,
                    anchors: approval
                        .peer_masters
                        .into_iter()
                        .map(|p| (p.user_id, p.master_key))
                        .collect(),
                })
            }
            None => None,
        };
        let (warm, fleet) = if let Some(factory) = config.factory_service {
            let contexts = state.join("factory-task-contexts");
            private::directory(&contexts).map_err(|_| Failure::Config {
                field: "state-dir factory-task-contexts",
                fix: "the directory must exist, be owner-private (0700) and writable",
            })?;
            let bridge = hagency_execution::WarmTaskBridge::new(own.clone(), address, contexts)
                .map_err(|_| Failure::Config {
                    field: "state-dir factory-task-contexts",
                    fix: "the warm task bridge must construct from the executable, listen address and contexts directory",
                })?;
            let initialize = Limits {
                operation_ms: limits.operation_ms.min(30_000),
                response_ms: limits.response_ms,
            };
            let mut warm = hagency_execution::WarmHostPlan::new(
                own,
                config.executable,
                environment,
                bridge,
                runtime_approvals.ok_or(Failure::Config {
                    field: "agent-driver.json: approval block",
                    fix: "the inline factory requires the approval host built before the warm bridge",
                })?,
                hagency_execution::WarmLimits {
                    initialize,
                    idle_ms: factory.idle_ms,
                },
            )
            .and_then(|plan| {
                plan.with_file_access(config.file_limit, config.send_file, config.receive_file)
            })
            .map(|plan| {
                if config.coordination_tools {
                    plan.with_coordination_tools()
                } else {
                    plan
                }
            })
            .map_err(|_| Failure::Config {
                field: "agent-driver.json: factory_service plan",
                fix: "the warm plan must apply the file, coordination and receive capabilities",
            })?;
            if uses_local_codex {
                warm = warm
                    .with_local_codex_from_host(&host)
                    .map_err(|_| Failure::Config {
                        field: "agent-driver.json: local_codex",
                        fix: "the warm bridge must accept the host's local codex binding",
                    })?;
            }
            (
                Some(warm),
                Some(super::fleet::Setup {
                    state: state.to_owned(),
                    limit: config.file_limit,
                    send: config.send_file,
                    receive: config.receive_file,
                    limits,
                }),
            )
        } else {
            (None, None)
        };
        let max_live = if warm.is_some() { 8 } else { 1 };
        Ok(Self {
            host,
            managed_account: config.managed_account,
            matrix: Some(matrix),
            approval,
            provisioning,
            warm,
            fleet,
            files,
            receives: config
                .receive_file
                .then_some(crate::receive_service::Setup {
                    limit: config.file_limit,
                }),
            enrollment,
            receive_inbox: config.receive_inbox,
            intake_sessions: config.intake_sessions,
            agent_inboxes: config.agent_inboxes,
            claim,
            limits,
            max_live,
            #[cfg(test)]
            discard_claim_reply: false,
        })
    }
}

fn workspace_map<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, PathBuf>, D::Error> {
    struct Map;
    impl<'de> serde::de::Visitor<'de> for Map {
        type Value = BTreeMap<String, PathBuf>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("at most sixteen unique workspace IDs")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut result = BTreeMap::new();
            while let Some((id, path)) = map.next_entry::<String, PathBuf>()? {
                if result.len() >= 16 || result.insert(id, path).is_some() {
                    return Err(serde::de::Error::custom(
                        "duplicate workspace or capacity exceeded",
                    ));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Map)
}

/// ADR-187 §A.1: an imported fleet's local runtime settings — the operator's
/// Codex executable, local Codex binding, file capabilities, limits and agent
/// homes. Everything about Matrix identities comes from the imported fleet,
/// not from here. Lives in `<state>/fleet-runtime.json`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FleetRuntimeConfig {
    profile: String,
    /// The Codex executable and its digest, together or not at all: a machine
    /// may run Claude Code only (ADR-192).
    #[serde(default)]
    executable: Option<PathBuf>,
    #[serde(default)]
    executable_sha256: Option<String>,
    #[serde(default)]
    local_codex: Option<LocalCodex>,
    #[serde(default)]
    claude: Option<ClaudeRuntimeConfig>,
    #[serde(default)]
    send_file: bool,
    #[serde(default)]
    receive_file: bool,
    #[serde(default)]
    coordination_tools: bool,
    file_limit: usize,
    operation_ms: u64,
    response_ms: u64,
    #[serde(default = "default_approval_wait")]
    approval_owner_wait_ms: u64,
    #[serde(default)]
    matrix_request_interval_ms: Option<u64>,
    #[serde(default)]
    matrix_sdk_timeout_ms: Option<u64>,
    idle_ms: u64,
    home: HomeConfiguration,
}
/// What the fleet service builds its provisioning host and agents from.
pub(super) struct FleetRuntime {
    pub(super) warm: hagency_execution::WarmHostPlan,
    pub(super) homes: hagency_store::agent_home::ManagedHomePlan,
    pub(super) setup: super::fleet::Setup,
    pub(super) matrix_limits: hagency_matrix::Limits,
}
/// Whether this state directory configures an imported fleet's runtime.
pub(super) fn fleet_runtime_configured(state: &Path) -> bool {
    state.join("fleet-runtime.json").exists()
}
pub(super) fn load_fleet_runtime(
    state: &Path,
    address: SocketAddr,
    origin: &str,
) -> Result<FleetRuntime, Failure> {
    const FIELD: &str = "fleet-runtime.json";
    let bytes = read(&state.join(FIELD), CONFIG_BYTES, FIELD)?;
    let config: FleetRuntimeConfig = serde_json::from_slice(&bytes).map_err(|error| {
        tracing::error!(line = error.line(), column = error.column(), "invalid fleet runtime configuration");
        Failure::Config {
            field: FIELD,
            fix: "repair the JSON at the logged line and column; the document must match profile palpo_fleet_runtime_v1",
        }
    })?;
    if config.profile != "palpo_fleet_runtime_v1"
        || !(100..=1_200_000).contains(&config.idle_ms)
        || config
            .local_codex
            .as_ref()
            .is_some_and(|local| local.profile != "provider_owned_codex_v1")
        || config.claude.as_ref().is_some_and(|claude| {
            claude
                .local_claude
                .as_ref()
                .is_some_and(|local| local.profile != "provider_owned_claude_v1")
        })
    {
        return Err(Failure::Config {
            field: FIELD,
            fix: "profile palpo_fleet_runtime_v1, idle_ms 100-1200000, local_codex (if any) with profile provider_owned_codex_v1, and claude.local_claude (if any) with profile provider_owned_claude_v1",
        });
    }
    let codex = match (config.executable, config.executable_sha256) {
        (Some(executable), Some(digest)) => {
            verify_executable(&executable, &digest)?;
            Some(executable)
        }
        (None, None) if config.local_codex.is_none() && config.claude.is_some() => None,
        _ => {
            return Err(Failure::Config {
                field: FIELD,
                fix: "name the Codex executable with its executable_sha256, or a claude runtime instead (a local_codex block needs the Codex executable)",
            });
        }
    };
    if let Some(claude) = &config.claude {
        verify_executable(&claude.executable, &claude.executable_sha256)?;
    }
    let own = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|_| Failure::Config {
            field: "running executable path",
            fix: "the process executable path must be resolvable",
        })?;
    let mut base = BTreeMap::new();
    if let Some(system) = std::env::var_os("SystemRoot") {
        base.insert("SystemRoot".into(), system);
    }
    let runtime_home = || {
        let runtime_home = state.join("runtime-home");
        private::directory(&runtime_home).map_err(|_| Failure::Config {
            field: "state-dir runtime-home",
            fix: "the runtime home must exist, be owner-private (0700) and writable",
        })?;
        Ok::<_, Failure>(runtime_home)
    };
    let mut environment = base.clone();
    if codex.is_some() && config.local_codex.is_none() {
        let runtime_home = runtime_home()?;
        environment.insert("HOME".into(), runtime_home.clone().into_os_string());
        environment.insert("CODEX_HOME".into(), runtime_home.into_os_string());
    }
    let limits = Limits {
        operation_ms: config.operation_ms,
        response_ms: config.response_ms,
    };
    // The warm bridge's local Codex binding is taken from a host built for
    // it; the fleet's agents get their own homes, so its one workspace is a
    // private placeholder no dispatch is ever routed to.
    let workspace = state.join("fleet-workspace");
    private::directory(&workspace).map_err(|_| Failure::Config {
        field: "state-dir fleet-workspace",
        fix: "the directory must exist, be owner-private (0700) and writable",
    })?;
    let codex_host = match &codex {
        Some(executable) => {
            let mut host = Host::new(
                own.clone(),
                executable.clone(),
                environment.clone(),
                BTreeMap::from([("fleet_workspace".to_owned(), workspace)]),
            )
            .map_err(|_| Failure::Config {
                field: FIELD,
                fix: "the executable and workspace must form an execution host",
            })?;
            if let Some(local) = config.local_codex {
                let local = hagency_execution::LocalCodex::new(
                    local.preset,
                    local.seat,
                    local.home,
                    local.codex_home,
                )
                .map_err(|_| Failure::Config {
                    field: "fleet-runtime.json: local_codex",
                    fix: "preset, seat, home and codex_home must form a valid local codex binding",
                })?;
                host = host.with_local_codex(local).map_err(|_| Failure::Config {
                    field: "fleet-runtime.json: local_codex",
                    fix: "the execution host must accept the local codex binding",
                })?;
                Some(host)
            } else {
                None
            }
        }
        None => None,
    };
    let contexts = state.join("factory-task-contexts");
    private::directory(&contexts).map_err(|_| Failure::Config {
        field: "state-dir factory-task-contexts",
        fix: "the directory must exist, be owner-private (0700) and writable",
    })?;
    let bridge = hagency_execution::WarmTaskBridge::new(own.clone(), address, contexts).map_err(|_| {
        Failure::Config {
            field: "state-dir factory-task-contexts",
            fix: "the warm task bridge must construct from the executable, listen address and contexts directory",
        }
    })?;
    let warm_limits = hagency_execution::WarmLimits {
        initialize: Limits {
            operation_ms: limits.operation_ms.min(30_000),
            response_ms: limits.response_ms,
        },
        idle_ms: config.idle_ms,
    };
    let approvals = approval_host(config.approval_owner_wait_ms, limits)?;
    let plan = match codex {
        Some(executable) => hagency_execution::WarmHostPlan::new(
            own,
            executable,
            environment,
            bridge,
            approvals,
            warm_limits,
        ),
        None => hagency_execution::WarmHostPlan::without_codex(
            own,
            environment,
            bridge,
            approvals,
            warm_limits,
        ),
    };
    let mut warm = plan
        .and_then(|plan| {
            plan.with_file_access(config.file_limit, config.send_file, config.receive_file)
        })
        .map(|plan| {
            if config.coordination_tools {
                plan.with_coordination_tools()
            } else {
                plan
            }
        })
        .map_err(|_| Failure::Config {
            field: "fleet-runtime.json: plan",
            fix: "the warm plan must apply the file, coordination and receive capabilities",
        })?;
    if let Some(host) = &codex_host {
        warm = warm
            .with_local_codex_from_host(host)
            .map_err(|_| Failure::Config {
                field: "fleet-runtime.json: local_codex",
                fix: "the warm bridge must accept the host's local codex binding",
            })?;
    }
    // ADR-192: Claude Code agents, with their own environment and binding.
    if let Some(claude) = config.claude {
        let mut claude_environment = base;
        let local = match claude.local_claude {
            Some(local) => Some(
                hagency_execution::LocalCodex::new_claude(
                    local.preset,
                    local.seat,
                    local.home,
                    local.config_dir,
                )
                .map_err(|_| Failure::Config {
                    field: "fleet-runtime.json: claude.local_claude",
                    fix: "preset, seat, home and config_dir must form a valid local Claude binding",
                })?,
            ),
            None => {
                let runtime_home = runtime_home()?;
                claude_environment.insert("HOME".into(), runtime_home.clone().into_os_string());
                claude_environment
                    .insert("CLAUDE_CONFIG_DIR".into(), runtime_home.into_os_string());
                None
            }
        };
        warm = warm
            .with_claude(claude.executable, claude_environment, local)
            .map_err(|_| Failure::Config {
                field: "fleet-runtime.json: claude",
                fix: "the Claude executable and binding must join the warm plan",
            })?;
    }
    let homes = hagency_store::agent_home::ManagedHomePlan::new(
        config.home.root,
        config.home.projects,
        config.home.task_client,
    )
    .map_err(|_| Failure::Config {
        field: "fleet-runtime.json: home",
        fix: "the home root, task client and at most 16 projects must form a managed home plan",
    })?;
    Ok(FleetRuntime {
        warm,
        homes,
        setup: super::fleet::Setup {
            state: state.to_owned(),
            limit: config.file_limit,
            send: config.send_file,
            receive: config.receive_file,
            limits,
        },
        matrix_limits: matrix_limits(
            origin,
            config.matrix_request_interval_ms,
            config.matrix_sdk_timeout_ms,
        )?,
    })
}

/// ADR-192: `fleet-runtime.json` may name a Codex runtime, a Claude Code
/// runtime, or both; never neither, and never half of the Codex one.
#[cfg(all(test, unix))]
mod fleet_runtime_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    struct Fixture {
        _root: tempfile::TempDir,
        root: PathBuf,
        state: PathBuf,
    }
    fn private_dir(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap();
        let state = path.join("state");
        for dir in [&state, &path.join("homes")] {
            private_dir(dir);
        }
        for name in ["codex", "claude"] {
            std::fs::write(path.join(name), format!("fixture {name} executable")).unwrap();
        }
        Fixture {
            _root: root,
            root: path,
            state,
        }
    }
    fn executable(f: &Fixture, name: &str) -> (PathBuf, String) {
        let path = f.root.join(name);
        let digest = Sha256::digest(std::fs::read(&path).unwrap())
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect();
        (path, digest)
    }
    fn document(f: &Fixture) -> serde_json::Value {
        serde_json::json!({
            "profile": "palpo_fleet_runtime_v1",
            "file_limit": 4194304, "operation_ms": 300000, "response_ms": 2000,
            "idle_ms": 1200000,
            "home": {"root": f.root.join("homes"),
                "task_client": std::env::current_exe().unwrap(), "projects": []},
        })
    }
    fn load(f: &Fixture, document: &serde_json::Value) -> Result<FleetRuntime, Failure> {
        private::replace(
            &f.state.join("fleet-runtime.json"),
            &serde_json::to_vec(document).unwrap(),
        )
        .unwrap();
        load_fleet_runtime(
            &f.state,
            "127.0.0.1:13300".parse().unwrap(),
            "https://matrix.example.test",
        )
    }
    #[test]
    fn native_fleet_runtime_may_run_claude_only() {
        let f = fixture();
        let (claude, digest) = executable(&f, "claude");
        let mut document = document(&f);
        document["claude"] = serde_json::json!({"executable": claude, "executable_sha256": digest});
        assert!(load(&f, &document).is_ok());
    }
    #[test]
    fn native_fleet_runtime_may_run_codex_and_claude() {
        let f = fixture();
        let (codex, codex_digest) = executable(&f, "codex");
        let (claude, claude_digest) = executable(&f, "claude");
        let (home, config) = (f.root.join("user-home"), f.root.join("user-claude"));
        private_dir(&home);
        private_dir(&config);
        let mut document = document(&f);
        document["executable"] = serde_json::json!(codex);
        document["executable_sha256"] = serde_json::json!(codex_digest);
        document["claude"] = serde_json::json!({"executable": claude,
            "executable_sha256": claude_digest,
            "local_claude": {"profile": "provider_owned_claude_v1", "preset": "local_claude",
                "seat": "local_claude_seat", "home": home, "config_dir": config}});
        assert!(load(&f, &document).is_ok());
    }
    #[test]
    fn native_fleet_runtime_refuses_no_runtime_or_half_of_codex() {
        let f = fixture();
        let (codex, codex_digest) = executable(&f, "codex");
        let (claude, claude_digest) = executable(&f, "claude");
        let claude_block =
            serde_json::json!({"executable": claude, "executable_sha256": claude_digest});
        // Neither runtime.
        assert!(load(&f, &document(&f)).is_err());
        // A Codex executable without its digest, beside a valid Claude runtime.
        let mut half = document(&f);
        half["executable"] = serde_json::json!(codex);
        half["claude"] = claude_block.clone();
        assert!(load(&f, &half).is_err());
        // A Codex sign-in folder with no Codex executable.
        let mut orphan = document(&f);
        orphan["claude"] = claude_block.clone();
        orphan["local_codex"] = serde_json::json!({"profile": "provider_owned_codex_v1",
            "preset": "p", "seat": "s", "home": f.root, "codex_home": f.root});
        assert!(load(&f, &orphan).is_err());
        // A Claude folder block under the wrong profile.
        let mut profile = document(&f);
        profile["executable"] = serde_json::json!(codex);
        profile["executable_sha256"] = serde_json::json!(codex_digest);
        profile["claude"] = claude_block;
        profile["claude"]["local_claude"] = serde_json::json!({"profile": "provider_owned_codex_v1",
            "preset": "p", "seat": "s", "home": f.root, "config_dir": f.root});
        assert!(load(&f, &profile).is_err());
    }
}
