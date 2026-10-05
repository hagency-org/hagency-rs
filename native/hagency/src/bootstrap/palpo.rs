//! Independently scoped outbound adapters; no implicit execution authority.
use super::{Failure, config::read};
use hagency_core::authority::Registration;
use hagency_palpo::{Adapter, CancellationToken, Error, HostConfig, Limits};
use hagency_store::{DomainStore, Store, outbound::RegistrationIdentity};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::task::JoinHandle;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    profile: String,
    endpoint: String,
    registration: Registration,
    machine_generation: u64,
}

pub(super) struct Prepared {
    state: std::path::PathBuf,
    host: HostConfig,
    registration: RegistrationIdentity,
    work: Option<Work>,
}
/// What the custody consumer needs; absent without `palpo-appservice.json`.
struct Work {
    appservice: super::palpo_work::Appservice,
    probes: Arc<super::palpo_work::Probes>,
    fleet: String,
    generation: u64,
}
impl Prepared {
    pub(super) fn load_all(state: &Path) -> Result<Vec<Self>, Failure> {
        let directories =
            super::palpo_import::profile_directories(state).map_err(|_| Failure::Config {
                field: "Palpo profiles",
                fix: "every profile must be private and readable",
            })?;
        let mut seen = std::collections::BTreeSet::new();
        let mut result = Vec::new();
        for directory in directories {
            let profile = Self::load(&directory)?;
            if !seen.insert(profile.registration.fleet_id.clone()) {
                return Err(Failure::Config {
                    field: "Palpo profiles",
                    fix: "an engagement must have exactly one credential directory",
                });
            }
            result.push(profile);
        }
        Ok(result)
    }
    pub(super) fn load(state: &Path) -> Result<Self, Failure> {
        let value: Config = serde_json::from_slice(&read(
            &state.join("palpo-transport.json"),
            16 * 1024,
            "palpo-transport.json",
        )?)
        .map_err(|_| Failure::Config {
            field: "palpo-transport.json",
            fix: "the file must be valid JSON for the palpo v2 transport profile",
        })?;
        // The endpoint's scheme rule is the host credential's own (https, or
        // plain http to a loopback Palpo), the same rule the import applies.
        if value.profile != "palpo_v2_resources_v1" {
            return Err(Failure::Config {
                field: "palpo-transport.json: profile",
                fix: "profile must be palpo_v2_resources_v1",
            });
        }
        value.registration.validate().map_err(|_| Failure::Config {
            field: "palpo-transport.json: registration",
            fix: "the six-field registration must be present and well-formed",
        })?;
        if state
            .parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            == Some("palpo-engagements")
            && state.file_name().and_then(|s| s.to_str())
                != Some(value.registration.fleet_id.as_str())
        {
            return Err(Failure::Config {
                field: "Palpo engagement directory",
                fix: "the directory must match the profile's engagement ID",
            });
        }
        let registration_fingerprint = hagency_store::publication_fingerprint(&value.registration)
            .map_err(|_| Failure::Config {
                field: "palpo-transport.json: registration",
                fix: "the registration digest must compute; keep the fields ASCII",
            })?;
        let work = super::palpo_work::Appservice::load(state, &value.registration.server_name).map(
            |appservice| Work {
                appservice,
                probes: super::palpo_work::Probes::new(state),
                fleet: value.registration.fleet_id.clone(),
                generation: value.machine_generation,
            },
        );
        let registration = RegistrationIdentity {
            binding: if state.file_name().and_then(|n| n.to_str())
                == Some(value.registration.fleet_id.as_str())
            {
                format!("native-palpo-v2-{}", value.registration.fleet_id)
            } else {
                "native-palpo-v2".into()
            },
            side_id: value.registration.server_name,
            fleet_id: value.registration.fleet_id,
            registration_generation: value.registration.generation,
            registration_fingerprint,
        };
        let token = read(
            &state.join("palpo.machine_token"),
            4096,
            "palpo.machine_token",
        )?;
        let token = std::str::from_utf8(&token).map_err(|_| Failure::Config {
            field: "palpo.machine_token",
            fix: "the token must be valid UTF-8 (max 4096 bytes, owner-private 0600)",
        })?;
        let mut host = HostConfig::new(
            registration.clone(),
            &value.endpoint,
            token,
            value.machine_generation,
            Limits::default(),
        )
        .map_err(|_| Failure::Config {
            field: "palpo-transport.json: machine credentials",
            fix: "machine token and generation must form a valid host credential",
        })?;
        let ca = state.join("palpo.ca.pem");
        match std::fs::symlink_metadata(&ca) {
            Ok(_) => {
                host = host
                    .with_root_pem(&read(&ca, 16 * 1024, "palpo.ca.pem")?)
                    .map_err(|_| Failure::Config {
                        field: "palpo.ca.pem",
                        fix: "the CA bundle must be a usable PEM root (max 16 KiB)",
                    })?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(Failure::Config {
                    field: "palpo.ca.pem",
                    fix: "the file must be readable by the service owner (stat failed)",
                });
            }
        }
        Ok(Self {
            state: state.to_owned(),
            host,
            registration,
            work,
        })
    }
}

