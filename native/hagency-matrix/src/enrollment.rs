//! Explicit one-pass enrollment on the original Collector and SDK owner.
use crate::{
    CancellationToken, Collector, Error,
    collector::Inner,
    sdk::{
        Owner,
        enrollment::{Command, Handle, Purpose},
    },
};
use state::View;
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};
use tokio::time::Instant;

pub(crate) mod provisioning;
pub(crate) mod state;
#[derive(Clone, Copy)]
pub(crate) enum Scope<'a> {
    Agent,
    Approval(&'a [String]),
    Provision(&'a provisioning::Scope),
}
impl Scope<'_> {
    fn purpose(self) -> Purpose {
        match self {
            Self::Agent => Purpose::Agent,
            Self::Approval(_) => Purpose::Approval,
            Self::Provision(_) => Purpose::Agent,
        }
    }
}

#[derive(Default)]
pub(crate) struct Jobs(Mutex<Option<Arc<Job>>>);
struct Job {
    result: Mutex<Option<Result<(), Error>>>,
}

impl Collector {
    /// Enroll only the explicitly provisioned ordinary fresh-account profile.
    /// This result creates no dispatch, source, upload or publication authority.
    pub async fn enroll_fresh_account(&self, cancel: &CancellationToken) -> Result<(), Error> {
        if self.inner.config.enrollment.is_none() {
            return Err(Error::Config);
        }
        let permit = self
            .inner
            .busy
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let deadline = Instant::now() + self.inner.config.limits.sdk;
        let job = Arc::new(Job {
            result: Mutex::new(None),
        });
        {
            let mut registry = self
                .inner
                .enrollment_jobs
                .0
                .lock()
                .map_err(|_| Error::OutcomeUnknown)?;
            if let Some(previous) = registry.as_ref() {
                match previous
                    .result
                    .lock()
                    .map_err(|_| Error::OutcomeUnknown)?
                    .as_ref()
                {
                    Some(Ok(())) => {}
                    Some(Err(error)) => return Err(error.clone()),
                    None => return Err(Error::OutcomeUnknown),
                }
            }
            *registry = Some(job.clone());
        }
        let inner = self.inner.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let expected = inner.expected_transport().await;
            let result = match expected {
                Err(error) => Err(error),
                Ok(expected) => {
                    let result = tokio::time::timeout_at(
                        deadline,
                        inner.enroll(Scope::Agent, &cancel, deadline),
                    )
                    .await
                    .unwrap_or(Err(Error::Timeout));
                    match result {
                        Ok(()) => Ok(()),
                        Err(error) => inner.fence_observation(expected, error).await,
                    }
                }
            };
            if let Ok(mut slot) = job.result.lock() {
                *slot = Some(result.clone());
            }
            result
        })
        .await
        .map_err(|_| Error::OutcomeUnknown)?
    }
}

