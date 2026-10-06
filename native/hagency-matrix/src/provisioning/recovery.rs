//! Inspect a completed pre-invite checkpoint; never replay an uncertain write.
use super::*;

impl TokenProvisioningHost {
    pub async fn recover_enrolled(
        &self,
        domain: &DomainStore,
        engagement: &str,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) || cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let plan = self.rooms.as_ref().ok_or(Error::Config)?;
        if self.homes.is_none() || self.warm.is_none() {
            return Err(Error::Config);
        }
        let mut effect = domain.effect(format!("provision_{engagement}")).await?;
        if effect.state != hagency_store::EffectState::Uncertain {
            return Err(Error::Config);
        }
        // All physical bindings were made with the original Started snapshot.
        effect.state = hagency_store::EffectState::Started;
        let inspection = domain
            .inspect_provision_scope(effect.clone(), self.registration.clone())
            .await?;
        let anchors = self.anchors_for(domain, &effect, AnchorUse::Enroll).await?;
        let owner = domain
            .engagement_owner(engagement.to_owned())
            .await?
            .ok_or(Error::Config)?;
        if !self.approvals_ready_for(&owner) {
            return Err(Error::AwaitingOwner);
        }
        let job = Arc::new(Job {
            factory: Some(Arc::new(factory::Custody::new())),
            home: Mutex::new(None),
            account: Mutex::new(None),
            result: Mutex::new(None),
            effect: Mutex::new(Some(effect.clone())),
            awaiting_since: Mutex::new(None),
        });
        {
            let mut jobs = self.jobs.lock().map_err(|_| Error::OutcomeUnknown)?;
            if jobs.contains_key(&effect.id) {
                return Err(Error::Busy);
            }
            if jobs.len() >= MAX_JOBS {
                return Err(Error::Capacity);
            }
            jobs.insert(effect.id.clone(), job.clone());
        }
        let mut stage = "inspect_home";
        let result = async {
            let home = self.homes.as_ref().ok_or(Error::Config)?.reopen(
                &inspection,
                &effect,
                &self.registration,
            )?;
            *job.home.lock().map_err(|_| Error::OutcomeUnknown)? = Some(home);
            stage = "inspect_account";
            let operation = if let Some(namespace) = &self.as_namespace {
                TokenAccountProvision::application_service(
                    &self.registration,
                    &effect,
                    self.endpoint.as_str(),
                    crate::ApplicationServiceCredential::new(&self.token, namespace)?,
                    self.state.clone(),
                    self.key,
                    self.limits.clone(),
                )
            } else {
                TokenAccountProvision::new(
                    &self.registration,
                    &effect,
                    self.endpoint.as_str(),
                    &self.token,
                    self.state.clone(),
                    self.key,
                    self.limits.clone(),
                )
            }?;
            let mut operation = operation.for_recovery().with_domain(
                domain.clone(),
                effect.clone(),
                self.registration.clone(),
            );
            operation.factory_rooms = Some(self.factory_rooms.clone());
            operation.roots = self.roots.clone();
            let mut account = operation.execute(cancel).await?;
            stage = "inspect_enrollment";
            account
                .resume_before_owner_invite(&plan.representative, anchors, inspection, cancel)
                .await?;
            let account = Arc::new(account);
            *job.account.lock().map_err(|_| Error::OutcomeUnknown)? = Some(account.clone());
            stage = "continue_setup";
            let mut activated = false;
            let result = self
                .continue_provision(domain, &effect, &account, &job, cancel, &mut activated)
                .await
                .map(|()| account);
            self.settle_provision(domain, &effect, &job, activated, result)
                .await
        }
        .await;
        eprintln!(
            "provision recovery stage={stage} effect={} result={:?}",
            effect.id,
            result.as_ref().map(|_| ())
        );
        let response = result.as_ref().map(|_| ()).map_err(Clone::clone);
        *job.result.lock().map_err(|_| Error::OutcomeUnknown)? = Some(result);
        response
    }
}