/// Whether `serve --palpo-transport` found an imported fleet at boot. A fresh
/// install has none yet: TS starts with no outbound fleet and picks one up
/// when the operator imports it, so an absent file is "waiting for the
/// import", never a startup refusal. A present but broken file still refuses.
pub(super) fn imported(state: &Path) -> Result<bool, Failure> {
    super::palpo_import::profile_directories(state)
        .map(|p| !p.is_empty())
        .map_err(|_| Failure::Config {
            field: "Palpo profiles",
            fix: "the profile directories must be private and readable",
        })
}

/// The service's Palpo transports, startable while the service runs (TS
/// `reconcileOutboundFleets`: an imported outbound fleet starts without a
/// restart, a re-import replaces its client). Bootstrap starts the imported
/// fleet at boot through this same handle, and closes it at shutdown.
#[derive(Clone)]
pub(crate) struct Live(Arc<LiveInner>);
struct LiveInner {
    state: std::path::PathBuf,
    store: Store,
    domain: DomainStore,
    status: StatusHandle,
    /// `--palpo-transport`: without it an import is saved for the next start
    /// that enables the transport, and nothing connects now.
    enabled: bool,
    owner: tokio::sync::Mutex<BTreeMap<String, Owner>>,
    imports: tokio::sync::Mutex<()>,
    /// ADR-187: an imported fleet's service (agents, approvals) with no
    /// coordinator; absent on a coordinator install, which runs its own.
    fleet: Option<FleetMode>,
    /// Shutdown wins over a concurrent import: once set, nothing starts.
    closed: std::sync::atomic::AtomicBool,
    cancel: Mutex<BTreeMap<String, CancellationToken>>,
}
struct FleetMode {
    address: std::net::SocketAddr,
    service: tokio::sync::Mutex<BTreeMap<String, super::fleet_service::FleetService>>,
}
/// What an import reports: the saved fleet's public facts and whether the
/// transport was started. Never a token.
pub(crate) struct Connected {
    pub(crate) imported: super::palpo_import::Imported,
    pub(crate) started: bool,
}
// The wrapped errors are read through `Debug` when a refusal is logged.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum ImportError {
    Invalid(&'static str),
    Store(hagency_store::Error),
    Start(Failure),
    Closed,
}
impl From<super::palpo_import::Error> for ImportError {
    fn from(error: super::palpo_import::Error) -> Self {
        match error {
            super::palpo_import::Error::Invalid(field) => Self::Invalid(field),
            super::palpo_import::Error::Store(error) => Self::Store(error),
        }
    }
}
impl Live {
    pub(crate) fn new(
        state: std::path::PathBuf,
        store: Store,
        domain: DomainStore,
        status: StatusHandle,
        enabled: bool,
    ) -> Self {
        Self(Arc::new(LiveInner {
            state,
            store,
            domain,
            status,
            enabled,
            owner: tokio::sync::Mutex::new(BTreeMap::new()),
            imports: tokio::sync::Mutex::new(()),
            fleet: None,
            closed: std::sync::atomic::AtomicBool::new(false),
            cancel: Mutex::new(BTreeMap::new()),
        }))
    }
    /// ADR-187: this service also runs an imported fleet's agents and
    /// approvals itself (no coordinator install). Set before sharing.
    pub(crate) fn with_fleet_service(self, address: std::net::SocketAddr) -> Self {
        let mut inner = Arc::try_unwrap(self.0).unwrap_or_else(|_| panic!("set before sharing"));
        inner.fleet = Some(FleetMode {
            address,
            service: tokio::sync::Mutex::new(BTreeMap::new()),
        });
        Self(Arc::new(inner))
    }
    /// ADR-189: the state directory and listen address the setup page uses.
    pub(crate) fn state_dir(&self) -> &Path {
        &self.0.state
    }
    pub(crate) fn fleet_address(&self) -> Option<std::net::SocketAddr> {
        self.0.fleet.as_ref().map(|fleet| fleet.address)
    }
    /// Whether a Palpo configuration has been imported into this state.
    pub(crate) fn is_imported(&self) -> bool {
        imported(&self.0.state).unwrap_or(false)
    }
    async fn start_fleet(&self, registration: &Registration, state: &Path) -> Result<(), Failure> {
        let Some(fleet) = &self.0.fleet else {
            return Ok(());
        };
        let mut service = fleet.service.lock().await;
        if let Some(previous) = service.get_mut(&registration.fleet_id) {
            previous.close().await?;
        }
        if !self.0.closed.load(std::sync::atomic::Ordering::Acquire) {
            service.insert(
                registration.fleet_id.clone(),
                super::fleet_service::FleetService::start_scoped(
                    state.to_owned(),
                    self.0.state.clone(),
                    fleet.address,
                    self.0.domain.clone(),
                    registration.fleet_id.clone(),
                    registration.server_name.clone(),
                ),
            );
        }
        Ok(())
    }
    pub(crate) fn status(&self) -> &StatusHandle {
        &self.0.status
    }
    /// Replace the running transport (if any) with one built from `prepared`.
    async fn replace(&self, prepared: Prepared) -> Result<(), Failure> {
        let registration = prepared.registration.clone();
        let profile = prepared.state.clone();
        let mut owner = self.0.owner.lock().await;
        if let Some(old) = owner.get_mut(&registration.fleet_id) {
            // A previous transport that does not acknowledge its close keeps
            // its original join; the new one starts only after it settled.
            old.close().await?;
        }
        if self.0.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(Failure::Cancelled);
        }
        let status = self.0.status.child(&registration.fleet_id);
        status.set("starting", None);
        let started = Owner::start(
            prepared,
            self.0.store.clone(),
            self.0.domain.clone(),
            status,
        );
        self.0
            .cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(registration.fleet_id.clone(), started.cancel.clone());
        owner.insert(registration.fleet_id.clone(), started);
        drop(owner);
        // Replace only this engagement's service so new credentials take effect.
        if let Ok(registration) = self
            .0
            .domain
            .provisioning_registration(registration.fleet_id.clone())
            .await
        {
            self.start_fleet(&registration, &profile).await?;
        }
        Ok(())
    }
    pub(super) async fn start(&self, prepared: Prepared) -> Result<(), Failure> {
        self.replace(prepared).await
    }
    /// The console import: validate the owner download exactly like the CLI,
    /// save it through the running store, then connect.
    pub(crate) async fn import(
        &self,
        raw: &str,
        homeserver: &str,
    ) -> Result<Connected, ImportError> {
        let _import = self.0.imports.lock().await;
        if self.0.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(ImportError::Closed);
        }
        let homeserver = super::palpo_import::homeserver(homeserver)?;
        super::association::validate_import(&self.0.state, raw, &homeserver)
            .map_err(|_| ImportError::Invalid("association binding"))?;
        let (mut registration, mut appservice, machine, endpoint, generation) =
            super::palpo_import::parse(raw)?;
        appservice["homeserver"] = serde_json::json!(homeserver);
        let profile =
            super::palpo_import::profile_directory(&self.0.state, &registration.fleet_id)?;
        let domain = &self.0.domain;
        // A re-import of the same fleet keeps a reception an earlier probe bound.
        if let Ok(current) = domain
            .provisioning_registration(registration.fleet_id.clone())
            .await
        {
            registration.reception_room_id = current.reception_room_id;
        }
        let policy = super::palpo_import::coordinator_profile(raw, &registration)?;
        domain
            .import_coordinator_registration(registration.clone(), policy)
            .await
            .map_err(ImportError::Store)?;
        super::palpo_import::write(
            &profile,
            &registration,
            &appservice,
            &machine,
            &endpoint,
            generation,
        )?;
        // The project side the console lists and verifies: the homeserver
        // address and the App Service credential the representative acts
        // with (TS `PUT /api/project-sides/:id/credential`).
        // The legacy side record is hostname keyed; secondary engagements must
        // never replace its credentials. They are addressed by exact fleet ID.
        if profile == self.0.state {
            let side = registration.server_name.clone();
            domain
                .ensure_side(side.clone())
                .await
                .map_err(ImportError::Store)?;
            domain
                .set_api_base_url(side.clone(), Some(homeserver.clone()))
                .await
                .map_err(ImportError::Store)?;
            let credential = serde_json::json!({
                "kind": "appservice",
                "asToken": appservice["as_token"], "hsToken": appservice["hs_token"],
                "namespace": appservice["namespace"],
                "senderLocalpart": appservice["sender_localpart"], "url": appservice["url"],
            });
            domain
                .set_credential(side, Some(credential), false)
                .await
                .map_err(ImportError::Store)?;
        }
        let imported = super::palpo_import::Imported {
            fleet_id: registration.fleet_id.clone(),
            server_name: registration.server_name.clone(),
            representative: registration.representative_mxid.clone(),
            approval_bot: registration.approval_bot_mxid.clone(),
            endpoint,
            reception: registration.reception_room_id.clone(),
        };
        if !self.0.enabled {
            return Ok(Connected {
                imported,
                started: false,
            });
        }
        let prepared = Prepared::load(&profile).map_err(ImportError::Start)?;
        self.replace(prepared).await.map_err(ImportError::Start)?;
        Ok(Connected {
            imported,
            started: true,
        })
    }
    pub(super) fn cancel(&self) {
        self.0
            .closed
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(fleet) = &self.0.fleet
            && let Ok(service) = fleet.service.try_lock()
        {
            for service in service.values() {
                service.cancel();
            }
        }
        for cancel in self
            .0
            .cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            cancel.cancel();
        }
    }
    pub(super) async fn close(&self) -> Result<(), Failure> {
        self.cancel();
        let mut result = Ok(());
        // The fleet's agents and approvals close before the transport.
        if let Some(fleet) = &self.0.fleet {
            for service in fleet.service.lock().await.values_mut() {
                if let Err(error) = service.close().await {
                    result = Err(error);
                }
            }
        }
        for owner in self.0.owner.lock().await.values_mut() {
            if let Err(error) = owner.close().await {
                result = Err(error);
            }
        }
        result
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct Status {
    configured: bool,
    state: &'static str,
    error: Option<&'static str>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    engagements: BTreeMap<String, &'static str>,
}
#[derive(Clone)]
pub(crate) struct StatusHandle(
    Arc<Mutex<Status>>,
    Arc<Mutex<BTreeMap<String, StatusHandle>>>,
);
impl StatusHandle {
    pub(crate) fn new(configured: bool) -> Self {
        Self(
            Arc::new(Mutex::new(Status {
                configured,
                state: if configured { "starting" } else { "disabled" },
                error: None,
                engagements: BTreeMap::new(),
            })),
            Default::default(),
        )
    }
    /// `--palpo-transport` with no fleet imported yet.
    pub(super) fn awaiting() -> Self {
        Self(
            Arc::new(Mutex::new(Status {
                configured: true,
                state: "awaiting_import",
                error: None,
                engagements: BTreeMap::new(),
            })),
            Default::default(),
        )
    }
    pub(crate) fn get(&self) -> Status {
        let mut status = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let children = self.1.lock().unwrap_or_else(|e| e.into_inner());
        if !children.is_empty() {
            for (id, child) in children.iter() {
                let child = child.get();
                status.engagements.insert(id.clone(), child.state);
                if child.error.is_some() {
                    status.error = child.error;
                }
            }
            let values = status.engagements.values();
            status.state = if values.clone().all(|s| *s == "running") {
                "running"
            } else if values.clone().all(|s| *s == "stopped") {
                "stopped"
            } else if values
                .clone()
                .any(|s| matches!(*s, "unavailable" | "outcome_unknown"))
            {
                "unavailable"
            } else {
                "starting"
            };
        }
        status
    }
    fn child(&self, id: &str) -> Self {
        self.1
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.into())
            .or_insert_with(|| Self::new(true))
            .clone()
    }
    /// The state word alone, for the readiness rollup (brief 19): the full
    /// `Status` stays console-only; `/health` names components by state
    /// words, never private detail.
    pub(crate) fn state(&self) -> &'static str {
        self.get().state
    }
    fn set(&self, state: &'static str, error: Option<&'static str>) {
        let mut status = self.0.lock().unwrap_or_else(|e| e.into_inner());
        status.state = state;
        status.error = error;
    }
    fn finish(&self, result: Result<(), Error>) {
        match result {
            Ok(()) => self.set("stopped", None),
            Err(error) => self.set(
                if matches!(error, Error::OutcomeUnknown | Error::Unavailable) {
                    "outcome_unknown"
                } else {
                    "unavailable"
                },
                Some(error_label(error)),
            ),
        }
    }
}
fn error_label(error: Error) -> &'static str {
    match error {
        Error::Config => "config",
        Error::Busy => "busy",
        Error::Cancelled => "cancelled",
        Error::Timeout => "timeout",
        Error::Transport => "transport",
        Error::Redirect => "redirect",
        Error::Headers => "headers",
        Error::BodyTooLarge => "body_too_large",
        Error::InvalidJson => "invalid_json",
        Error::Wire => "wire",
        Error::Generation => "generation",
        Error::Unauthorized => "unauthorized",
        Error::Remote(_) => "remote",
        Error::Rejected(_) => "rejected",
        Error::Custody => "custody",
        Error::Unavailable => "unavailable",
        Error::OutcomeUnknown => "outcome_unknown",
        Error::Capacity => "capacity",
        Error::Conflict => "conflict",
    }
}