impl Inner {
    /// ADR-183 B: re-derive a completed enrollment's recipient set now — the
    /// same pass a startup makes (anchor pinned, unsigned devices excluded
    /// and counted, an Olm session claimed for any signed device that
    /// appeared since). Only a completed enrollment is re-verified; an
    /// absent or partial one is not enrolled from here.
    pub(crate) async fn reverify(
        &self,
        scope: Scope<'_>,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<(), Error> {
        let owner = self.enrollment_handle(scope).await?;
        if !matches!(owner.command(Command::Status).await?, View::Complete) {
            return Err(Error::OutcomeUnknown);
        }
        self.enroll(scope, cancel, deadline).await
    }
    pub(crate) async fn enrollment_handle(&self, scope: Scope<'_>) -> Result<Handle, Error> {
        let mut guard = self.owner.lock().await;
        if guard.is_none() {
            *guard = Some(match scope {
                Scope::Provision(_) => Owner::open(&self.config).await?,
                _ => Owner::open_existing(&self.config).await?,
            });
        }
        Ok(guard
            .as_ref()
            .ok_or(Error::Storage)?
            .enrollment_handle_for(scope.purpose()))
    }
    async fn enrollment_current(&self, cancel: &CancellationToken) -> Result<Vec<String>, Error> {
        self.expected_transport().await?;
        self.whoami(cancel).await?;
        let mut users = BTreeSet::from([self.config.identity.transport.sender_mxid.clone()]);
        for room in &self.config.rooms {
            let observation = self.collect_room_observation(room, cancel).await?;
            if observation.encrypted {
                users.extend(observation.joined);
            }
            if users.len() > 17 {
                return Err(Error::Capacity);
            }
        }
        let users = users.into_iter().collect::<Vec<_>>();
        self.config
            .enrollment
            .as_ref()
            .ok_or(Error::Config)?
            .users(&self.config.identity.transport.sender_mxid, &users)?;
        let state = self
            .domain
            .matrix_transport_state(self.config.identity.transport.engagement_id.clone())
            .await?;
        if state.is_none_or(|s| !s.available || s.observation != self.config.identity.transport) {
            return Err(Error::Generation);
        }
        Ok(users)
    }
    async fn enrollment_current_for(
        &self,
        scope: Scope<'_>,
        cancel: &CancellationToken,
    ) -> Result<Vec<String>, Error> {
        match scope {
            Scope::Agent => self.enrollment_current(cancel).await,
            Scope::Approval(engagements) => {
                self.approval_enrollment_current(engagements, cancel).await
            }
            Scope::Provision(scope) => scope.current(self, cancel).await,
        }
    }
    async fn enrollment_query(
        &self,
        owner: &Handle,
        users: &[String],
        cancel: &CancellationToken,
    ) -> Result<serde_json::Value, Error> {
        let View::Query(body) = owner.command(Command::Query(users.to_vec())).await? else {
            return Err(Error::Storage);
        };
        let response = self
            .http
            .post(&["_matrix", "client", "v3", "keys", "query"], body, cancel)
            .await?
            .success()?;
        state::size(&response, state::QUERY)?;
        Ok(response)
    }
    pub(crate) async fn enroll(
        &self,
        scope: Scope<'_>,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<(), Error> {
        self.enroll_bounded(scope, cancel, Some(deadline)).await
    }

    /// A provision is a finite sequence of durable steps. Give each step its
    /// own SDK budget; key publication must not consume the first-sync budget.
    pub(crate) async fn enroll_provision(
        &self,
        scope: &provisioning::Scope,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        self.enroll_bounded(Scope::Provision(scope), cancel, None)
            .await
    }

