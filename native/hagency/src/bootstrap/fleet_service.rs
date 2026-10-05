//! ADR-187: an imported Palpo fleet's service, with no coordinator agent.
//!
//! Started by `palpo::Live` once a fleet is imported (at import, or at boot
//! when one already is). A supervisor walks four stages, each retried with
//! backoff and never exiting (ADR-183 decision 0):
//!
//! 1. `awaiting_runtime_config` — `fleet-runtime.json` (the operator's local
//!    Codex runtime) is not there yet;
//! 2. `awaiting_reception` — Palpo's Verify connection has not bound the
//!    reception room yet (the registration fingerprint changes when it does,
//!    and every SDK binding below hashes it);
//! 3. `identities` — the representative's device and the local keys exist;
//! 4. `running` — the provisioning host, the agent service and one approval
//!    pump per owner.
//!
//! The provisioning loop runs one pass per tick: for each engagement waiting
//! to be provisioned it pins its owner's anchor on first use (§C), ensures
//! that owner's approval-bot device (amendment), then runs the host's pass,
//! where one engagement's refusal never stops another (ADR-182).
use super::{
    Failure, approval, config,
    fleet::{Provider, Service},
    fleet_identity,
};
use hagency_core::replies::{MatrixTransportObservation, RoomPrivacy};
use hagency_matrix::{
    ApplicationServiceCredential, ApprovalCollector, CancellationToken, FleetApprovalAnchor,
    HostApprovalConfig, HostConfig, HostIdentity, HostRoom, TokenProvisioningHost,
};
use hagency_store::{DomainStore, private};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::mpsc;

const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const PASS_PERIOD: Duration = Duration::from_secs(2);

/// The fleet service's stage. Each change is logged, so the operator can see
/// which step the supervisor is waiting on.
#[derive(Clone)]
pub(crate) struct Stage(Arc<Mutex<&'static str>>);
impl Stage {
    fn set(&self, stage: &'static str) {
        let mut current = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if *current != stage {
            tracing::info!(from = *current, to = stage, "fleet service stage");
            *current = stage;
        }
    }
}

/// The running fleet service; cancelled and joined by `palpo::Live`.
pub(crate) struct FleetService {
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<Result<(), Failure>>>,
    joined: Option<Result<(), Failure>>,
}
impl FleetService {
    pub(crate) fn start_scoped(
        state: PathBuf,
        runtime_state: PathBuf,
        address: SocketAddr,
        domain: DomainStore,
        fleet_id: String,
        server_name: String,
    ) -> Self {
        let cancel = CancellationToken::new();
        let stage = Stage(Arc::new(Mutex::new("starting")));
        let task = tokio::spawn(supervise(
            state,
            runtime_state,
            address,
            domain,
            fleet_id,
            server_name,
            stage,
            cancel.clone(),
        ));
        Self {
            cancel,
            task: Some(task),
            joined: None,
        }
    }
    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }
    pub(crate) async fn close(&mut self) -> Result<(), Failure> {
        self.cancel.cancel();
        if let Some(result) = self.joined {
            return result;
        }
        let task = self.task.as_mut().ok_or(Failure::OutcomeUnknown)?;
        let joined = tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .map_err(|_| Failure::OutcomeUnknown)?;
        let result = joined
            .map_err(|_| Failure::OutcomeUnknown)
            .and_then(|result| result);
        self.task = None;
        self.joined = Some(result);
        result
    }
}

async fn pause(cancel: &CancellationToken, backoff: &mut Duration) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => return false,
        _ = tokio::time::sleep(*backoff) => {}
    }
    *backoff = (*backoff * 2).min(BACKOFF_MAX);
    true
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn secret(state: &Path, name: &str) -> Result<String, Failure> {
    let bytes = private::read_secret(&state.join(name)).map_err(|_| Failure::Config {
        field: "fleet service state file",
        fix: "the fleet identity files must exist and be owner-private",
    })?;
    String::from_utf8(bytes)
        .map(|s| s.trim().to_owned())
        .map_err(|_| Failure::OutcomeUnknown)
}

