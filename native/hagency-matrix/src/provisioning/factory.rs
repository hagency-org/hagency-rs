//! Consume the original physical job, SDK and initialized native owner inline.
use super::*;
use crate::Collector;
use hagency_core::{
    project::EngagementState,
    replies::RoomPrivacy,
    tasks::{RunnerCapability, SessionBinding},
};
use hagency_execution::{FactoryRuntime, Failure, Operation, WarmHostPlan};
use hagency_store::{Effect, OwnedClaimProfile, OwnedClaimRoom};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct Custody {
    runtime: tokio::sync::Mutex<Option<FactoryRuntime>>,
    collector: Mutex<Option<Collector>>,
    binding: Mutex<Option<SessionBinding>>,
    taken: AtomicBool,
    closed: AtomicBool,
    closure: Mutex<Closure>,
}
enum Closure {
    Waiting,
    Running,
    Complete(Result<(), Error>),
}
impl Custody {
    pub(super) fn new() -> Self {
        Self {
            runtime: tokio::sync::Mutex::new(None),
            collector: Mutex::new(None),
            binding: Mutex::new(None),
            taken: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            closure: Mutex::new(Closure::Waiting),
        }
    }
    async fn ready(&self, cancel: &CancellationToken) -> Result<(), Error> {
        let mut runtime = self.runtime.lock().await;
        let owner = runtime.as_mut().ok_or(Error::OutcomeUnknown)?;
        let result = tokio::select! {result=owner.ready()=>result.map_err(|error| {
            eprintln!("agent factory runtime readiness refused: {error:?}");
            Error::OutcomeUnknown
        }),_ = cancel.cancelled()=>Err(Error::Cancelled)};
        if result.is_err() {
            owner.cancel();
        }
        result
    }
    pub(super) async fn cancel(&self) {
        if let Some(runtime) = self.runtime.lock().await.as_ref() {
            runtime.cancel();
        }
    }
    async fn activate(
        &self,
        cancel: &CancellationToken,
    ) -> Result<hagency_core::project::Engagement, Error> {
        let mut runtime = self.runtime.lock().await;
        let owner = runtime.as_mut().ok_or(Error::OutcomeUnknown)?;
        let result = tokio::select! {result=owner.activate()=>result.map_err(|_|Error::OutcomeUnknown),_ = cancel.cancelled()=>Err(Error::Cancelled)};
        if result.is_err() {
            owner.cancel();
        }
        result
    }
    pub(super) async fn fence_failure(&self, error: Error) -> Error {
        let collector = match self.collector.lock() {
            Ok(owner) => owner.as_ref().map(|c| Collector {
                inner: c.inner.clone(),
            }),
            Err(_) => return Error::OutcomeUnknown,
        };
        let Some(collector) = collector else {
            return error;
        };
        let inner = &collector.inner;
        match inner
            .domain
            .matrix_transport_state(inner.config.identity.transport.engagement_id.clone())
            .await
        {
            Ok(Some(state))
                if state.available && state.observation == inner.config.identity.transport =>
            {
                match inner
                    .fence_observation::<()>(state.observation, error)
                    .await
                {
                    Err(error) => error,
                    Ok(()) => Error::OutcomeUnknown,
                }
            }
            Ok(_) => error,
            Err(_) => Error::OutcomeUnknown,
        }
    }
    pub(super) async fn close(self: &Arc<Self>) -> Result<(), Error> {
        {
            let closure = self.closure.lock().map_err(|_| Error::OutcomeUnknown)?;
            match &*closure {
                Closure::Waiting => {}
                Closure::Running => return Err(Error::Busy),
                Closure::Complete(result) => return result.clone(),
            }
        }
        let collector = self
            .collector
            .lock()
            .map_err(|_| Error::OutcomeUnknown)?
            .as_ref()
            .map(|c| Collector {
                inner: c.inner.clone(),
            });
        // Busy is known pre-effect refusal. Once admitted, the exact SDK busy
        // permit stays with the original join/close job through caller loss.
        let permit = collector
            .as_ref()
            .map(|c| {
                c.inner
                    .busy
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| Error::Busy)
            })
            .transpose()?;
        {
            let mut closure = self.closure.lock().map_err(|_| Error::OutcomeUnknown)?;
            match &*closure {
                Closure::Waiting => {}
                Closure::Running => return Err(Error::Busy),
                Closure::Complete(result) => return result.clone(),
            }
            *closure = Closure::Running;
        }
        self.closed.store(true, Ordering::Release);
        let original = self.clone();
        // Retained owned cleanup survives caller loss. A blocking join is not
        // a whole-tree cleanup observation or permission to free dirty leases.
        tokio::spawn(async move {
            let result = async {
                let runtime = original.runtime.lock().await.take();
                tokio::task::spawn_blocking(move || {
                    if let Some(runtime) = runtime {
                        runtime.cancel();
                        drop(runtime);
                    }
                })
                .await
                .map_err(|_| Error::OutcomeUnknown)?;
                if let (Some(collector), Some(permit)) = (collector, permit) {
                    collector.close_with_permit(permit).await?;
                }
                Ok(())
            }
            .await;
            *original.closure.lock().map_err(|_| Error::OutcomeUnknown)? =
                Closure::Complete(result.clone());
            result
        })
        .await
        .map_err(|_| Error::OutcomeUnknown)?
    }
}
/// One non-cloneable sequential-dispatch owner. Collector references retain the very
/// same enrolled SDK; no credential, replacement config or ready setter escapes.
pub struct ProvisionedAgent {
    custody: Arc<Custody>,
    collector: Arc<Collector>,
    binding: SessionBinding,
    workspace: String,
    appservice_identity: bool,
}
impl ProvisionedAgent {
    /// Whole-identity cleanup belongs to the profile's authenticated Palpo
    /// worker. A device logout cannot settle an appservice identity.
    pub fn requires_identity_retirement(&self) -> bool {
        self.appservice_identity
    }
    pub fn collector(&self) -> &Collector {
        &self.collector
    }
    /// Share the same enrolled queue/owner with this agent's native file
    /// services. This does not construct a new SDK or export configuration.
    pub fn shared_collector(&self) -> Arc<Collector> {
        self.collector.clone()
    }
    pub fn session(&self) -> &SessionBinding {
        &self.binding
    }
    pub fn workspace_id(&self) -> &str {
        &self.workspace
    }
    /// Fresh host scheduling metadata after this original collector refreshed.
    /// Old sessions remain retired; no runtime-facing request can supply a room.
    pub async fn inboxes(
        &self,
        profile: OwnedClaimProfile,
    ) -> Result<
        (
            OwnedClaimProfile,
            Vec<hagency_core::agent_inbox::AgentInboxPlan>,
        ),
        Error,
    > {
        use hagency_core::agent_inbox::AgentInboxPlan;
        if self.custody.closed.load(Ordering::Acquire) {
            return Err(Error::Generation);
        }
        let inner = &self.collector.inner;
        let transport = inner.expected_transport().await?;
        let mut inboxes = vec![AgentInboxPlan {
            session_id: self.binding.id.clone(),
            workspace_id: self.workspace.clone(),
        }];
        let mut rooms = Vec::new();
        for room in &inner.config.rooms {
            let generation = inner.observed_room_generation(room).await?;
            let selected =
                OwnedClaimRoom::new(room.room_id.clone(), generation, room.privacy.clone())?;
            rooms.push(if matches!(room.privacy, RoomPrivacy::Group {}) {
                selected.with_plaintext_project()?
            } else {
                selected
            });
            if !matches!(room.privacy, RoomPrivacy::Group {}) {
                continue;
            }
            if inner.config.factory_rooms.is_none() {
                return Err(Error::Config);
            }
            let binding = SessionBinding {
                id: format!(
                    "project_{}_{}_{}",
                    transport.engagement_id, transport.generation, generation
                ),
                engagement_id: transport.engagement_id.clone(),
                room_id: room.room_id.clone(),
                thread_root: None,
            };
            let resolved = inner
                .domain
                .resolve_verified_matrix_session(binding.clone())
                .await?;
            if resolved.id != binding.id
                || resolved.engagement_id != binding.engagement_id
                || resolved.room_id != binding.room_id
                || resolved.thread_root != binding.thread_root
            {
                return Err(Error::Conflict);
            }
            let route = inner.domain.matrix_intake_route(binding.id.clone()).await?;
            if route.room_generation != generation
                || route.transport_generation != transport.generation
                || route.privacy != room.privacy
            {
                return Err(Error::Generation);
            }
            inboxes.push(AgentInboxPlan {
                session_id: binding.id,
                workspace_id: self.workspace.clone(),
            });
        }
        self.joined_rooms(&transport, &mut rooms, &mut inboxes)
            .await?;
        let profile = profile.refresh_matrix_rooms(transport, rooms)?;
        Ok((profile, inboxes))
    }
    /// ADR-188: the rooms this agent joined by invitation, read from the
    /// store on every pass. Each is observed on its own; a room that cannot
    /// be observed or is not safe to work in is skipped this pass, never
    /// fatal to the agent's identity rooms.
    async fn joined_rooms(
        &self,
        transport: &hagency_core::replies::MatrixTransportObservation,
        rooms: &mut Vec<OwnedClaimRoom>,
        inboxes: &mut Vec<hagency_core::agent_inbox::AgentInboxPlan>,
    ) -> Result<(), Error> {
        use hagency_store::JoinedRoomState;
        let inner = &self.collector.inner;
        let engagement = transport.engagement_id.clone();
        let joined = inner.domain.joined_rooms(engagement.clone()).await?;
        // The store is the truth: forget rooms it no longer lists.
        inner
            .joined
            .lock()
            .unwrap()
            .retain(|room, _| joined.iter().any(|j| &j.room_id == room));
        if joined.is_empty() {
            return Ok(());
        }
        if inner.config.factory_rooms.is_none() {
            return Err(Error::Config);
        }
        let owner = inner.config.rooms.iter().find_map(|r| match &r.privacy {
            RoomPrivacy::Direct { human_mxid } => Some(human_mxid.clone()),
            RoomPrivacy::Group {} => None,
        });
        let Some(owner) = owner else {
            return Ok(());
        };
        let cancel = crate::CancellationToken::new();
        // ADR-188 §5: a room the agent is no longer in (it left, was kicked,
        // or the room closed) retires. A failed read retires nothing.
        let member_of: Option<Vec<String>> = match inner
            .http
            .request(&["_matrix", "client", "v3", "joined_rooms"], None, &cancel)
            .await
            .and_then(|response| response.success())
        {
            Ok(value) => value["joined_rooms"].as_array().map(|rooms| {
                rooms
                    .iter()
                    .filter_map(|r| r.as_str().map(str::to_owned))
                    .collect()
            }),
            Err(_) => None,
        };
        let mut live = Vec::with_capacity(joined.len());
        for room in joined {
            if member_of
                .as_ref()
                .is_some_and(|rooms| !rooms.contains(&room.room_id))
            {
                inner
                    .domain
                    .set_joined_room_state(
                        engagement.clone(),
                        room.room_id.clone(),
                        JoinedRoomState::Retired,
                        wall_ms(),
                    )
                    .await?;
                inner.joined.lock().unwrap().remove(&room.room_id);
                continue;
            }
            live.push(room);
        }
        for room in live {
            // An identity room is never also a joined room.
            if inner.config.rooms.iter().any(|r| r.room_id == room.room_id) {
                continue;
            }
            let target = crate::HostRoom {
                room_id: room.room_id.clone(),
                generation: 1,
                privacy: RoomPrivacy::Group {},
            };
            inner
                .joined
                .lock()
                .unwrap()
                .entry(room.room_id.clone())
                .or_insert(false);
            // A room already found encrypted and shared is only re-read, not
            // published: the store admits working joined rooms only.
            if room.state == JoinedRoomState::EncryptedShared {
                inner
                    .joined_shared
                    .lock()
                    .unwrap()
                    .insert(room.room_id.clone());
            } else {
                inner.joined_shared.lock().unwrap().remove(&room.room_id);
            }
            let observation = match inner.collect_room_observation(&target, &cancel).await {
                Ok(observation) => observation,
                Err(error) => {
                    eprintln!(
                        "joined room {} of {engagement} not observed this pass: {error:?}",
                        room.room_id
                    );
                    inner
                        .joined
                        .lock()
                        .unwrap()
                        .insert(room.room_id.clone(), false);
                    continue;
                }
            };
            let owner_only = observation.joined.len() == 2
                && observation.joined.contains(&owner)
                && observation.joined.contains(&transport.sender_mxid);
            let state = if observation.encrypted && !owner_only {
                JoinedRoomState::EncryptedShared
            } else {
                JoinedRoomState::Working
            };
            let now = wall_ms();
            if room.state != state {
                inner
                    .domain
                    .set_joined_room_state(engagement.clone(), room.room_id.clone(), state, now)
                    .await?;
            }
            if state == JoinedRoomState::EncryptedShared {
                inner
                    .joined
                    .lock()
                    .unwrap()
                    .insert(room.room_id.clone(), false);
                if inner
                    .domain
                    .claim_joined_room_notice(engagement.clone(), room.room_id.clone(), now)
                    .await?
                {
                    self.encrypted_shared_notice(&room.room_id, now, &cancel)
                        .await;
                } else if let Some(last) = room.notice_at
                    && now >= last.saturating_add(RENOTICE_GAP_MS)
                    && self
                        .latest_other_message(&room.room_id, &transport.sender_mxid, &cancel)
                        .await
                        .is_some_and(|at| at > last)
                    && inner
                        .domain
                        .claim_joined_room_renotice(
                            engagement.clone(),
                            room.room_id.clone(),
                            now,
                            now - RENOTICE_GAP_MS,
                        )
                        .await?
                {
                    // Someone posted since the last notice: remind them, at
                    // most once per gap. Only senders and times are read;
                    // the room's messages are never decrypted.
                    self.encrypted_shared_notice(&room.room_id, now, &cancel)
                        .await;
                }
                continue;
            }
            // A room the store will not route (its owner left, it went
            // unsafe) is skipped this pass; the identity rooms carry on.
            match self.joined_session(transport, &target).await {
                Ok((selected, inbox)) => {
                    rooms.push(selected);
                    inner
                        .joined
                        .lock()
                        .unwrap()
                        .insert(room.room_id.clone(), true);
                    inboxes.push(inbox);
                }
                Err(error) => {
                    eprintln!(
                        "joined room {} of {engagement} not routable this pass: {error:?}",
                        room.room_id
                    );
                    inner
                        .joined
                        .lock()
                        .unwrap()
                        .insert(room.room_id.clone(), false);
                }
            }
        }
        Ok(())
    }
    /// The session and claim selection for one working joined room.
    async fn joined_session(
        &self,
        transport: &hagency_core::replies::MatrixTransportObservation,
        target: &crate::HostRoom,
    ) -> Result<(OwnedClaimRoom, hagency_core::agent_inbox::AgentInboxPlan), Error> {
        let inner = &self.collector.inner;
        let engagement = transport.engagement_id.clone();
        let generation = inner.observed_room_generation(target).await?;
        let binding = SessionBinding {
            id: joined_session_id(
                &engagement,
                transport.generation,
                generation,
                &target.room_id,
            ),
            engagement_id: engagement.clone(),
            room_id: target.room_id.clone(),
            thread_root: None,
        };
        let resolved = inner
            .domain
            .resolve_verified_matrix_session(binding.clone())
            .await?;
        if resolved.id != binding.id
            || resolved.engagement_id != binding.engagement_id
            || resolved.room_id != binding.room_id
            || resolved.thread_root != binding.thread_root
        {
            return Err(Error::Conflict);
        }
        let route = inner.domain.matrix_intake_route(binding.id.clone()).await?;
        if route.room_generation != generation
            || route.transport_generation != transport.generation
            || !matches!(route.privacy, RoomPrivacy::Group {})
        {
            return Err(Error::Generation);
        }
        Ok((
            OwnedClaimRoom::new(target.room_id.clone(), generation, RoomPrivacy::Group {})?
                .joined_group()?,
            hagency_core::agent_inbox::AgentInboxPlan {
                session_id: binding.id,
                workspace_id: self.workspace.clone(),
            },
        ))
    }
    /// The newest message time from anyone but the agent, read from the
    /// room's recent events without decrypting them. None when unreadable.
    async fn latest_other_message(
        &self,
        room: &str,
        agent: &str,
        cancel: &crate::CancellationToken,
    ) -> Option<u64> {
        let value = self
            .collector
            .inner
            .http
            .request(
                &["_matrix", "client", "v3", "rooms", room, "messages"],
                Some(&[("dir", "b"), ("limit", "20")]),
                cancel,
            )
            .await
            .and_then(|response| response.success())
            .ok()?;
        value["chunk"]
            .as_array()?
            .iter()
            .filter(|e| {
                matches!(
                    e["type"].as_str(),
                    Some("m.room.encrypted" | "m.room.message")
                ) && e["sender"].as_str().is_some_and(|s| s != agent)
            })
            .filter_map(|e| e["origin_server_ts"].as_u64())
            .max()
    }
    /// ADR-188 §3: the one plain notice in an encrypted room with other
    /// people in it. Its replies could be read only by the owner, so the
    /// agent says so instead of working there. Best effort: a failed send is
    /// logged, and the notice is not repeated.
    async fn encrypted_shared_notice(
        &self,
        room: &str,
        now: u64,
        cancel: &crate::CancellationToken,
    ) {
        let txn = format!("joined-notice-{now}");
        let body = serde_json::json!({
            "msgtype": "m.notice",
            "body": "I can't work in an encrypted room with other people in it: only my owner could read my replies. Encryption can't be turned off in a room, so for working with me create a new room with encryption off, or talk to me in a room with just my owner and me.",
        })
        .to_string();
        let segments = [
            "_matrix",
            "client",
            "v3",
            "rooms",
            room,
            "send",
            "m.room.message",
            txn.as_str(),
        ];
        match self.collector.inner.http.put(&segments, body, cancel).await {
            Ok(response) if response.status == 200 => {}
            Ok(response) => eprintln!(
                "encrypted-room notice in {room} refused: HTTP {}",
                response.status
            ),
            Err(error) => eprintln!("encrypted-room notice in {room} failed: {error:?}"),
        }
    }
    pub async fn claim_profile(&self) -> Result<OwnedClaimProfile, Error> {
        if self.custody.closed.load(Ordering::Acquire) {
            return Err(Error::Generation);
        }
        let transport = self.collector.inner.expected_transport().await?;
        let rooms = self
            .collector
            .inner
            .config
            .rooms
            .iter()
            .map(|room| {
                OwnedClaimRoom::new(room.room_id.clone(), room.generation, room.privacy.clone())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let profile = OwnedClaimProfile::new(transport, rooms, vec![self.workspace.clone()])?;
        self.custody
            .runtime
            .lock()
            .await
            .as_ref()
            .ok_or(Error::OutcomeUnknown)?
            .bind_claim_profile(profile)
            .map_err(|_| Error::OutcomeUnknown)
    }
    pub async fn dispatch(
        &mut self,
        capability: RunnerCapability,
        limits: hagency_execution::Limits,
    ) -> Result<Operation, Failure> {
        let ticket = {
            let mut runtime = self.custody.runtime.lock().await;
            if self.custody.closed.load(Ordering::Acquire) {
                return Err(Failure::Admission);
            }
            runtime
                .as_mut()
                .ok_or(Failure::Admission)?
                .reserve_dispatch(capability, limits)?
        };
        let original = self.custody.clone();
        // The original runtime spends its exact ticket before enqueue. A later
        // ticket requires the original operation's private acknowledged witness;
        // public Report output and lost waiters cannot authorize one.
        tokio::task::spawn_blocking(move || {
            let mut runtime = original.runtime.blocking_lock();
            if original.closed.load(Ordering::Acquire) {
                return Err(Failure::Admission);
            }
            runtime
                .as_mut()
                .ok_or(Failure::Admission)?
                .dispatch_reserved(ticket)
        })
        .await
        .map_err(|_| Failure::Worker)?
    }
    pub async fn close(self) -> Result<(), Error> {
        self.custody.close().await
    }
}
impl TokenProvisioningHost {
    /// Concrete full inline runtime consumer; existing checkpoint profiles are
    /// unchanged unless this additional private Host capability is configured.
    pub fn with_warm_runtime(
        mut self,
        plan: WarmHostPlan,
        approvals: Arc<crate::ApprovalCollector>,
    ) -> Result<Self, Error> {
        if self.warm.is_some() || self.homes.is_none() || self.rooms.is_none() {
            return Err(Error::Config);
        }
        self.check_approvals(&approvals)?;
        self.factory_approvals = super::ApprovalLink::Fixed(Some(approvals));
        self.warm = Some(plan);
        Ok(self)
    }
    /// ADR-187: an imported fleet's warm factory. Each owner's approval-bot
    /// device is attached by the fleet service with `attach_owner_approvals`
    /// before that owner's first provision.
    pub fn with_warm_plan(mut self, plan: WarmHostPlan) -> Result<Self, Error> {
        if self.warm.is_some() || self.homes.is_none() || self.rooms.is_none() {
            return Err(Error::Config);
        }
        self.factory_approvals = super::ApprovalLink::PerOwner(Mutex::new(BTreeMap::new()));
        self.warm = Some(plan);
        Ok(self)
    }
    /// ADR-187 amendment: the approval-bot device that serves `owner`.
    pub fn attach_owner_approvals(
        &self,
        owner: &str,
        approvals: Arc<crate::ApprovalCollector>,
    ) -> Result<(), Error> {
        self.check_approvals(&approvals)?;
        let super::ApprovalLink::PerOwner(map) = &self.factory_approvals else {
            return Err(Error::Config);
        };
        let mut map = map.lock().map_err(|_| Error::OutcomeUnknown)?;
        match map.get(owner) {
            Some(existing) if !Arc::ptr_eq(existing, &approvals) => Err(Error::Conflict),
            _ => {
                map.insert(owner.to_owned(), approvals);
                Ok(())
            }
        }
    }
    /// Whether `owner` has an approval device attached (always true for a
    /// coordinator install, whose one bot serves its configured owners).
    pub(crate) fn approvals_ready_for(&self, owner: &str) -> bool {
        match &self.factory_approvals {
            super::ApprovalLink::Fixed(link) => link.is_some(),
            super::ApprovalLink::PerOwner(map) => {
                map.lock().map(|m| m.contains_key(owner)).unwrap_or(false)
            }
        }
    }
    fn check_approvals(&self, approvals: &crate::ApprovalCollector) -> Result<(), Error> {
        let config = &approvals.inner.config;
        if !config.approval
            || config.endpoint != self.endpoint
            || config.identity.server_name != self.registration.server_name
            || config.identity.registration_fingerprint != self.fingerprint
            || config.identity.transport.registration_generation != self.registration.generation
            || config.identity.transport.sender_mxid != self.registration.approval_bot_mxid
        {
            return Err(Error::Config);
        }
        Ok(())
    }
    /// The approval collector this engagement's agent uses.
    fn approvals_for(&self, effect: &Effect) -> Result<Arc<crate::ApprovalCollector>, Error> {
        match &self.factory_approvals {
            super::ApprovalLink::Fixed(link) => link.clone().ok_or(Error::Config),
            super::ApprovalLink::PerOwner(map) => {
                let owner = effect
                    .payload
                    .get("request")
                    .and_then(|r| r.get("ownerMxid"))
                    .and_then(serde_json::Value::as_str)
                    .ok_or(Error::Config)?;
                map.lock()
                    .map_err(|_| Error::OutcomeUnknown)?
                    .get(owner)
                    .cloned()
                    .ok_or(Error::AwaitingOwner)
            }
        }
    }
    pub(super) async fn finish_factory(
        &self,
        domain: &DomainStore,
        effect: &Effect,
        account: &Arc<ProvisionedTokenAccount>,
        job: &Arc<Job>,
        cancel: &CancellationToken,
        activated: &mut bool,
    ) -> Result<(), Error> {
        let plan = self.warm.as_ref().ok_or(Error::Config)?;
        let scope = domain
            .provision_runtime_scope(effect.clone(), self.registration.clone())
            .await?;
        let approvals = self.approvals_for(effect)?;
        // Only the original producing writer may contribute this capability.
        approvals
            .inner
            .domain
            .validate_warm_runtime_scope(scope.clone())
            .await?;
        let home = job
            .home
            .lock()
            .map_err(|_| Error::OutcomeUnknown)?
            .as_ref()
            .cloned()
            .ok_or(Error::Config)?;
        let custody = job.factory.as_ref().ok_or(Error::Config)?;
        let runtime = plan
            .start(domain.clone(), scope.clone(), home)
            .await
            .map_err(|error| {
                eprintln!("agent factory {} runtime startup refused: {error:?}", effect.engagement_id);
                Error::OutcomeUnknown
            })?;
        *custody.runtime.lock().await = Some(runtime);
        custody.ready(cancel).await?;
        // GET-only current verification on the original successful SDK job;
        // its Complete ledger prevents any signing upload/session claim replay.
        let anchors = self
            .anchors_for(domain, effect, super::AnchorUse::Enroll)
            .await?;
        account
            .enroll_created_rooms(1, self.key, anchors, cancel)
            .await?;
        custody.ready(cancel).await?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let acknowledgment = custody.activate(cancel).await?;
        if acknowledgment.id != effect.engagement_id
            || acknowledgment.state != EngagementState::Active
        {
            return Err(Error::OutcomeUnknown);
        }
        *activated = true; // Only this received original writer ACK grants forward custody.
        // Native approval control needs its own authenticated current binding;
        // an Agent SDK/DM cannot stand in for the fixed shared approval bot.
        {
            let _turn = approvals.service_turn(cancel).await?;
            approvals
                .observe_factory_engagement(scope.clone(), cancel)
                .await?;
        }
        let collector = account.active_collector(cancel).await?;
        *custody
            .collector
            .lock()
            .map_err(|_| Error::OutcomeUnknown)? = Some(collector);
        custody.ready(cancel).await?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let room_id = account.created_agent_dm()?.ok_or(Error::Recipients)?;
        {
            let owner = custody
                .collector
                .lock()
                .map_err(|_| Error::OutcomeUnknown)?;
            if !owner
                .as_ref()
                .ok_or(Error::OutcomeUnknown)?
                .inner
                .config
                .rooms
                .iter()
                .any(|room| {
                    room.room_id == room_id && matches!(room.privacy, RoomPrivacy::Direct { .. })
                })
            {
                return Err(Error::Recipients);
            }
        }
        let workspace = format!("work_{}", effect.engagement_id);
        domain.register_workspace(workspace).await?;
        let binding = SessionBinding {
            id: format!("session_{}", effect.engagement_id),
            engagement_id: effect.engagement_id.clone(),
            room_id,
            thread_root: None,
        };
        let resolved = domain
            .resolve_verified_matrix_session(binding.clone())
            .await?;
        if resolved.id != binding.id
            || resolved.engagement_id != binding.engagement_id
            || resolved.room_id != binding.room_id
            || resolved.thread_root != binding.thread_root
        {
            return Err(Error::Conflict);
        }
        *custody.binding.lock().map_err(|_| Error::OutcomeUnknown)? = Some(binding);
        Ok(())
    }
    /// The re-attach counterpart of `finish_factory` for a provision that is
    /// already Complete. It starts no warm child and never activates: the scope
    /// and the home were rebuilt and reopened by the store, the rooms and the
    /// enrolled SDK are replayed and reopened by the account. Everything it
    /// writes to the domain is idempotent (workspace, session resolution).
    pub(super) async fn reattach_factory(
        &self,
        domain: &DomainStore,
        effect: &Effect,
        scope: hagency_store::OwnedProvisionScope,
        account: &Arc<ProvisionedTokenAccount>,
        job: &Arc<Job>,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        let plan = self.warm.as_ref().ok_or(Error::Config)?;
        let approvals = self.approvals_for(effect)?;
        approvals
            .inner
            .domain
            .validate_warm_runtime_scope(scope.clone())
            .await?;
        let home = job
            .home
            .lock()
            .map_err(|_| Error::OutcomeUnknown)?
            .as_ref()
            .cloned()
            .ok_or(Error::Config)?;
        let custody = job.factory.as_ref().ok_or(Error::Config)?;
        let runtime = plan
            .reattach_runtime(domain.clone(), scope.clone(), home)
            .await
            .map_err(|_| Error::OutcomeUnknown)?;
        *custody.runtime.lock().await = Some(runtime);
        let rooms = self.rooms.as_ref().ok_or(Error::Config)?;
        // Re-attach continues the stored transport incarnation, or opens the
        // next one when that incarnation was fenced (a failed room read or
        // refresh retires it, and the store admits only generation + 1 after
        // that). After a restart no worker of the fenced incarnation is alive,
        // so the same account and device continue under the new generation.
        let generation = match domain
            .matrix_transport_state(effect.engagement_id.clone())
            .await?
        {
            Some(state) if !state.available => state
                .observation
                .generation
                .checked_add(1)
                .ok_or(Error::Capacity)?,
            Some(state) => state.observation.generation,
            None => 1,
        };
        let collector = account
            .reattach_collector(
                &rooms.representative,
                generation,
                self.key,
                self.anchors_for(domain, effect, super::AnchorUse::Reattach)
                    .await?,
                cancel,
            )
            .await?;
        {
            let _turn = approvals.service_turn(cancel).await?;
            approvals
                .observe_factory_engagement(scope.clone(), cancel)
                .await?;
        }
        let room_id = account.created_agent_dm()?.ok_or(Error::Recipients)?;
        if !collector.inner.config.rooms.iter().any(|room| {
            room.room_id == room_id && matches!(room.privacy, RoomPrivacy::Direct { .. })
        }) {
            return Err(Error::Recipients);
        }
        *custody
            .collector
            .lock()
            .map_err(|_| Error::OutcomeUnknown)? = Some(collector);
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        domain
            .register_workspace(format!("work_{}", effect.engagement_id))
            .await?;
        // The DM session of a later transport incarnation is a new session:
        // the store never rebinds a session id whose route a fence retired,
        // so generation 1 keeps the original id and every later one names its
        // generation (as the project sessions already do).
        let binding = SessionBinding {
            id: if generation == 1 {
                format!("session_{}", effect.engagement_id)
            } else {
                format!("session_{}_{generation}", effect.engagement_id)
            },
            engagement_id: effect.engagement_id.clone(),
            room_id,
            thread_root: None,
        };
        let resolved = domain
            .resolve_verified_matrix_session(binding.clone())
            .await?;
        if resolved.id != binding.id
            || resolved.engagement_id != binding.engagement_id
            || resolved.room_id != binding.room_id
            || resolved.thread_root != binding.thread_root
        {
            return Err(Error::Conflict);
        }
        *custody.binding.lock().map_err(|_| Error::OutcomeUnknown)? = Some(binding);
        Ok(())
    }
    pub fn take_agent(&self, engagement: &str) -> Result<ProvisionedAgent, Error> {
        let job = self
            .jobs
            .lock()
            .map_err(|_| Error::OutcomeUnknown)?
            .get(&format!("provision_{engagement}"))
            .cloned()
            .ok_or(Error::Config)?;
        match job
            .result
            .lock()
            .map_err(|_| Error::OutcomeUnknown)?
            .as_ref()
        {
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error.clone()),
            None => return Err(Error::OutcomeUnknown),
        }
        let custody = job.factory.as_ref().cloned().ok_or(Error::Config)?;
        if custody.closed.load(Ordering::Acquire) {
            return Err(Error::Generation);
        }
        let binding = custody
            .binding
            .lock()
            .map_err(|_| Error::OutcomeUnknown)?
            .clone()
            .ok_or(Error::OutcomeUnknown)?;
        let collector = custody
            .collector
            .lock()
            .map_err(|_| Error::OutcomeUnknown)?
            .as_ref()
            .map(|c| Collector {
                inner: c.inner.clone(),
            })
            .ok_or(Error::OutcomeUnknown)?;
        if custody.taken.swap(true, Ordering::AcqRel) {
            return Err(Error::Busy);
        }
        Ok(ProvisionedAgent {
            custody,
            collector: Arc::new(collector),
            binding,
            workspace: format!("work_{engagement}"),
            appservice_identity: self.as_namespace.is_some(),
        })
    }
    /// Discovery of a NEW agent. An agent a restart brings back enters through
    /// `reattach`, which rebuilds it from the completion's own receipt and the
    /// custody on disk; discovery itself still never derives an owner from a
    /// canonical Active row.
    pub fn take_next_agent(&self) -> Result<Option<ProvisionedAgent>, Error> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::Generation);
        }
        // Metadata inspection never reconstructs an owner from canonical
        // Active state. Only the retained job's actual successful result and
        // non-cloneable take below can admit an original agent.
        let jobs = self.jobs.lock().map_err(|_| Error::OutcomeUnknown)?;
        let mut next = None;
        for (id, job) in jobs.iter() {
            let Some(custody) = &job.factory else {
                continue;
            };
            if custody.closed.load(Ordering::Acquire) || custody.taken.load(Ordering::Acquire) {
                continue;
            }
            if matches!(
                job.result
                    .lock()
                    .map_err(|_| Error::OutcomeUnknown)?
                    .as_ref(),
                Some(Ok(_))
            ) {
                next = Some(
                    id.strip_prefix("provision_")
                        .ok_or(Error::Config)?
                        .to_owned(),
                );
                break;
            }
        }
        drop(jobs);
        next.map(|id| self.take_agent(&id)).transpose()
    }
    pub async fn close_agents(&self) -> Result<(), Error> {
        self.closed.store(true, Ordering::Release);
        let jobs = self
            .jobs
            .lock()
            .map_err(|_| Error::OutcomeUnknown)?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for job in &jobs {
            if let Some(custody) = &job.factory {
                custody.closed.store(true, Ordering::Release);
            }
        }
        let mut failed = false;
        for job in jobs {
            if let Some(custody) = &job.factory {
                if custody.close().await.is_err() {
                    failed = true;
                }
                let account = job
                    .account
                    .lock()
                    .map(|owner| owner.as_ref().cloned())
                    .map_err(|_| Error::OutcomeUnknown);
                match account {
                    Ok(Some(account)) => {
                        if account.close_enrollment_sdk().await.is_err() {
                            failed = true;
                        }
                    }
                    Ok(None) => {}
                    Err(_) => failed = true,
                }
            }
        }
        // This admitted finite drain may already have closed other owners.
        // Individual exact results remain in their original custody; Busy is
        // not an honest aggregate pre-effect verdict after partial shutdown.
        if failed {
            Err(Error::OutcomeUnknown)
        } else {
            Ok(())
        }
    }
}
impl Collector {
    /// Bounded discovery of an actual successful original inline job. Unknown,
    /// unfinished, closed and already-taken jobs never create a replacement.
    pub fn take_next_provisioned_agent(&self) -> Result<Option<ProvisionedAgent>, Error> {
        self.inner
            .config
            .provisioning
            .as_ref()
            .ok_or(Error::Config)?
            .take_next_agent()
    }
    /// Called by the owning native Host after an actual inline verdict returned.
    /// A failed/lost factory result never yields an agent or replacement runtime.
    pub fn take_provisioned_agent(&self, engagement: &str) -> Result<ProvisionedAgent, Error> {
        self.inner
            .config
            .provisioning
            .as_ref()
            .ok_or(Error::Config)?
            .take_agent(engagement)
    }
    /// The engagements this coordinator's inline factory completed and that a
    /// restart should bring back, in id order. Read-only.
    pub async fn provisioned_engagements(&self) -> Result<Vec<String>, Error> {
        if self.inner.config.provisioning.is_none() {
            return Ok(Vec::new());
        }
        Ok(self.inner.domain.inline_factory_engagements().await?)
    }
    /// Bring back one such agent after a restart. It concerns that agent only:
    /// a refusal leaves the coordinator and every other agent as they were. On
    /// success the agent is taken with `take_provisioned_agent` like a new one.
    /// It never uses the coordinator's own SDK, so it does not hold the
    /// coordinator's permit: a refresh that found it Busy would end the
    /// coordinator. The Host's job table is what excludes a second owner.
    pub async fn reattach_provisioned_agent(
        &self,
        engagement: &str,
        cancel: &crate::CancellationToken,
    ) -> Result<(), Error> {
        let inner = self.inner.clone();
        let engagement = engagement.to_owned();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            inner
                .config
                .provisioning
                .as_ref()
                .ok_or(Error::Config)?
                .reattach_completed(&inner.domain, &engagement, &cancel)
                .await
        })
        .await
        .map_err(|_| Error::OutcomeUnknown)?
    }
    /// Provisions waiting for their owner to join, with the wall-clock
    /// millisecond each started waiting. Read-only, for the fleet's status.
    pub fn awaiting_owner_engagements(&self) -> Vec<(String, u64)> {
        self.inner
            .config
            .provisioning
            .as_ref()
            .map(|host| host.awaiting_owner_engagements())
            .unwrap_or_default()
    }
    pub async fn close_provisioned_agents(&self) -> Result<(), Error> {
        let permit = self
            .inner
            .busy
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Some(host) = &inner.config.provisioning {
                host.close_agents().await?;
            }
            Ok(())
        })
        .await
        .map_err(|_| Error::OutcomeUnknown)?
    }
}

/// A reminder in an encrypted shared room is repeated at most this often.
const RENOTICE_GAP_MS: u64 = 15 * 60 * 1000;

/// ADR-188: one session per (engagement, transport generation, room
/// generation, joined room), distinct from the legacy `invite_…` sessions.
fn joined_session_id(engagement: &str, transport: u64, generation: u64, room: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(room.as_bytes());
    let short: String = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
    format!("joined_{engagement}_{transport}_{generation}_{short}")
}