    async fn enroll_bounded(
        &self,
        scope: Scope<'_>,
        cancel: &CancellationToken,
        fixed_deadline: Option<Instant>,
    ) -> Result<(), Error> {
        let next_deadline =
            || fixed_deadline.unwrap_or_else(|| Instant::now() + self.config.limits.sdk);
        let deadline = next_deadline();
        let prepared = tokio::time::timeout_at(deadline, async {
            checkpoint(cancel, deadline)?;
            // Validate before opening a fresh SDK, and retain that census.
            // Every write below still gets a fresh check immediately before it.
            let initial_users = if let Scope::Provision(scope) = scope {
                Some(scope.current(self, cancel).await?)
            } else {
                None
            };
            let owner = self.enrollment_handle(scope).await?;
            let status = match owner.command(Command::Status).await? {
                View::Absent => Status::Absent,
                View::Complete => Status::Complete,
                View::Verify | View::Ready | View::Write(_) => Status::Resume,
                _ => return Err(Error::Storage),
            };
            let users = match initial_users {
                Some(users) => users,
                None => self.enrollment_current_for(scope, cancel).await?,
            };
            let versions = self
                .http
                .request(&["_matrix", "client", "versions"], None, cancel)
                .await?
                .success()?;
            let advertised = versions
                .get("versions")
                .and_then(|v| v.as_array())
                .ok_or(Error::Unsupported)?;
            if advertised.len() > 64
                || !advertised.iter().any(|v| {
                    matches!(
                        v.as_str(),
                        Some("v1.11" | "v1.12" | "v1.13" | "v1.14" | "v1.15" | "v1.16" | "v1.17")
                    )
                })
            {
                return Err(Error::Unsupported);
            }
            match status {
                Status::Absent => {
                    let query = self.enrollment_query(&owner, &users, cancel).await?;
                    owner.command(Command::Prepare(query)).await?;
                }
                Status::Complete => {
                    let query = self.enrollment_query(&owner, &users, cancel).await?;
                    match owner.command(Command::Verify(query)).await? {
                        View::Complete | View::Write(_) => {}
                        _ => return Err(Error::Storage),
                    }
                }
                Status::Resume => {}
            }
            checkpoint(cancel, deadline)?;
            Ok((owner, users))
        })
        .await
        .unwrap_or(Err(Error::Timeout));
        let (owner, users) = prepared.map_err(|error| {
            eprintln!("matrix enrollment stage=prepare error={error:?}");
            error
        })?;
        // The ledger caps writes at WRITES. A step either advances a durable
        // phase or finishes; a stalled step cannot continually renew its timer.
        for _ in 0..state::WRITES + 4 {
            let deadline = next_deadline();
            let mut stage = "status";
            let mut unsent = None;
            let result = tokio::time::timeout_at(deadline, async {
                checkpoint(cancel, deadline)?;
                match owner.command(Command::Next).await? {
                    View::Write(packet) => {
                        stage = "before_key_write";
                        owner.command(Command::Possible(packet.index)).await?;
                        unsent = Some(packet.index);
                        let acceptance = owner.acceptance()?;
                        if self.enrollment_current_for(scope, cancel).await? != users {
                            return Err(Error::Recipients);
                        }
                        checkpoint(cancel, deadline)?;
                        stage = "key_write";
                        // From here the POST may have crossed the wire. Never
                        // clear Possible because its response was lost.
                        unsent = None;
                        let response = self
                            .http
                            .post(packet.kind.path(), packet.body, cancel)
                            .await?
                            .success()?;
                        acceptance.accept(packet.index, response).await?;
                        Ok(false)
                    }
                    View::Verify => {
                        stage = "verify_keys";
                        if self.enrollment_current_for(scope, cancel).await? != users {
                            return Err(Error::Recipients);
                        }
                        let query = self.enrollment_query(&owner, &users, cancel).await?;
                        owner.command(Command::Verify(query)).await?;
                        Ok(false)
                    }
                    View::Ready => {
                        stage = "finish_keys";
                        if self.enrollment_current_for(scope, cancel).await? != users {
                            return Err(Error::Recipients);
                        }
                        checkpoint(cancel, deadline)?;
                        let View::Complete = owner.command(Command::Finish).await? else {
                            return Err(Error::Storage);
                        };
                        if self.enrollment_current_for(scope, cancel).await? != users {
                            return Err(Error::Recipients);
                        }
                        checkpoint(cancel, deadline)?;
                        Ok(true)
                    }
                    View::Complete => {
                        stage = "verify_complete";
                        if self.enrollment_current_for(scope, cancel).await? != users {
                            return Err(Error::Recipients);
                        }
                        checkpoint(cancel, deadline)?;
                        Ok(true)
                    }
                    _ => Err(Error::OutcomeUnknown),
                }
            })
            .await
            .unwrap_or(Err(Error::Timeout));
            if let Some(index) = unsent {
                // The original future ended before entering HTTP. Persist the
                // negative observation; a lost SDK acknowledgment stays unknown.
                owner.command(Command::NotSent(index)).await?;
            }
            match result {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) => {
                    eprintln!("matrix enrollment stage={stage} error={error:?}");
                    return Err(error);
                }
            }
        }
        Err(Error::Capacity)
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Absent,
    Complete,
    Resume,
}
pub(crate) fn checkpoint(cancel: &CancellationToken, deadline: Instant) -> Result<(), Error> {
    if cancel.is_cancelled() {
        Err(Error::Cancelled)
    } else if Instant::now() >= deadline {
        Err(Error::Timeout)
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/enrollment/mod.rs"]
mod tests;
#[cfg(test)]
pub(crate) use tests::crypto as crypto_fixture;
