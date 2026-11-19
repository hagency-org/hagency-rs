//! Original warm custody, never Applied or dispatch authority.
use super::{
    DomainRepository, Effect, EffectState, accounts, read_effect, read_engagement, read_resource,
};
use crate::Error;
use hagency_core::{
    authority::{ProjectRequest, Registration},
    canonical,
    project::{EngagementState, Resource},
};
use rusqlite::{Connection, TransactionBehavior};
use serde::Serialize;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone)]
pub struct OwnedProvisionScope {
    effect: Effect,
    registration: Registration,
    resource: Resource,
    pub(super) account: Option<accounts::Association>,
    owner: Arc<()>,
    claimed: Arc<AtomicBool>,
    runtime_live: Arc<AtomicBool>,
}
/// Exclusive live runtime custody. Cloned bindings share this guard; the next
/// attachment is admitted only after all original workers and bindings drop it.
/// This does not reset the one-shot warm initialization claim.
pub struct OwnedRuntimeLease(Arc<AtomicBool>);
impl Drop for OwnedRuntimeLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
impl OwnedProvisionScope {
    pub fn resource(&self) -> &Resource {
        &self.resource
    }
    pub fn engagement_id(&self) -> &str {
        &self.effect.engagement_id
    }
    pub fn requires_managed_account(&self) -> bool {
        self.account.is_some()
    }
    pub fn claim_warm(&self) -> Result<(), Error> {
        if self.claimed.swap(true, Ordering::AcqRel) {
            return Err(Error::Busy);
        }
        Ok(())
    }
    pub fn claim_runtime(&self) -> Result<Arc<OwnedRuntimeLease>, Error> {
        if self.runtime_live.swap(true, Ordering::AcqRel) {
            return Err(Error::Busy);
        }
        Ok(Arc::new(OwnedRuntimeLease(self.runtime_live.clone())))
    }
    pub(crate) fn queue_value(&self) -> impl Serialize + '_ {
        (
            &self.effect,
            &self.registration,
            &self.resource,
            &self.account,
        )
    }
    pub(crate) fn provision_digest(&self) -> Result<String, Error> {
        canonical::transport_digest(&json!([self.effect, self.registration])).map_err(Into::into)
    }
}
fn current(
    db: &Connection,
    scope: &OwnedProvisionScope,
    registry: &accounts::Registry,
    owner: &Arc<()>,
    active: bool,
) -> Result<(), Error> {
    if !Arc::ptr_eq(owner, &scope.owner) {
        return Err(Error::RunnerAuthority);
    }
    let actual = read_effect(db, &scope.effect.id)?;
    let engagement = read_engagement(db, scope.engagement_id())?;
    if scope.effect.kind != "provision"
        || scope.effect.state != EffectState::Started
        || actual.kind != scope.effect.kind
        || actual.engagement_id != scope.engagement_id()
        || actual.fence != scope.effect.fence
        || actual.fence == 0
        || canonical::transport_digest(&actual.payload)?
            != canonical::transport_digest(&scope.effect.payload)?
        || !((actual.state == EffectState::Started
            && engagement.state == EngagementState::Reserved)
            || (active
                && actual.state == EffectState::Complete
                && engagement.state == EngagementState::Active))
    {
        return Err(Error::State);
    }
    let encoded: String = db.query_row(
        "SELECT config FROM registrations WHERE fleet_id=?1",
        [&scope.registration.fleet_id],
        |r| r.get(0),
    )?;
    let registration: Registration = serde_json::from_str(&encoded)?;
    let generation: u64 = db.query_row(
        "SELECT generation FROM engagements WHERE id=?1",
        [scope.engagement_id()],
        |r| r.get(0),
    )?;
    if registration != scope.registration || generation != registration.generation {
        return Err(Error::Generation);
    }
    let resource = read_resource(db, &scope.resource.id())?;
    if canonical::transport_digest(&runtime_identity(&resource))?
        != canonical::transport_digest(&runtime_identity(&scope.resource))?
        || accounts::association(db, &scope.resource)? != scope.account
    {
        return Err(Error::Unqualified);
    }
    registry.check_resource(db, &scope.resource)?;
    if !accounts::warm_ready(db, scope.account.as_ref(), super::graphs::now_ms()?)? {
        return Err(Error::LocalAuthority);
    }
    Ok(())
}
type Snapshot = (Effect, Registration, Resource);
/// The original claimed snapshot of a provision the inline factory completed,
/// or the reason this engagement is not one (absent, not Active, not Complete,
/// or completed with another receipt). Read-only.
fn inline_factory_snapshot(
    db: &Connection,
    engagement_id: &str,
) -> Result<Result<Snapshot, &'static str>, Error> {
    let id = format!("provision_{engagement_id}");
    let actual = match read_effect(db, &id) {
        Ok(effect) => effect,
        Err(Error::NotFound) => return Ok(Err("no provision effect")),
        Err(error) => return Err(error),
    };
    if actual.kind != "provision"
        || actual.state != EffectState::Complete
        || actual.engagement_id != engagement_id
        || actual.fence == 0
        || read_engagement(db, engagement_id)?.state != EngagementState::Active
    {
        return Ok(Err(
            "provision effect is not a complete, fenced provision of an active engagement",
        ));
    }
    let encoded: String = db.query_row(
        "SELECT r.config FROM registrations r JOIN engagements e ON e.fleet_id=r.fleet_id WHERE e.id=?1",
        [engagement_id],
        |r| r.get(0),
    )?;
    let registration: Registration = serde_json::from_str(&encoded)?;
    let Some(resource) = actual.payload.get("resource").cloned() else {
        return Ok(Err("provision payload has no resource"));
    };
    let resource: Resource = serde_json::from_value(resource)?;
    // The claim produced exactly this value; only its state has moved since
    // and, after an applied rebind, its resource.runtimePlugin.
    let effect = Effect {
        state: EffectState::Started,
        ..actual
    };
    let stored: Option<String> = db.query_row(
        "SELECT outcome_digest FROM effects WHERE id=?1",
        [&id],
        |r| r.get(0),
    )?;
    if !receipt_proven(db, &effect, &registration, stored.as_deref())? {
        return Ok(Err(
            "provision receipt does not match the payload or a recorded runtime rebind of it",
        ));
    }
    Ok(Ok((effect, registration, resource)))
}
fn receipt_digest(effect: &Effect, registration: &Registration) -> Result<String, Error> {
    let receipt = format!(
        "inline_factory_{}",
        canonical::transport_digest(&json!([effect, registration]))?
    );
    Ok(canonical::digest(&serde_json::to_value(
        super::EffectOutcome::Applied { receipt },
    )?)?)
}
/// The receipt the completion stored proves this effect. It must match the
/// payload as it is. The only payload change admitted is the resource's
/// runtimePlugin, and only as the audit of applied rebinds and rollbacks
/// (ADR-197, append-only) records it: the rows chain unbroken from the
/// original identity to the current one, and the payload with the first row's
/// old identity put back must reproduce the stored receipt. Anything else
/// (another resource, request, plugin id or adapter id, or a runtimePlugin the
/// audit does not end at) fails the digest or the chain.
fn receipt_proven(
    db: &Connection,
    effect: &Effect,
    registration: &Registration,
    stored: Option<&str>,
) -> Result<bool, Error> {
    let Some(stored) = stored else {
        return Ok(false);
    };
    if receipt_digest(effect, registration)? == stored {
        return Ok(true);
    }
    type Pair = (Option<String>, Option<String>);
    let rows = db
        .prepare(
            "SELECT old_package_sha256,old_settings_sha256,new_package_sha256,new_settings_sha256 \
             FROM engagement_runtime_rebinds WHERE engagement_id=?1 \
             AND ((action='rebind' AND outcome='applied') OR (action='rollback' AND outcome='rolled_back')) \
             ORDER BY id",
        )?
        .query_map([&effect.engagement_id], |r| {
            Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?)))
        })?
        .collect::<Result<Vec<(Pair, Pair)>, _>>()?;
    let (Some((first, _)), Some((_, last))) = (rows.first(), rows.last()) else {
        return Ok(false);
    };
    let mut previous: Option<&Pair> = None;
    for (old, new) in &rows {
        if old.0.is_none()
            || old.1.is_none()
            || new.0.is_none()
            || new.1.is_none()
            || previous.is_some_and(|p| p != old)
        {
            return Ok(false);
        }
        previous = Some(new);
    }
    let mut original = effect.clone();
    let Some(plugin) = original
        .payload
        .pointer_mut("/resource/runtimePlugin")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(false);
    };
    let current = (
        plugin.get("packageSha256").and_then(|v| v.as_str()),
        plugin.get("settingsSha256").and_then(|v| v.as_str()),
    );
    if current.0 != last.0.as_deref() || current.1 != last.1.as_deref() {
        return Ok(false);
    }
    plugin.insert("packageSha256".into(), json!(first.0));
    plugin.insert("settingsSha256".into(), json!(first.1));
    Ok(receipt_digest(&original, registration)? == stored)
}
impl DomainRepository {
    /// Original registry owner only; never reopen a writer or select an account
    /// from caller metadata. The scope's producing owner and current facts gate
    /// this read before the fixed Host can prepare a launch.
    pub fn provision_runtime_account(
        &mut self,
        scope: &OwnedProvisionScope,
    ) -> Result<Option<crate::ManagedAccount>, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, scope, &self.accounts, &self.approval_owner, false)?;
        tx.commit()?;
        scope
            .account
            .as_ref()
            .map(|association| self.managed_account(accounts::provision_account_id(association)))
            .transpose()
    }
    /// Trusted Host admission after physical checks, not caller-supplied proof.
    /// The original acknowledged scope must still be Started/Reserved and warm
    /// consumption sticky. Both admission and the existing kernel share one tx.
    pub fn complete_original_provision(
        &mut self,
        scope: &OwnedProvisionScope,
    ) -> Result<hagency_core::project::Engagement, Error> {
        if !scope.claimed.load(Ordering::Acquire) {
            return Err(Error::State);
        }
        let receipt = format!("inline_factory_{}", scope.provision_digest()?);
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, scope, &self.accounts, &self.approval_owner, false)?;
        let value = super::observe_effect_transaction(
            &tx,
            &scope.effect.id,
            scope.effect.fence,
            &super::EffectOutcome::Applied { receipt },
        )?;
        tx.commit()?;
        Ok(value)
    }
    pub fn provision_runtime_scope(
        &mut self,
        effect: &Effect,
        registration: &Registration,
    ) -> Result<OwnedProvisionScope, Error> {
        self.provision_scope_at(effect, registration, false)
    }
    /// Mint inspection custody only. Runtime authorization still rejects the
    /// uncertain effect until the original Matrix artifacts have been checked.
    pub fn inspect_provision_scope(
        &mut self,
        effect: &Effect,
        registration: &Registration,
    ) -> Result<OwnedProvisionScope, Error> {
        self.provision_scope_at(effect, registration, true)
    }
    pub fn resume_inspected_provision(&mut self, scope: &OwnedProvisionScope) -> Result<(), Error> {
        if !Arc::ptr_eq(&scope.owner, &self.approval_owner)
            || scope.claimed.load(Ordering::Acquire)
            || scope.runtime_live.load(Ordering::Acquire)
        {
            return Err(Error::RunnerAuthority);
        }
        self.validate_recoverable_provision(&scope.effect, &scope.registration)?;
        if self.warm_scopes.len() >= 16 {
            return Err(Error::Capacity);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute("UPDATE effects SET state='started',outcome_digest=NULL WHERE id=?1 AND fence=?2 AND state='uncertain'",
            rusqlite::params![scope.effect.id, scope.effect.fence])?;
        if changed != 1 {
            return Err(Error::State);
        }
        current(&tx, scope, &self.accounts, &self.approval_owner, false)?;
        let result = json!({"effectId":scope.effect.id,"fence":scope.effect.fence,"checkpoint":"enrolled_before_owner_invite"});
        let digest = canonical::transport_digest(&result)?;
        tx.execute("INSERT OR IGNORE INTO decisions(id,digest,result,kind,at) VALUES(?1,?2,?3,'resume_enrolled_provision',?4)",
            rusqlite::params![format!("provision_recovery_{digest}"), digest, result.to_string(), super::graphs::now_ms()?])?;
        tx.commit()?;
        self.warm_scopes
            .insert(scope.effect.id.clone(), scope.clone());
        Ok(())
    }
    fn provision_scope_at(
        &mut self,
        effect: &Effect,
        registration: &Registration,
        recovering: bool,
    ) -> Result<OwnedProvisionScope, Error> {
        if recovering {
            self.validate_recoverable_provision(effect, registration)?;
        } else {
            self.validate_provision_account(effect, registration)?;
        }
        let resource: Resource = serde_json::from_value(
            effect
                .payload
                .get("resource")
                .cloned()
                .ok_or(Error::Schema)?,
        )?;
        let request: ProjectRequest = serde_json::from_value(
            effect
                .payload
                .get("request")
                .cloned()
                .ok_or(Error::Schema)?,
        )?;
        request.validate(registration)?;
        resource.validate()?;
        if request.engagement_id()? != effect.engagement_id
            || request.agent_definition.resource_id != resource.id()
            || !resource.qualifies(&request.role)
        {
            return Err(Error::Unqualified);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let scope = OwnedProvisionScope {
            effect: effect.clone(),
            registration: registration.clone(),
            account: accounts::association(&tx, &resource)?,
            resource,
            owner: self.approval_owner.clone(),
            claimed: Arc::new(AtomicBool::new(false)),
            runtime_live: Arc::new(AtomicBool::new(false)),
        };
        if recovering {
            let actual = read_resource(&tx, &scope.resource.id())?;
            if canonical::transport_digest(&runtime_identity(&actual))?
                != canonical::transport_digest(&runtime_identity(&scope.resource))?
            {
                return Err(Error::Unqualified);
            }
            self.accounts.check_resource(&tx, &scope.resource)?;
            tx.commit()?;
            return Ok(scope);
        }
        current(&tx, &scope, &self.accounts, &self.approval_owner, false)?;
        tx.commit()?;
        if let Some(known) = self.warm_scopes.get(&effect.id) {
            if canonical::transport_digest(&json!(known.queue_value()))?
                != canonical::transport_digest(&json!(scope.queue_value()))?
            {
                return Err(Error::Conflict);
            }
            return Ok(known.clone());
        }
        if self.warm_scopes.len() >= 16 {
            return Err(Error::Capacity);
        }
        self.warm_scopes.insert(effect.id.clone(), scope.clone());
        Ok(scope)
    }
    /// A restart loses the in-memory scope of an agent this service already
    /// completed. This rebuilds that ORIGINAL claimed snapshot from the durable
    /// Complete row and proves it exact: the receipt the original completion
    /// stored is recomputed from the rebuilt snapshot and must match. A
    /// provision another path completed, a changed payload, fence or
    /// registration, or an engagement that is no longer Active yields no scope.
    /// It never claims, completes or changes an effect.
    pub fn reattach_provision_scope(
        &mut self,
        engagement_id: &str,
    ) -> Result<(Effect, Registration, OwnedProvisionScope), Error> {
        hagency_core::project::identifier(engagement_id, 128)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (effect, registration, resource) =
            inline_factory_snapshot(&tx, engagement_id)?.map_err(|_| Error::State)?;
        let scope = OwnedProvisionScope {
            effect: effect.clone(),
            registration: registration.clone(),
            account: accounts::association(&tx, &resource)?,
            resource,
            owner: self.approval_owner.clone(),
            claimed: Arc::new(AtomicBool::new(false)),
            runtime_live: Arc::new(AtomicBool::new(false)),
        };
        current(&tx, &scope, &self.accounts, &self.approval_owner, true)?;
        tx.commit()?;
        if let Some(known) = self.warm_scopes.get(&effect.id) {
            if canonical::transport_digest(&json!(known.queue_value()))?
                != canonical::transport_digest(&json!(scope.queue_value()))?
            {
                return Err(Error::Conflict);
            }
            return Ok((effect, registration, known.clone()));
        }
        if self.warm_scopes.len() >= 16 {
            return Err(Error::Capacity);
        }
        self.warm_scopes.insert(effect.id.clone(), scope.clone());
        Ok((effect, registration, scope))
    }
    /// Engagements this service's inline factory completed and that are still
    /// Active, in id order. An engagement another path provisioned (an adopted
    /// coordinator or approval account) is not listed.
    pub fn inline_factory_engagements(
        &mut self,
        registration: &Registration,
    ) -> Result<Vec<String>, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = tx
            .prepare(
                "SELECT e.id FROM engagements e JOIN effects f ON f.engagement_id=e.id \
                 WHERE e.fleet_id=?1 AND e.generation=?2 \
                 AND e.state='active' AND f.kind='provision' AND f.state='complete' \
                 ORDER BY e.id",
            )?
            .query_map(
                rusqlite::params![registration.fleet_id, registration.generation],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut found = Vec::new();
        for id in ids {
            match inline_factory_snapshot(&tx, &id)? {
                Ok((_, actual, _)) if &actual == registration => found.push(id),
                Ok(_) => tracing::warn!(
                    engagement = %id,
                    reason = "engagement registration differs from this service's",
                    "active engagement skipped by re-attach"
                ),
                Err(reason) => tracing::warn!(
                    engagement = %id, reason,
                    "active engagement skipped by re-attach"
                ),
            }
        }
        tx.commit()?;
        Ok(found)
    }
    pub fn validate_warm_runtime_scope(
        &mut self,
        scope: &OwnedProvisionScope,
    ) -> Result<(), Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, scope, &self.accounts, &self.approval_owner, true)?;
        tx.commit()?;
        Ok(())
    }
    /// The provider-managed account of a scope a restart rebuilt: the same
    /// current-facts gate as `provision_runtime_account`, on the completed
    /// provision, and the binding the reopened registry loaded from its own
    /// durable rows, never caller metadata. None for a scope on the
    /// operator's own login; a lapsed login observation refuses.
    pub fn reattach_runtime_account(
        &mut self,
        scope: &OwnedProvisionScope,
    ) -> Result<Option<crate::ManagedAccount>, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, scope, &self.accounts, &self.approval_owner, true)?;
        tx.commit()?;
        scope
            .account
            .as_ref()
            .map(|association| self.managed_account(accounts::provision_account_id(association)))
            .transpose()
    }
}

/// The part of a resource the running agent depends on: what it runs and on
/// which seat. The monthly ceiling, the publication flag and the derived role
/// cache are budget and catalog facts; editing them (raising a ceiling) must
/// not detach the agents already running on the resource.
fn runtime_identity(resource: &hagency_core::project::Resource) -> serde_json::Value {
    json!({
        "preset_id": resource.preset_id,
        "seat_id": resource.seat_id,
        "framework": resource.framework,
        "model": resource.model,
        "provider": resource.provider,
        "reasoning": resource.reasoning,
    })
}