// A panic or executor abandonment must not leave a false running observation.
struct Completion {
    status: StatusHandle,
    finished: bool,
}
impl Drop for Completion {
    fn drop(&mut self) {
        if !self.finished {
            self.status.set("outcome_unknown", Some("worker"));
        }
    }
}

pub(super) struct Owner {
    cancel: CancellationToken,
    task: Option<JoinHandle<Result<(), Error>>>,
    joined: Option<Result<(), Failure>>,
}
impl Owner {
    /// Synchronous handoff: Bootstrap retains the original join before awaiting.
    pub(super) fn start(
        prepared: Prepared,
        store: Store,
        domain: DomainStore,
        status: StatusHandle,
    ) -> Self {
        let cancel = CancellationToken::new();
        let signal = cancel.clone();
        let task = tokio::spawn(async move {
            let mut completion = Completion {
                status,
                finished: false,
            };
            let result = async {
                if signal.is_cancelled() {
                    return Ok(());
                }
                // Registration is an existing canonical prerequisite. In
                // particular, do not register/rotate then fail custody attach.
                domain
                    .check_publication_registration(prepared.registration)
                    .await?;
                if signal.is_cancelled() {
                    return Ok(());
                }
                // Preserve the original activation future through its receipt.
                let mut adapter = Adapter::attach(prepared.host, store).await?;
                if let Some(work) = &prepared.work {
                    adapter = adapter.with_probe_receipts(work.probes.clone());
                }
                if signal.is_cancelled() {
                    return Ok(());
                }
                completion.status.set("running", None);
                // No cancellation select around this joined operation. Its
                // original received custody/known receipts settle before return.
                // ADR109 rechecks domain identity after custody waits before
                // HTTP admission; already admitted bytes cannot be recalled.
                // The custody consumer runs beside it and stops with it.
                let consumer = async {
                    if let Some(work) = &prepared.work {
                        super::palpo_work::run(
                            &adapter,
                            &work.probes,
                            &domain,
                            &work.appservice,
                            &work.fleet,
                            work.generation,
                            &signal,
                        )
                        .await;
                    }
                };
                let (result, ()) = tokio::join!(
                    async {
                        let result = adapter.run_with_resources(&domain, &signal).await;
                        signal.cancel();
                        result
                    },
                    consumer
                );
                result
            }
            .await;
            completion.status.finish(result);
            completion.finished = true;
            result
        });
        Self {
            cancel,
            task: Some(task),
            joined: None,
        }
    }
    pub(super) fn cancel(&self) {
        self.cancel.cancel();
    }
    pub(super) async fn close(&mut self) -> Result<(), Failure> {
        self.cancel();
        if let Some(result) = self.joined {
            return result;
        }
        let task = self.task.as_mut().ok_or(Failure::OutcomeUnknown)?;
        let joined = match tokio::time::timeout(Duration::from_secs(2), task).await {
            Ok(result) => result,
            Err(_) => return Err(Failure::OutcomeUnknown), // original join retained
        };
        // Adapter failure is separately retained status, not an active worker.
        // A failed join remains unknown, never inferred to be a clean close.
        let result = joined.map(|_| ()).map_err(|_| Failure::OutcomeUnknown);
        self.joined = Some(result);
        self.task = None;
        result
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        // Abrupt abandonment only requests cancellation; it is no close ACK.
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn native_palpo_multi_engagement_health_does_not_hide_a_failed_transport() {
        let status = StatusHandle::new(true);
        let one = status.child("first");
        let two = status.child("second");
        one.set("running", None);
        assert_eq!(status.state(), "starting");
        two.set("unavailable", Some("transport"));
        assert_eq!(status.state(), "unavailable");
        assert_eq!(status.get().engagements.len(), 2);
        two.set("running", None);
        assert_eq!(status.state(), "running");
        one.set("stopped", None);
        two.set("stopped", None);
        assert_eq!(status.state(), "stopped");
    }

    #[tokio::test]
    async fn native_palpo_service_cancel_custody_retained_join() {
        // A gated task models a delayed worker completion, not HTTPS acceptance.
        // The production close implementation must keep the original join even
        // after caller cancellation and after its own unchanged 2s observation.
        let (release, wait) = tokio::sync::oneshot::channel();
        let finishes = Arc::new(AtomicUsize::new(0));
        let finished = finishes.clone();
        let task = tokio::spawn(async move {
            wait.await.unwrap();
            finished.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let mut owner = Owner {
            cancel: CancellationToken::new(),
            task: Some(task),
            joined: None,
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(10), owner.close())
                .await
                .is_err()
        );
        assert_eq!(owner.close().await, Err(Failure::OutcomeUnknown));
        assert_eq!(finishes.load(Ordering::SeqCst), 0);
        assert!(owner.task.is_some());
        release.send(()).unwrap();
        assert_eq!(owner.close().await, Ok(()));
        assert_eq!(owner.close().await, Ok(()));
        assert_eq!(finishes.load(Ordering::SeqCst), 1);

        let status = StatusHandle::new(true);
        let observed = status.clone();
        let task = tokio::spawn(async move {
            let _completion = Completion {
                status: observed,
                finished: false,
            };
            panic!("offline original Palpo worker panic");
            #[allow(unreachable_code)]
            Ok(())
        });
        let mut owner = Owner {
            cancel: CancellationToken::new(),
            task: Some(task),
            joined: None,
        };
        assert_eq!(owner.close().await, Err(Failure::OutcomeUnknown));
        assert_eq!(owner.close().await, Err(Failure::OutcomeUnknown));
        assert_eq!(status.get().state, "outcome_unknown");
        assert_eq!(status.get().error, Some("worker"));
    }

    #[test]
    fn native_palpo_service_configuration_refusal_projection() {
        let status = StatusHandle::new(true);
        status.finish(Err(Error::OutcomeUnknown));
        let value = serde_json::to_string(&status.get()).unwrap();
        assert!(value.len() <= 128);
        status.finish(Err(Error::Remote(u16::MAX)));
        let value = serde_json::to_string(&status.get()).unwrap();
        assert!(!value.contains("65535"));
        assert_eq!(status.get().error, Some("remote"));
        assert_eq!(status.get().state, "unavailable");
    }

    #[test]
    fn native_palpo_service_error_label_names_every_class_without_status_detail() {
        // The failure label is the class word only: a Remote/Rejected status code
        // never leaks a value into the operator projection.
        assert_eq!(error_label(Error::Rejected(400)), "rejected");
        assert_eq!(error_label(Error::Rejected(u16::MAX)), "rejected");
        assert_eq!(error_label(Error::Remote(429)), "remote");
        assert_eq!(error_label(Error::Unauthorized), "unauthorized");
        assert_eq!(error_label(Error::OutcomeUnknown), "outcome_unknown");
        assert_eq!(error_label(Error::Capacity), "capacity");
    }
}
