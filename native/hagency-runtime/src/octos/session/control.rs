//! Wire mechanics only (ADR-193 decision 4). Original durable owner grants
//! remain the Host's duty.
use super::{Error, Event, Operation, Phase, SessionDriver, WriteProgress, io};
use serde_json::json;
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::Instant,
};

const MAX_APPROVALS: usize = 16;

#[derive(Clone, Copy, Debug)]
pub struct ApprovalControlPolicy {
    pub owner_wait_ms: u64,
    pub response_reserve_ms: u64,
}
/// Host-selected wire behavior, not an owner verdict. No JSON constructor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
}
pub enum ControlUpdate<T> {
    Event(Event),
    Control(T),
}
pub enum PreparedUpdate {
    Event(Event),
    WriteAccepted(WriteProgress),
}
/// Exactly one original frame. Neither Clone, Deserialize nor arbitrary bytes.
pub struct PreparedApproval {
    source: Arc<()>,
    id: String,
    frame: io::PreparedFrame,
    response_deadline: Instant,
    sequence: u64,
}
impl PreparedApproval {
    pub fn approval_id(&self) -> &str {
        &self.id
    }
    pub fn response_deadline(&self) -> Instant {
        self.response_deadline
    }
}
enum Stage {
    Waiting,
    /// The owner wait ran out unanswered and the host took the approval over
    /// (the ADR046 amendment, as for Codex). The only frame it can produce is
    /// the deny, inside the response reserve fixed at admission.
    Expired,
    Prepared(u64),
    Sent,
    /// Octos settled it itself: cancelled, timed out or decided elsewhere.
    Withdrawn,
}
struct Approval {
    stage: Stage,
    owner: Option<Instant>,
    response: Option<Instant>,
}
#[derive(Default)]
pub(super) struct State {
    policy: Option<ApprovalControlPolicy>,
    /// The host answers an unanswered approval at its owner bound instead of
    /// letting the session time out there.
    host_expiry: bool,
    entries: BTreeMap<String, Approval>,
    /// Each prepared `approval/respond` by request ID, until Octos answers it.
    answers: BTreeMap<String, String>,
    read_deadline: Option<Instant>,
    sequence: u64,
}
impl State {
    pub fn clear(&mut self) {
        self.entries.clear();
        self.answers.clear();
    }
    pub fn observed(&mut self) {
        self.read_deadline = None;
    }
    pub fn admit<R, W, E>(
        &mut self,
        id: &str,
        received: Instant,
        wire: &io::Wire<R, W, E>,
    ) -> Result<(), Error> {
        if self.entries.contains_key(id) {
            return Err(Error::Identity);
        }
        if self.entries.len() >= MAX_APPROVALS {
            return Err(Error::Capacity);
        }
        let (owner, response) = match self.policy {
            Some(policy) => {
                let (owner, response) = wire.permission_deadlines(
                    received,
                    policy.owner_wait_ms,
                    policy.response_reserve_ms,
                )?;
                (Some(owner), Some(response))
            }
            None => (None, None),
        };
        self.entries.insert(
            id.to_owned(),
            Approval {
                stage: Stage::Waiting,
                owner,
                response,
            },
        );
        Ok(())
    }
    /// Octos settled an approval (`approval/cancelled`, `approval/decided` or
    /// `approval/auto_resolved`). True when it withdraws one this host has not
    /// answered: a decision on the answer this host sent is its echo. So is a
    /// settlement while this host's answer is in write custody (`writing`):
    /// Octos may echo the answer before its flush is observed, and a started
    /// frame is finished, never cut. Octos's own answer to it then says what
    /// happened (ADR-046: a resolution in flight is a quiet drop).
    pub fn settled(&mut self, id: &str, writing: Option<u64>) -> bool {
        match self.entries.get_mut(id) {
            Some(approval) => match approval.stage {
                Stage::Prepared(sequence) if writing == Some(sequence) => false,
                Stage::Waiting | Stage::Expired | Stage::Prepared(_) => {
                    approval.stage = Stage::Withdrawn;
                    true
                }
                Stage::Sent | Stage::Withdrawn => false,
            },
            None => false,
        }
    }
    /// Octos's answer to one of this host's `approval/respond` requests. A
    /// refusal means Octos had already settled that approval: under the
    /// ADR-046 rules a resolution after admission is a quiet drop.
    pub fn answered(&mut self, id: &str) -> bool {
        self.answers.remove(id).is_some()
    }
    pub fn deadline<R, W, E>(&mut self, wire: &io::Wire<R, W, E>) -> Result<Instant, Error> {
        if self.policy.is_none() {
            return Ok(wire.event_deadline());
        }
        let until = self
            .entries
            .values()
            .filter_map(|approval| match approval.stage {
                // A host that expires approvals needs the session readable
                // through the owner bound, or the wait spanning it would end
                // the session before the host could act. A read bound only: an
                // allow prepared after the owner bound is still refused.
                Stage::Waiting if self.host_expiry => approval.response,
                Stage::Waiting => approval.owner,
                Stage::Expired | Stage::Prepared(_) => approval.response,
                Stage::Sent | Stage::Withdrawn => None,
            })
            .min()
            .unwrap_or_else(|| {
                *self
                    .read_deadline
                    .get_or_insert_with(|| wire.event_deadline())
            });
        if Instant::now() >= until {
            return Err(Error::Timeout);
        }
        Ok(until)
    }
}
impl<R, W, E> SessionDriver<R, W, E> {
    /// Explicit host opt-in once the dispatch's turn is accepted and before
    /// any approval is consumed. Does not authorize an allow or a response.
    pub fn enable_approval_control(&mut self, policy: ApprovalControlPolicy) -> Result<(), Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = operation.driver.enable_inner(policy);
        operation.finish(result)
    }
    fn enable_inner(&mut self, policy: ApprovalControlPolicy) -> Result<(), Error> {
        if self.session_id.is_none()
            || self.control.policy.is_some()
            || !self.control.entries.is_empty()
        {
            return Err(Error::State);
        }
        self.wire.permission_deadlines(
            Instant::now(),
            policy.owner_wait_ms,
            policy.response_reserve_ms,
        )?;
        self.control.policy = Some(policy);
        Ok(())
    }
    /// The host will answer an unanswered approval at its owner bound with a
    /// deny (`expire_approval`). Grants nothing.
    pub fn enable_owner_wait_expiry(&mut self) -> Result<(), Error> {
        if self.phase != Phase::Running || self.control.policy.is_none() {
            return Err(Error::State);
        }
        self.control.host_expiry = true;
        Ok(())
    }
    /// Hand one unanswered approval from the owner to the host at its owner
    /// bound. Writes no byte and renews no clock. Not wrapped in `Operation`:
    /// an error here is a host sequencing fault, and failing the session on it
    /// would end the very turn this exists to keep alive.
    pub fn expire_approval(&mut self, id: &str) -> Result<(), Error> {
        if self.phase != Phase::Running || !self.control.host_expiry {
            return Err(Error::State);
        }
        let approval = self.control.entries.get_mut(id).ok_or(Error::Identity)?;
        if !matches!(approval.stage, Stage::Waiting) {
            return Err(Error::State);
        }
        let now = Instant::now();
        if now < approval.owner.ok_or(Error::State)?
            || now >= approval.response.ok_or(Error::State)?
        {
            return Err(Error::Timeout);
        }
        approval.stage = Stage::Expired;
        Ok(())
    }
    /// Whether this prepared frame may still be sent: Octos has not settled
    /// its approval and the session still runs. A read only.
    pub fn prepared_admissible(&self, prepared: &PreparedApproval) -> bool {
        self.phase == Phase::Running
            && Arc::ptr_eq(&self.source, &prepared.source)
            && self.control.entries.get(&prepared.id).is_some_and(|approval| {
                matches!(approval.stage, Stage::Prepared(sequence) if sequence == prepared.sequence)
            })
    }
    pub fn approval_deadline(&self, id: &str) -> Result<Instant, Error> {
        self.control
            .entries
            .get(id)
            .and_then(|approval| approval.owner)
            .ok_or(Error::Identity)
    }
    /// Freeze one `approval/respond` with scope `request`, so Octos records no
    /// rule of its own. The Host must hold this value before awaiting durable
    /// response-begin and compare the exact original grant before sending.
    pub fn prepare_approval(
        &mut self,
        id: &str,
        decision: PermissionDecision,
    ) -> Result<PreparedApproval, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = operation.driver.prepare_inner(id, decision);
        operation.finish(result)
    }
    fn prepare_inner(
        &mut self,
        id: &str,
        decision: PermissionDecision,
    ) -> Result<PreparedApproval, Error> {
        if self.control.policy.is_none() {
            return Err(Error::State);
        }
        let until = self.control.deadline(&self.wire)?;
        self.wire.check(until)?;
        let session = self.session_id.clone().ok_or(Error::State)?;
        let approval = self.control.entries.get_mut(id).ok_or(Error::Identity)?;
        match approval.stage {
            // The owner's path: preparation finishes before the owner bound.
            Stage::Waiting => {
                if Instant::now() >= approval.owner.ok_or(Error::State)? {
                    return Err(Error::Timeout);
                }
            }
            // The host's expiry path: the reserve covers one deny only.
            Stage::Expired if decision == PermissionDecision::Deny => {}
            Stage::Expired => return Err(Error::State),
            Stage::Prepared(_) | Stage::Sent | Stage::Withdrawn => {
                return Err(Error::PermissionUnavailable);
            }
        }
        // A deny returns a failure to the model and the turn continues, as
        // for Codex and Claude Code. Never a `turn`, `session` or `tool` scope.
        let decision = match decision {
            PermissionDecision::Allow => "approve",
            PermissionDecision::Deny => "deny",
        };
        self.next_id = self.next_id.checked_add(1).ok_or(Error::Capacity)?;
        let request = format!("hagency-{}", self.next_id);
        let bytes = crate::octos::request(
            &request,
            "approval/respond",
            json!({"session_id":session,"approval_id":id,"decision":decision,
                "approval_scope":"request"}),
        )?;
        let response_deadline = approval.response.ok_or(Error::State)?;
        self.control.sequence = self
            .control
            .sequence
            .checked_add(1)
            .ok_or(Error::Capacity)?;
        let sequence = self.control.sequence;
        approval.stage = Stage::Prepared(sequence);
        self.control.answers.insert(request, id.to_owned());
        Ok(PreparedApproval {
            source: self.source.clone(),
            id: id.into(),
            frame: io::PreparedFrame::new(sequence, bytes, response_deadline),
            response_deadline,
            sequence,
        })
    }
}
impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, E: AsyncRead + Unpin> SessionDriver<R, W, E> {
    /// Keep the SAME host future pinned across Event returns. Control returns
    /// its output once; never poll a completed future.
    pub async fn next_or_control<F: Future + ?Sized>(
        &mut self,
        control: Pin<&mut F>,
    ) -> Result<ControlUpdate<F::Output>, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = if operation.driver.control.policy.is_none() {
            Err(Error::State)
        } else {
            operation.driver.next_inner(Some(control)).await
        };
        operation.finish(result)
    }
    /// Send only following positive original durable grant/begin/recheck. Each
    /// Event return suspends this exact frame: process it, recheck authority,
    /// then continue this same value. No ordinary read resumes its writer.
    pub async fn send_prepared_approval(
        &mut self,
        prepared: &mut PreparedApproval,
    ) -> Result<PreparedUpdate, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = operation.driver.send_inner(prepared).await;
        operation.finish(result)
    }
    async fn send_inner(
        &mut self,
        prepared: &mut PreparedApproval,
    ) -> Result<PreparedUpdate, Error> {
        if !Arc::ptr_eq(&self.source, &prepared.source) || self.control.policy.is_none() {
            return Err(Error::Identity);
        }
        loop {
            let until = self.control.deadline(&self.wire)?;
            self.wire.check(until)?;
            let approval = self
                .control
                .entries
                .get(&prepared.id)
                .ok_or(Error::Identity)?;
            if !matches!(approval.stage, Stage::Prepared(sequence) if sequence == prepared.sequence)
            {
                return Err(Error::PermissionUnavailable);
            }
            match self.wire.send_prepared(&mut prepared.frame, until).await? {
                io::Controlled::Message(received) => {
                    if let Some(event) = self.receive(received)? {
                        return Ok(PreparedUpdate::Event(event));
                    }
                }
                io::Controlled::Control(receipt) => {
                    self.control
                        .entries
                        .get_mut(&prepared.id)
                        .ok_or(Error::Identity)?
                        .stage = Stage::Sent;
                    return Ok(PreparedUpdate::WriteAccepted(receipt));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(stage: Stage) -> State {
        State {
            entries: BTreeMap::from([(
                "approval-1".to_owned(),
                Approval {
                    stage,
                    owner: None,
                    response: None,
                },
            )]),
            ..State::default()
        }
    }

    /// Octos echoes `approval/decided` once it has read this host's answer,
    /// which can be before the flush is observed. A settlement on an answer
    /// in write custody is not a withdrawal; one before custody is.
    #[test]
    fn native_octos_settlement_never_withdraws_an_answer_in_custody() {
        let mut custody = state(Stage::Prepared(7));
        assert!(!custody.settled("approval-1", Some(7)));
        assert!(matches!(
            custody.entries["approval-1"].stage,
            Stage::Prepared(7)
        ));
        for writing in [None, Some(6)] {
            let mut before = state(Stage::Prepared(7));
            assert!(before.settled("approval-1", writing));
            assert!(matches!(
                before.entries["approval-1"].stage,
                Stage::Withdrawn
            ));
        }
        for stage in [Stage::Waiting, Stage::Expired] {
            let mut open = state(stage);
            assert!(open.settled("approval-1", Some(7)));
        }
        let mut sent = state(Stage::Sent);
        assert!(!sent.settled("approval-1", None));
        assert!(!state(Stage::Withdrawn).settled("approval-1", None));
        assert!(!State::default().settled("approval-1", None));
    }
}