#[allow(clippy::too_many_arguments)]
async fn supervise(
    state: PathBuf,
    runtime_state: PathBuf,
    address: SocketAddr,
    domain: DomainStore,
    fleet_id: String,
    server_name: String,
    stage: Stage,
    cancel: CancellationToken,
) -> Result<(), Failure> {
    let mut backoff = BACKOFF_MIN;
    // 1. The operator's local runtime settings.
    while !config::fleet_runtime_configured(&runtime_state) {
        stage.set("awaiting_runtime_config");
        if !pause(&cancel, &mut Duration::from_secs(5)).await {
            return Ok(());
        }
    }
    // 2. The reception Palpo's Verify connection binds.
    let registration = loop {
        match domain.provisioning_registration(fleet_id.clone()).await {
            Ok(registration) if !registration.reception_room_id.is_empty() => break registration,
            _ => stage.set("awaiting_reception"),
        }
        if !pause(&cancel, &mut Duration::from_secs(5)).await {
            return Ok(());
        }
    };
    // 3. The fleet's own identities and keys.
    loop {
        stage.set("identities");
        match fleet_identity::ensure(&state, &fleet_id, &server_name).await {
            Ok(_) => break,
            Err(error) => tracing::warn!(%error, "fleet identities not ready; retrying"),
        }
        if !pause(&cancel, &mut backoff).await {
            return Ok(());
        }
    }
    backoff = BACKOFF_MIN;
    // 4. The provisioning host and the agent service.
    let running = loop {
        match build(&state, &runtime_state, address, &domain, &registration) {
            Ok(running) => break running,
            Err(error) => {
                stage.set("refused_config");
                tracing::warn!(?error, "fleet service configuration refused; retrying");
            }
        }
        if !pause(&cancel, &mut backoff).await {
            return Ok(());
        }
    };
    stage.set("running");
    run(running, &state, &domain, &registration, &stage, &cancel).await
}

struct Running {
    host: Arc<TokenProvisioningHost>,
    service: Option<Service>,
    owner_notices: Arc<Mutex<BTreeMap<String, mpsc::Sender<hagency_execution::ApprovalRequests>>>>,
    agents: approval::AgentDirectory,
    homeserver: String,
    matrix_limits: hagency_matrix::Limits,
    matrix_root: Option<Vec<u8>>,
}

fn build(
    state: &Path,
    runtime_state: &Path,
    address: SocketAddr,
    domain: &DomainStore,
    registration: &hagency_core::authority::Registration,
) -> Result<Running, Failure> {
    let appservice: Value = serde_json::from_str(&secret(state, "palpo-appservice.json")?)
        .map_err(|_| Failure::OutcomeUnknown)?;
    let homeserver = format!(
        "{}/",
        text(&appservice, "homeserver")
            .ok_or(Failure::OutcomeUnknown)?
            .trim_end_matches('/')
    );
    let runtime = config::load_fleet_runtime(runtime_state, address, &homeserver)?;
    let key: [u8; 32] = private::read_secret(&state.join("matrix.provisioning_key"))
        .map_err(|_| Failure::OutcomeUnknown)?
        .try_into()
        .map_err(|_| Failure::OutcomeUnknown)?;
    let refused = |field: &'static str| Failure::Config {
        field,
        fix: "the imported fleet's files must form a provisioning host",
    };
    let representative = secret(state, "matrix.representative_token")?;
    let matrix_root = config::matrix_root(state)?;
    let host = TokenProvisioningHost::application_service(
        registration.clone(),
        &homeserver,
        ApplicationServiceCredential::new(
            &secret(state, "matrix.appservice_token")?,
            &format!("{}_", registration.fleet_id),
        )
        .map_err(|_| refused("matrix.appservice_token"))?,
        state.to_owned(),
        key,
        runtime.matrix_limits.clone(),
    )
    .and_then(|host| match &matrix_root {
        Some(pem) => host.with_root_pem(pem),
        None => Ok(host),
    })
    .and_then(|host| host.with_agent_rooms_pinned_anchors(&representative))
    .and_then(|host| host.with_managed_homes(runtime.homes))
    .and_then(|host| host.with_warm_plan(runtime.warm))
    .map_err(|_| refused("fleet provisioning host"))?;
    let host = Arc::new(host);
    let sweep = Arc::new(
        host.membership_sweep(domain.clone())
            .map_err(|_| refused("membership sweep"))?,
    );
    let owner_notices = Arc::new(Mutex::new(BTreeMap::new()));
    let agents: approval::AgentDirectory = Arc::new(Mutex::new(BTreeMap::new()));
    let service = Service::with_provider(
        domain.clone(),
        Provider::Fleet {
            host: host.clone(),
            sweep,
            owner_notices: owner_notices.clone(),
            agents: agents.clone(),
        },
        runtime.setup,
    )?;
    Ok(Running {
        host,
        service: Some(service),
        owner_notices,
        agents,
        homeserver,
        matrix_limits: runtime.matrix_limits,
        matrix_root,
    })
}

