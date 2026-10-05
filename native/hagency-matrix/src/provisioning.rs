//! Original inline physical-account owner. Account observation is deliberately
//! not a full factory result: home/rooms/SDK/runtime and domain completion follow.
use crate::{
    CancellationToken, Error, HostConfig, Limits, ProvisionedTokenAccount, TokenAccountProvision,
    config::host_endpoint,
};
use hagency_core::{authority::Registration, canonical};
use hagency_store::{DomainStore, EffectOutcome};
use reqwest::Url;
use std::{
    collections::BTreeMap,
    path::{Component, PathBuf},
    sync::{Arc, Mutex},
};

const MAX_JOBS: usize = 16;
mod factory;
pub use factory::ProvisionedAgent;
/// What one provisioning pass did, per engagement (ADR-182: one
/// engagement's refusal never stops the others).
#[derive(Debug, Default)]
pub struct PassReport {
    pub started: Vec<String>,
    pub awaiting: Vec<String>,
    pub failed: Vec<(String, Error)>,
    pub retired: usize,
}
struct Job {
    factory: Option<Arc<factory::Custody>>,
    home: Mutex<Option<Arc<hagency_store::agent_home::ManagedAgentHome>>>,
    account: Mutex<Option<Arc<ProvisionedTokenAccount>>>,
    result: Mutex<Option<Result<Arc<ProvisionedTokenAccount>, Error>>>,
    /// The claimed effect, kept so a wait for the owner resumes from the rooms.
    effect: Mutex<Option<hagency_store::Effect>>,
    /// Wall-clock milliseconds since this provision first waited for its owner.
    awaiting_since: Mutex<Option<u64>>,
}
fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
/// Private process Host capability. No Deserialize/Debug/Clone, credential
/// getter, generic adapter callback or public observed-result setter.
pub struct TokenProvisioningHost {
    factory_rooms: Arc<tokio::sync::Mutex<()>>,
    closed: std::sync::atomic::AtomicBool,
    warm: Option<hagency_execution::WarmHostPlan>,
    factory_approvals: ApprovalLink,
    as_namespace: Option<String>,
    homes: Option<hagency_store::agent_home::ManagedHomePlan>,
    rooms: Option<RoomPlan>,
    registration: Registration,
    fingerprint: String,
    endpoint: Url,
    token: String,
    state: PathBuf,
    key: [u8; 32],
    limits: Limits,
    roots: Vec<reqwest::Certificate>,
    jobs: Mutex<BTreeMap<String, Arc<Job>>>,
}
struct RoomPlan {
    representative: String,
    anchors: AnchorSource,
}
/// The approval collector a warm agent's approvals ride on.
enum ApprovalLink {
    /// A coordinator install: the one configured approval bot.
    Fixed(Option<Arc<crate::ApprovalCollector>>),
    /// An imported fleet (ADR-187 amendment): one approval-bot device per
    /// owner, attached by the fleet service before that owner's provision.
    PerOwner(Mutex<BTreeMap<String, Arc<crate::ApprovalCollector>>>),
}
/// Where an agent's owner anchor comes from (ADR-187 §C).
enum AnchorSource {
    /// A coordinator install's configured list (ADR-102 as written).
    Static(Vec<(String, String)>),
    /// An imported fleet: the owner's anchor pinned in the store on first use.
    Pinned,
}
/// Why the anchors are needed: a first enrollment waits for an unpinned or
/// disputed owner key; a re-attach keeps the pinned key and lets the
/// enrollment's own key check catch a change.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnchorUse {
    Enroll,
    Reattach,
}
impl TokenProvisioningHost {
    pub fn new(
        registration: Registration,
        endpoint: &str,
        token: &str,
        state: PathBuf,
        key: [u8; 32],
        limits: Limits,
    ) -> Result<Self, Error> {
        Self::configured(registration, endpoint, token, state, key, limits, None)
    }
    pub fn application_service(
        registration: Registration,
        endpoint: &str,
        credential: crate::ApplicationServiceCredential,
        state: PathBuf,
        key: [u8; 32],
        limits: Limits,
    ) -> Result<Self, Error> {
        Self::configured(
            registration,
            endpoint,
            &credential.token,
            state,
            key,
            limits,
            Some(credential.namespace),
        )
    }
    fn configured(
        registration: Registration,
        endpoint: &str,
        token: &str,
        state: PathBuf,
        key: [u8; 32],
        limits: Limits,
        as_namespace: Option<String>,
    ) -> Result<Self, Error> {
        registration.validate().map_err(|_| Error::Config)?;
        limits.validate()?;
        if if let Some(namespace) = &as_namespace {
            namespace != &format!("{}_", registration.fleet_id)
                || !(16..=4096).contains(&token.len())
                || !token.bytes().all(|b| (33..=126).contains(&b))
        } else {
            token.is_empty()
                || token.len() > 64
                || !token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'=' | b'_' | b'-'))
        } || !state.is_absolute()
            || state.as_os_str().as_encoded_bytes().len() > 4096
            || state
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(Error::Config);
        }
        let fingerprint =
            canonical::digest(&serde_json::to_value(&registration).map_err(|_| Error::Config)?)
                .map_err(|_| Error::Config)?;
        Ok(Self {
            factory_rooms: Arc::new(tokio::sync::Mutex::new(())),
            closed: std::sync::atomic::AtomicBool::new(false),
            warm: None,
            factory_approvals: ApprovalLink::Fixed(None),
            as_namespace,
            homes: None,
            rooms: None,
            registration,
            fingerprint,
            endpoint: host_endpoint(endpoint)?,
            token: token.to_owned(),
            state,
            key,
            limits,
            roots: vec![],
            jobs: Mutex::new(BTreeMap::new()),
        })
    }
    pub fn with_root_pem(mut self, pem: &[u8]) -> Result<Self, Error> {
        if pem.len() > 16384 || self.roots.len() >= 4 {
            return Err(Error::Config);
        }
        self.roots
            .push(reqwest::Certificate::from_pem(pem).map_err(|_| Error::Config)?);
        Ok(self)
    }
    /// Explicit private physical-rooms/SDK checkpoint, not the full factory.
    pub fn with_agent_rooms_enrollment(
        mut self,
        representative_token: &str,
        anchors: Vec<(String, String)>,
    ) -> Result<Self, Error> {
        if self.rooms.is_some()
            || !(16..=4096).contains(&representative_token.len())
            || !representative_token
                .bytes()
                .all(|b| (33..=126).contains(&b))
        {
            return Err(Error::Config);
        }
        // Validate peer material before the new account is known. Its actual
        // self-exclusion remains mandatory when that account config is built.
        crate::enrollment::state::Profile::new(
            anchors.clone(),
            "",
            &self.registration.server_name,
        )?;
        self.rooms = Some(RoomPlan {
            representative: representative_token.to_owned(),
            anchors: AnchorSource::Static(anchors),
        });
        Ok(self)
    }
    /// ADR-187 §C: an imported fleet's agent rooms, enrolled with each
    /// owner's anchor pinned on first use. The fleet service pins the owner's
    /// key before a pass; until it is pinned the provision waits.
    pub fn with_agent_rooms_pinned_anchors(
        mut self,
        representative_token: &str,
    ) -> Result<Self, Error> {
        if self.rooms.is_some()
            || !(16..=4096).contains(&representative_token.len())
            || !representative_token
                .bytes()
                .all(|b| (33..=126).contains(&b))
        {
            return Err(Error::Config);
        }
        self.rooms = Some(RoomPlan {
            representative: representative_token.to_owned(),
            anchors: AnchorSource::Pinned,
        });
        Ok(self)
    }
    /// The engagements this host's factory completed and a restart brings
    /// back, in id order (the fleet service's re-attach list). Read-only.
    pub async fn provisioned_engagements(
        &self,
        domain: &DomainStore,
    ) -> Result<Vec<String>, Error> {
        Ok(domain.inline_factory_engagements().await?)
    }
    /// ADR-187 §A.5: the fleet's membership sweep, acting with the
    /// representative's credential instead of a coordinator's.
    pub fn membership_sweep(&self, domain: DomainStore) -> Result<crate::MembershipSweep, Error> {
        let plan = self.rooms.as_ref().ok_or(Error::Config)?;
        let authorization =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", plan.representative))
                .map_err(|_| Error::Config)?;
        Ok(crate::MembershipSweep {
            http: crate::http::Http::for_host(
                &self.endpoint,
                Some(&authorization),
                &self.limits,
                &self.roots,
            )?,
            domain,
            registration: self.registration.clone(),
        })
    }
    /// The anchors this engagement's agent enrolls or re-attaches with.
    pub(crate) async fn anchors_for(
        &self,
        domain: &DomainStore,
        effect: &hagency_store::Effect,
        purpose: AnchorUse,
    ) -> Result<Vec<(String, String)>, Error> {
        let plan = self.rooms.as_ref().ok_or(Error::Config)?;
        match &plan.anchors {
            AnchorSource::Static(anchors) => Ok(anchors.clone()),
            AnchorSource::Pinned => {
                let owner = effect
                    .payload
                    .get("request")
                    .and_then(|r| r.get("ownerMxid"))
                    .and_then(serde_json::Value::as_str)
                    .ok_or(Error::Config)?
                    .to_owned();
                let pinned = domain
                    .owner_anchor(owner.clone())
                    .await
                    .map_err(|_| Error::Storage)?;
                match (pinned, purpose) {
                    (Some(anchor), AnchorUse::Reattach) => Ok(vec![(owner, anchor.master_key)]),
                    (Some(anchor), AnchorUse::Enroll) if anchor.mismatch_key.is_none() => {
                        Ok(vec![(owner, anchor.master_key)])
                    }
                    // No key pinned yet, or a disputed one: wait, without a
                    // deadline, exactly like an owner who has not joined.
                    (_, AnchorUse::Enroll) => Err(Error::AwaitingOwner),
                    (None, AnchorUse::Reattach) => Err(Error::Recipients),
                }
            }
        }
    }
    /// Original physical v1 homes, still not runtime fulfillment or Applied.
    pub fn with_managed_homes(
        mut self,
        plan: hagency_store::agent_home::ManagedHomePlan,
    ) -> Result<Self, Error> {
        if self.homes.is_some() || self.rooms.is_none() {
            return Err(Error::Config);
        }
        plan.separate_from(&self.state)?;
        self.homes = Some(plan);
        Ok(self)
    }
    pub(crate) fn bind(&self, config: &HostConfig) -> Result<(), Error> {
        if config.approval
            || config.endpoint != self.endpoint
            || config.identity.registration_fingerprint != self.fingerprint
            || config.identity.server_name != self.registration.server_name
            || config.identity.transport.registration_generation != self.registration.generation
            || config.reception_room.as_ref().is_none_or(|room| {
                room.room_id != self.registration.reception_room_id
                    || room.generation != self.registration.generation
            })
        {
            return Err(Error::Config);
        }
        Ok(())
    }
    pub(crate) fn room_coordinator(&self) -> Option<Arc<tokio::sync::Mutex<()>>> {
        self.warm.as_ref().map(|_| self.factory_rooms.clone())
    }
    /// Called only inside the Collector's accepted owned intake job, AFTER
    /// representative verification and canonical approve. No effect worker.
    /// After a restart: bring back one agent this service's inline factory
    /// already completed. Nothing is claimed, registered, created or activated;
    /// every step reads what the original provision left and refuses when it is
    /// missing or changed. A failure here concerns this agent only and leaves
    /// its durable state exactly as it was, except that genuine negative Matrix
    /// evidence still fences as everywhere else.
    pub async fn reattach_completed(
        &self,
        domain: &DomainStore,
        engagement: &str,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(Error::Generation);
        }
        if self.warm.is_none() || self.homes.is_none() || self.rooms.is_none() {
            return Err(Error::Config);
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let (effect, registration, scope) = domain
            .reattach_provision_scope(engagement.to_owned())
            .await?;
        if canonical::digest(&serde_json::to_value(&registration).map_err(|_| Error::Config)?)
            .map_err(|_| Error::Config)?
            != self.fingerprint
        {
            return Err(Error::Generation);
        }
        let job = {
            let mut jobs = self.jobs.lock().map_err(|_| Error::OutcomeUnknown)?;
            if jobs.contains_key(&effect.id) {
                return Err(Error::Busy);
            }
            if jobs.len() >= MAX_JOBS {
                return Err(Error::Capacity);
            }
            let job = Arc::new(Job {
                factory: Some(Arc::new(factory::Custody::new())),
                home: Mutex::new(None),
                account: Mutex::new(None),
                result: Mutex::new(None),
                effect: Mutex::new(Some(effect.clone())),
                awaiting_since: Mutex::new(None),
            });
            jobs.insert(effect.id.clone(), job.clone());
            job
        };
        let result = async {
            let home =
                self.homes
                    .as_ref()
                    .ok_or(Error::Config)?
                    .reopen(&scope, &effect, &registration)?;
            *job.home.lock().map_err(|_| Error::OutcomeUnknown)? = Some(home);
            let mut operation = if let Some(namespace) = &self.as_namespace {
                TokenAccountProvision::application_service(
                    &registration,
                    &effect,
                    self.endpoint.as_str(),
                    crate::ApplicationServiceCredential::new(&self.token, namespace)?,
                    self.state.clone(),
                    self.key,
                    self.limits.clone(),
                )
            } else {
                TokenAccountProvision::new(
                    &registration,
                    &effect,
                    self.endpoint.as_str(),
                    &self.token,
                    self.state.clone(),
                    self.key,
                    self.limits.clone(),
                )
            }?
            .for_reattach()
            .with_domain(domain.clone(), effect.clone(), registration.clone());
            operation.factory_rooms = Some(self.factory_rooms.clone());
            operation.roots = self.roots.clone();
            let account = Arc::new(operation.execute(cancel).await?);
            *job.account.lock().map_err(|_| Error::OutcomeUnknown)? = Some(account.clone());
            self.reattach_factory(domain, &effect, scope, &account, &job, cancel)
                .await?;
            Ok(account)
        }
        .await;
        if result.is_err()
            && let Some(custody) = &job.factory
        {
            custody.cancel().await;
        }
        let response = result.as_ref().map(|_| ()).map_err(Clone::clone);
        *job.result.lock().map_err(|_| Error::OutcomeUnknown)? = Some(result);
        response
    }
    /// Rooms, SDK enrollment and the factory, once the account was observed.
    /// This is the part a wait for the owner resumes.
    async fn continue_provision(
        &self,
        domain: &DomainStore,
        effect: &hagency_store::Effect,
        account: &Arc<ProvisionedTokenAccount>,
        job: &Arc<Job>,
        cancel: &CancellationToken,
        activated: &mut bool,
    ) -> Result<(), Error> {
        if let Some(plan) = &self.rooms {
            // The owner's anchor first: no room is created while it is not
            // pinned (ADR-187 §C.3).
            let anchors = self.anchors_for(domain, effect, AnchorUse::Enroll).await?;
            account
                .create_agent_rooms(&plan.representative, cancel)
                .await?;
            account
                .enroll_created_rooms(1, self.key, anchors, cancel)
                .await?;
            // ADR-184: the agent's keys are published; only now may the owner
            // arrive and write to it.
            account.invite_owner(&plan.representative, cancel).await?;
        }
        if self.warm.is_some() {
            self.finish_factory(domain, effect, account, job, cancel, activated)
                .await?;
        }
        Ok(())
    }
    /// The custody after an attempt: a fenced factory when it failed after
    /// activation, a cancelled one before, and the effect observed unknown.
    /// Waiting for the owner is not a failure: nothing is fenced or observed,
    /// the effect stays Started, and a later turn resumes from the rooms.
    async fn settle_provision(
        &self,
        domain: &DomainStore,
        effect: &hagency_store::Effect,
        job: &Arc<Job>,
        activated: bool,
        result: Result<Arc<ProvisionedTokenAccount>, Error>,
    ) -> Result<Arc<ProvisionedTokenAccount>, Error> {
        if matches!(result, Err(Error::AwaitingOwner)) {
            let mut since = job
                .awaiting_since
                .lock()
                .map_err(|_| Error::OutcomeUnknown)?;
            if since.is_none() {
                *since = Some(wall_ms());
            }
            return Err(Error::AwaitingOwner);
        }
        if let Err(error) = &result {
            // Never silent: the effect is only recorded as unknown, so the
            // reason exists nowhere else.
            eprintln!(
                "provision attempt failed: effect={} activated={activated} error={error:?}",
                effect.id
            );
        }
        let mut result = result;
        if activated && let Err(error) = result {
            result = Err(match &job.factory {
                Some(custody) => custody.fence_failure(error).await,
                None => error,
            });
        }
        if result.is_err()
            && let Some(custody) = &job.factory
        {
            custody.cancel().await;
        }
        if result.is_err()
            && !activated
            && domain
                .observe_effect(effect.id.clone(), effect.fence, EffectOutcome::Unknown)
                .await
                .is_err()
        {
            Err(Error::OutcomeUnknown)
        } else {
            result
        }
    }
    /// One more look for the owner, then the rest of the provision if they
    /// joined. Called on every coordinator turn for each waiting provision.
    async fn resume_provision(
        &self,
        domain: &DomainStore,
        job: &Arc<Job>,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let effect = job
            .effect
            .lock()
            .map_err(|_| Error::OutcomeUnknown)?
            .clone()
            .ok_or(Error::OutcomeUnknown)?;
        let account = job
            .account
            .lock()
            .map_err(|_| Error::OutcomeUnknown)?
            .clone()
            .ok_or(Error::OutcomeUnknown)?;
        let mut activated = false;
        let result = self
            .continue_provision(domain, &effect, &account, job, cancel, &mut activated)
            .await
            .map(|()| account.clone());
        let result = self
            .settle_provision(domain, &effect, job, activated, result)
            .await;
        let response = result.as_ref().map(|_| ()).map_err(Clone::clone);
        if response.is_ok() {
            *job.awaiting_since
                .lock()
                .map_err(|_| Error::OutcomeUnknown)? = None;
        }
        *job.result.lock().map_err(|_| Error::OutcomeUnknown)? = Some(result);
        response
    }
    /// The provisions waiting for their owner, with the wall-clock millisecond
    /// each started waiting. Read-only; for the fleet's status.
    pub fn awaiting_owner_engagements(&self) -> Vec<(String, u64)> {
        let Ok(jobs) = self.jobs.lock() else {
            return Vec::new();
        };
        jobs.iter()
            .filter_map(|(id, job)| {
                let waiting = matches!(
                    job.result.lock().ok()?.as_ref(),
                    Some(Err(Error::AwaitingOwner))
                );
                let since = (*job.awaiting_since.lock().ok()?)?;
                (waiting && id.starts_with("provision_"))
                    .then(|| (id["provision_".len()..].to_owned(), since))
            })
            .collect()
    }
    /// ADR-187 §A.2 / ADR-182: one provisioning pass, every engagement on its
    /// own. Provisions waiting for their owner get one look, console-approved
    /// provisions start, and never-attached retirements settle. A refusal is
    /// that engagement's alone: it is collected in the report and the pass
    /// goes on to the next one. Only a store read the pass itself needs fails
    /// the pass.
    pub async fn provision_pass(
        &self,
        domain: &DomainStore,
        cancel: &CancellationToken,
    ) -> Result<PassReport, Error> {
        let mut report = PassReport::default();
        let waiting: Vec<String> = self
            .awaiting_owner_engagements()
            .into_iter()
            .map(|(engagement, _)| engagement)
            .collect();
        let pending = domain
            .pending_provisions(self.registration.fleet_id.clone())
            .await
            .map_err(|_| Error::Storage)?;
        for engagement in waiting.into_iter().chain(pending) {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if report.started.contains(&engagement)
                || report.awaiting.contains(&engagement)
                || report.failed.iter().any(|(e, _)| e == &engagement)
            {
                continue;
            }
            match self
                .account(domain, &self.registration, &engagement, cancel)
                .await
            {
                Ok(()) => report.started.push(engagement),
                Err(Error::AwaitingOwner) => report.awaiting.push(engagement),
                Err(error) => report.failed.push((engagement, error)),
            }
        }
        report.retired = self.settle_unattached_retirements(domain).await?;
        Ok(report)
    }
    /// Settle the retirement of every revoked engagement whose agent never got a
    /// credential. The `retire` effect is otherwise run only by the agent's own
    /// live worker, and an agent whose provisioning never stored an account has
    /// none, so its cleanup would stay pending forever. TS
    /// `lib/matrix-work-executor.js` answers a logout with no stored credential
    /// as already done; the same holds here. An agent whose credential
    /// directory exists is left to its worker: it may hold rooms to leave.
    pub(crate) async fn settle_unattached_retirements(
        &self,
        domain: &DomainStore,
    ) -> Result<usize, Error> {
        // The independently supervised Palpo profile verifies full identity
        // removal, including an account created before local credential save.
        if self.as_namespace.is_some() {
            return Ok(0);
        }
        let pending = domain
            .pending_unattached_retirements(self.registration.fleet_id.clone())
            .await
            .map_err(|_| Error::Storage)?;
        let mut settled = 0;
        for engagement in pending {
            let credential = self
                .state
                .join(format!("agent-matrix-provision_{engagement}"));
            if std::fs::symlink_metadata(&credential).is_ok() {
                continue;
            }
            let Some(effect) = domain
                .claim_effect_for(format!("retire_{engagement}"))
                .await
                .map_err(|_| Error::Storage)?
            else {
                continue;
            };
            domain
                .observe_effect(
                    effect.id,
                    effect.fence,
                    EffectOutcome::Applied {
                        receipt: r#"{"credential":"none","logout":"not_required"}"#.into(),
                    },
                )
                .await?;
            settled += 1;
        }
        Ok(settled)
    }
    pub(crate) async fn account(
        &self,
        domain: &DomainStore,
        registration: &Registration,
        engagement: &str,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(Error::Generation);
        }
        if canonical::digest(&serde_json::to_value(registration).map_err(|_| Error::Config)?)
            .map_err(|_| Error::Config)?
            != self.fingerprint
        {
            return Err(Error::Generation);
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let id = format!("provision_{engagement}");
        // Historical step acknowledgement only; it cannot establish current
        // SDK/room/runner authority for the remaining factory. A provision
        // waiting for its owner is the one exception: it resumes from the
        // rooms, with the account it already observed. The registry lock is
        // released before anything awaits.
        let resume = {
            let jobs = self.jobs.lock().map_err(|_| Error::OutcomeUnknown)?;
            match jobs.get(&id) {
                None => None,
                Some(job) => match job
                    .result
                    .lock()
                    .map_err(|_| Error::OutcomeUnknown)?
                    .as_ref()
                {
                    Some(Ok(_)) => return Ok(()),
                    Some(Err(Error::AwaitingOwner)) => Some(job.clone()),
                    Some(Err(error)) => return Err(error.clone()),
                    None => return Err(Error::OutcomeUnknown),
                },
            }
        };
        if let Some(job) = resume {
            return self.resume_provision(domain, &job, cancel).await;
        }
        // ADR-187: an imported fleet's warm agent needs its owner's
        // approval-bot device. Until the fleet service attached it the
        // provision waits, before any claim: a refusal after the claim would
        // be cached for the life of the process.
        if self.warm.is_some() && matches!(self.factory_approvals, ApprovalLink::PerOwner(_)) {
            let owner = domain
                .engagement_owner(engagement.to_owned())
                .await
                .map_err(|_| Error::Storage)?
                .ok_or(Error::Config)?;
            if !self.approvals_ready_for(&owner) {
                return Err(Error::AwaitingOwner);
            }
        }
        let job = {
            let mut jobs = self.jobs.lock().map_err(|_| Error::OutcomeUnknown)?;
            if jobs.len() >= MAX_JOBS {
                return Err(Error::Capacity);
            }
            let job = Arc::new(Job {
                factory: self
                    .warm
                    .as_ref()
                    .map(|_| Arc::new(factory::Custody::new())),
                home: Mutex::new(None),
                account: Mutex::new(None),
                result: Mutex::new(None),
                effect: Mutex::new(None),
                awaiting_since: Mutex::new(None),
            });
            jobs.insert(id.clone(), job.clone());
            job
        };
        // Burn the admitted local claim attempt even when its acknowledgement
        // is lost. A restarted Host also cannot claim Started/Uncertain again.
        let claimed = domain.claim_effect_for(id).await;
        let result = match claimed {
            Ok(Some(effect)) => {
                *job.effect.lock().map_err(|_| Error::OutcomeUnknown)? = Some(effect.clone());
                let mut activated = false;
                let result = async {
                    domain
                        .validate_provision_account(effect.clone(), registration.clone())
                        .await?;
                    if let Some(plan) = &self.homes {
                        let cancelled =
                            Arc::new(std::sync::atomic::AtomicBool::new(cancel.is_cancelled()));
                        let home = plan.materialize(
                            domain.clone(),
                            effect.clone(),
                            registration.clone(),
                            std::time::Instant::now() + self.limits.sdk,
                            cancelled.clone(),
                        );
                        tokio::pin!(home);
                        let home = tokio::select! {
                            result = &mut home => result,
                            _ = cancel.cancelled() => {
                                cancelled.store(true, std::sync::atomic::Ordering::Release);
                                home.await
                            },
                        };
                        *job.home.lock().map_err(|_| Error::OutcomeUnknown)? = Some(home?);
                        domain
                            .validate_provision_account(effect.clone(), registration.clone())
                            .await?;
                    }
                    let mut operation = if let Some(namespace) = &self.as_namespace {
                        TokenAccountProvision::application_service(
                            registration,
                            &effect,
                            self.endpoint.as_str(),
                            crate::ApplicationServiceCredential::new(&self.token, namespace)?,
                            self.state.clone(),
                            self.key,
                            self.limits.clone(),
                        )
                    } else {
                        TokenAccountProvision::new(
                            registration,
                            &effect,
                            self.endpoint.as_str(),
                            &self.token,
                            self.state.clone(),
                            self.key,
                            self.limits.clone(),
                        )
                    }?
                    .with_domain(
                        domain.clone(),
                        effect.clone(),
                        registration.clone(),
                    );
                    if self.warm.is_some() {
                        operation.factory_rooms = Some(self.factory_rooms.clone());
                    }
                    operation.roots = self.roots.clone();
                    let account = Arc::new(operation.execute(cancel).await?);
                    *job.account.lock().map_err(|_| Error::OutcomeUnknown)? = Some(account.clone());
                    self.continue_provision(
                        domain,
                        &effect,
                        &account,
                        &job,
                        cancel,
                        &mut activated,
                    )
                    .await?;
                    Ok(account)
                }
                .await;
                self.settle_provision(domain, &effect, &job, activated, result)
                    .await
            }
            Ok(None) => Err(Error::OutcomeUnknown),
            Err(error) => Err(error.into()),
        };
        let response = result.as_ref().map(|_| ()).map_err(Clone::clone);
        // Keep the opaque observed account for the remaining physical stages.
        // Checkpoint-only jobs do not submit Applied; concrete factory jobs
        // retain the original runtime/SDK and their acknowledged derived route.
        *job.result.lock().map_err(|_| Error::OutcomeUnknown)? = Some(result);
        response
    }
    #[cfg(test)]
    pub(crate) fn application_service_profile(&self) -> bool {
        self.as_namespace.is_some()
    }
    #[cfg(test)]
    pub(crate) fn observed_home_handle(
        &self,
        engagement: &str,
    ) -> Option<Arc<hagency_store::agent_home::ManagedAgentHome>> {
        let jobs = self.jobs.lock().unwrap();
        jobs.get(&format!("provision_{engagement}"))?
            .home
            .lock()
            .unwrap()
            .as_ref()
            .cloned()
    }
    #[cfg(test)]
    pub(crate) fn observed_account(&self, engagement: &str) -> Option<(String, String)> {
        let jobs = self.jobs.lock().unwrap();
        let result = jobs
            .get(&format!("provision_{engagement}"))?
            .result
            .lock()
            .unwrap();
        let account = result.as_ref()?.as_ref().ok()?;
        Some((
            account.sender_mxid().to_owned(),
            account.device_id().to_owned(),
        ))
    }
    #[cfg(test)]
    pub(crate) fn observed_account_handle(&self, engagement: &str) -> Arc<ProvisionedTokenAccount> {
        let jobs = self.jobs.lock().unwrap();
        jobs.get(&format!("provision_{engagement}"))
            .unwrap()
            .account
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone()
    }
}