async fn run(
    mut running: Running,
    state: &Path,
    domain: &DomainStore,
    registration: &hagency_core::authority::Registration,
    stage: &Stage,
    cancel: &CancellationToken,
) -> Result<(), Failure> {
    let mut pumps: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    // Coordinator-only notices never arrive here: each agent is admitted with
    // its owner's pump (fleet::Service::admit). This channel only satisfies
    // the service's signature.
    let (unused, _unused_rx) = mpsc::channel(1);
    // Owners first: re-attaching an existing agent needs its owner's approval
    // device attached before the agent service starts.
    if let Err(error) =
        prepare_owners(&running, state, domain, registration, &mut pumps, cancel).await
    {
        tracing::warn!(?error, "fleet owners not ready before start");
    }
    let service_cancel = cancel.child_token();
    let Some(mut service) = running.service.take() else {
        return Err(Failure::OutcomeUnknown);
    };
    let service_task = {
        let cancel = service_cancel.clone();
        tokio::spawn(async move {
            if let Err(error) = service.run(unused, &cancel).await {
                tracing::error!(?error, "fleet agent service stopped");
            }
            service.close().await
        })
    };
    let mut tick = tokio::time::interval(PASS_PERIOD);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tick.tick() => {}
        }
        if service_task.is_finished() {
            break;
        }
        if let Err(error) =
            prepare_owners(&running, state, domain, registration, &mut pumps, cancel).await
        {
            tracing::warn!(?error, "fleet owners not ready this pass");
        }
        match running.host.provision_pass(domain, cancel).await {
            Ok(report) => {
                for (engagement, error) in &report.failed {
                    tracing::warn!(%engagement, ?error, "provision refused this pass");
                }
            }
            Err(error) => tracing::warn!(?error, "provisioning pass refused"),
        }
        stage.set("running");
    }
    service_cancel.cancel();
    // The enclosing FleetService owns the bounded close and original join.
    // Never detach running agents and then start a replacement generation.
    let result = service_task
        .await
        .map_err(|_| Failure::OutcomeUnknown)
        .and_then(|result| result);
    for pump in pumps {
        pump.abort();
    }
    result?;
    if !cancel.is_cancelled() {
        return Err(Failure::OutcomeUnknown);
    }
    Ok(())
}

/// Before a pass: for each engagement waiting to be provisioned, pin its
/// owner's anchor (first use) and make sure that owner's approval device and
/// pump exist and are attached to the host.
async fn prepare_owners(
    running: &Running,
    state: &Path,
    domain: &DomainStore,
    registration: &hagency_core::authority::Registration,
    pumps: &mut Vec<tokio::task::JoinHandle<()>>,
    cancel: &CancellationToken,
) -> Result<(), Failure> {
    let mut engagements = domain
        .pending_provisions(registration.fleet_id.clone())
        .await
        .map_err(|_| Failure::OutcomeUnknown)?;
    engagements.extend(
        running
            .host
            .awaiting_owner_engagements()
            .into_iter()
            .map(|(e, _)| e),
    );
    // Agents already provisioned still need their owner's pump after a restart.
    engagements.extend(
        running
            .host
            .provisioned_engagements(domain)
            .await
            .unwrap_or_default(),
    );
    for engagement in engagements {
        let Some((owner, room)) = domain
            .engagement_owner_room(engagement.clone())
            .await
            .map_err(|_| Failure::OutcomeUnknown)?
        else {
            continue;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let Some(anchor) = fleet_identity::owner_anchor(domain, state, &owner, now)
            .await
            .map_err(|_| Failure::OutcomeUnknown)?
        else {
            // No cross-signing yet: the provision waits for the owner (§C.3).
            continue;
        };
        let attached = running
            .owner_notices
            .lock()
            .map_err(|_| Failure::OutcomeUnknown)?
            .contains_key(&owner);
        if attached {
            continue;
        }
        let device = fleet_identity::owner_approval_device(
            state,
            &registration.fleet_id,
            &registration.server_name,
            &owner,
            &room,
        )
        .await
        .map_err(|_| Failure::OutcomeUnknown)?;
        let collector = owner_collector(
            running,
            state,
            domain,
            registration,
            &owner,
            &anchor,
            &device,
        )?;
        running
            .host
            .attach_owner_approvals(&owner, collector.clone())
            .map_err(|_| Failure::OutcomeUnknown)?;
        let (sender, receiver) = mpsc::channel(16);
        running
            .owner_notices
            .lock()
            .map_err(|_| Failure::OutcomeUnknown)?
            .insert(owner.clone(), sender);
        pumps.push(tokio::spawn(supervise_pump(
            approval::Pump::new(collector, None, domain.clone())
                .with_agents(running.agents.clone()),
            receiver,
            owner,
            cancel.clone(),
        )));
    }
    Ok(())
}

fn owner_collector(
    running: &Running,
    state: &Path,
    domain: &DomainStore,
    registration: &hagency_core::authority::Registration,
    owner: &str,
    anchor: &str,
    device: &fleet_identity::OwnerApprovalDevice,
) -> Result<Arc<ApprovalCollector>, Failure> {
    let refused = || Failure::Config {
        field: "owner approval device",
        fix: "the owner's approval device must form an approval host",
    };
    let fingerprint = hagency_core::canonical::digest(
        &serde_json::to_value(registration).map_err(|_| refused())?,
    )
    .map_err(|_| refused())?;
    let identity = HostIdentity {
        server_name: registration.server_name.clone(),
        registration_fingerprint: fingerprint,
        transport: MatrixTransportObservation {
            engagement_id: device.label.clone(),
            registration_generation: registration.generation,
            generation: 1,
            sender_mxid: device.device.user_id.clone(),
            device_id: device.device.device_id.clone(),
        },
    };
    let key: [u8; 32] = private::read_secret(&state.join(&device.key_file))
        .map_err(|_| refused())?
        .try_into()
        .map_err(|_| refused())?;
    let config = HostConfig::new(
        identity,
        &running.homeserver,
        &secret(state, &device.token_file)?,
        state.join(&device.sdk_root),
        key,
        vec![HostRoom {
            room_id: device.first_room.clone(),
            generation: 1,
            privacy: RoomPrivacy::Direct {
                human_mxid: owner.to_owned(),
            },
        }],
        running.matrix_limits.clone(),
    )
    .and_then(|config| match &running.matrix_root {
        Some(pem) => config.with_root_pem(pem),
        None => Ok(config),
    })
    .map_err(|_| refused())?;
    let approval = HostApprovalConfig::for_fleet(
        config,
        FleetApprovalAnchor {
            fleet_id: registration.fleet_id.clone(),
            server_name: registration.server_name.clone(),
            registration_generation: registration.generation,
            bot_mxid: registration.approval_bot_mxid.clone(),
        },
    )
    .and_then(|approval| {
        approval.with_fresh_account_enrollment(vec![(owner.to_owned(), anchor.to_owned())])
    })
    .map_err(|_| refused())?;
    Ok(Arc::new(
        ApprovalCollector::new(approval, domain.clone()).map_err(|_| refused())?,
    ))
}

/// One owner's approval pump, supervised (ADR-187 amendment). Enrollment is
/// retried until the owner's device is enrolled (it needs the owner's first
/// agent admitted). The owner's channel stays put for every agent that holds
/// it: the supervisor relays it into a fresh drain, so a drain that ends on a
/// refused card is restarted instead of stopping that owner's approvals.
async fn supervise_pump(
    pump: approval::Pump,
    mut receiver: mpsc::Receiver<hagency_execution::ApprovalRequests>,
    owner: String,
    cancel: CancellationToken,
) {
    let cap = Duration::from_secs(10);
    let mut backoff = BACKOFF_MIN;
    loop {
        match pump.initialize(&cancel).await {
            Ok(()) => break,
            Err(error) => {
                tracing::info!(%owner, ?error, "owner approval device not enrolled yet; retrying")
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(cap);
    }
    backoff = BACKOFF_MIN;
    loop {
        let (relay, handoffs) = mpsc::channel(16);
        let drain = pump.drain(handoffs, &cancel);
        tokio::pin!(drain);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                result = &mut drain => {
                    if let Err(error) = result {
                        tracing::warn!(%owner, ?error, "owner approval pump stopped; restarting");
                    }
                    break;
                }
                next = receiver.recv() => match next {
                    Some(requests) => {
                        if relay.send(requests).await.is_err() {
                            break;
                        }
                    }
                    None => return,
                },
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

#[cfg(test)]
mod close_tests {
    use super::*;

    #[tokio::test]
    async fn native_palpo_fleet_close_retains_cancelled_wait_and_propagates_drain_failure() {
        let (release, waiting) = tokio::sync::oneshot::channel::<()>();
        let mut service = FleetService {
            cancel: CancellationToken::new(),
            task: Some(tokio::spawn(async move {
                waiting.await.map_err(|_| Failure::OutcomeUnknown)?;
                Err(Failure::OutcomeUnknown)
            })),
            joined: None,
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(10), service.close())
                .await
                .is_err()
        );
        assert!(service.task.is_some());
        release.send(()).unwrap();
        assert_eq!(service.close().await, Err(Failure::OutcomeUnknown));
        assert_eq!(service.close().await, Err(Failure::OutcomeUnknown));
        assert!(service.task.is_none());
    }
}
