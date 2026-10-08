//! Wire mechanics only. Original durable owner grants remain the Host's duty.
use super::{Error, Message, Operation, Phase, SessionDriver, WriteProgress, io};
use serde_json::{Value, json};
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::Instant,
};

const MAX_CALLBACKS: usize = 16;
const MAX_INPUT_BYTES: usize = 64 * 1024;

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
    Message(Message),
    Control(T),
}
pub enum PreparedUpdate {
    Message(Message),
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
    pub fn request_id(&self) -> &str {
        &self.id
    }
    pub fn response_deadline(&self) -> Instant {
        self.response_deadline
    }
}
enum Stage {
    Waiting,
    /// The owner wait ran out unanswered and the host took the callback over
    /// (the ADR046 amendment, as for Codex). It grants nothing and renews no
    /// clock; the only frame it can produce is the deny, inside the response
    /// reserve fixed at admission.
    Expired,
    Prepared(u64),
    Sent,
    Cancelled,
}
struct Callback {
    stage: Stage,
    input: Option<Value>,
    owner: Option<Instant>,
    response: Option<Instant>,
}
#[derive(Default)]
pub(super) struct State {
    policy: Option<ApprovalControlPolicy>,
    /// The host answers an unanswered callback at its owner bound instead of
    /// letting the session time out there.
    host_expiry: bool,
    entries: BTreeMap<String, Callback>,
    read_deadline: Option<Instant>,
    sequence: u64,
}
impl State {
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn observed(&mut self) {
        self.read_deadline = None;
    }
    pub fn admit<R, W, E>(
        &mut self,
        message: &Message,
        received: Instant,
        wire: &io::Wire<R, W, E>,
    ) -> Result<(), Error> {
        let Message::Permission {
            request_id, input, ..
        } = message
        else {
            return Err(Error::State);
        };
        if self.entries.contains_key(request_id) {
            return Err(Error::Identity);
        }
        if self.entries.len() >= MAX_CALLBACKS {
            return Err(Error::Capacity);
        }
        let (input, owner, response) = if let Some(policy) = self.policy {
            // Input is already a one-MiB/depth-bounded decoded JSON object.
            // This tighter limit bounds all16 retained originals and responses.
            if serde_json::to_vec(input)
                .map_err(|_| Error::Capacity)?
                .len()
                > MAX_INPUT_BYTES
            {
                return Err(Error::Capacity);
            }
            let (owner, response) = wire.permission_deadlines(
                received,
                policy.owner_wait_ms,
                policy.response_reserve_ms,
            )?;
            (Some(input.clone()), Some(owner), Some(response))
        } else {
            (None, None, None)
        };
        self.entries.insert(
            request_id.clone(),
            Callback {
                stage: Stage::Waiting,
                input,
                owner,
                response,
            },
        );
        Ok(())
    }
    pub fn cancel(&mut self, id: &str) -> Result<(), Error> {
        let callback = self.entries.get_mut(id).ok_or(Error::Identity)?;
        if matches!(callback.stage, Stage::Cancelled) {
            return Err(Error::Identity);
        }
        callback.stage = Stage::Cancelled;
        callback.input = None;
        Ok(())
    }
    pub fn deadline<R, W, E>(&mut self, wire: &io::Wire<R, W, E>) -> Result<Instant, Error> {
        if self.policy.is_none() {
            return Ok(wire.event_deadline());
        }
        let until = self
            .entries
            .values()
            .filter_map(|callback| match callback.stage {
                // A host that expires callbacks needs the session readable
                // through the owner bound, or the wait spanning it would end
                // the session before the host could act. A read bound only: an
                // allow prepared after the owner bound is still refused.
                Stage::Waiting if self.host_expiry => callback.response,
                Stage::Waiting => callback.owner,
                Stage::Expired | Stage::Prepared(_) => callback.response,
                Stage::Sent | Stage::Cancelled => None,
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
    /// Explicit host opt-in after original stream identity binding and before
    /// any callback is consumed. Does not authorize an allow or durable response.
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
    /// The host will answer an unanswered callback at its owner bound with a
    /// deny (`expire_approval`). Without this the session times out at the
    /// owner bound, as before. Grants nothing.
    pub fn enable_owner_wait_expiry(&mut self) -> Result<(), Error> {
        if self.phase != Phase::Running || self.control.policy.is_none() {
            return Err(Error::State);
        }
        self.control.host_expiry = true;
        Ok(())
    }
    /// Hand one unanswered callback from the owner to the host at its owner
    /// bound. Writes no byte and renews no clock. Not wrapped in `Operation`:
    /// an error here is a host sequencing fault, and failing the session on it
    /// would end the very turn this exists to keep alive.
    pub fn expire_approval(&mut self, id: &str) -> Result<(), Error> {
        if self.phase != Phase::Running || !self.control.host_expiry {
            return Err(Error::State);
        }
        let callback = self.control.entries.get_mut(id).ok_or(Error::Identity)?;
        if !matches!(callback.stage, Stage::Waiting) {
            return Err(Error::State);
        }
        let now = Instant::now();
        // Before the owner bound the owner is still deciding; at or after the
        // response bound there is no margin left to answer in.
        if now < callback.owner.ok_or(Error::State)?
            || now >= callback.response.ok_or(Error::State)?
        {
            return Err(Error::Timeout);
        }
        callback.stage = Stage::Expired;
        Ok(())
    }
    /// Whether this prepared frame may still be sent: its callback is neither
    /// cancelled by Claude nor answered, and the turn is still running. A read
    /// only, so a cancelled callback is skipped without failing the session.
    pub fn prepared_admissible(&self, prepared: &PreparedApproval) -> bool {
        self.phase == Phase::Running
            && Arc::ptr_eq(&self.source, &prepared.source)
            && self.control.entries.get(&prepared.id).is_some_and(|callback| {
                matches!(callback.stage, Stage::Prepared(sequence) if sequence == prepared.sequence)
            })
    }
    pub fn approval_deadline(&self, id: &str) -> Result<Instant, Error> {
        self.control
            .entries
            .get(id)
            .and_then(|callback| callback.owner)
            .ok_or(Error::Identity)
    }
    /// Freeze once from the privately retained original input. The Host must
    /// hold this value before awaiting durable response-begin and must compare
    /// the exact original grant before sending. Encoding grants no authority.
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
        let callback = self.control.entries.get_mut(id).ok_or(Error::Identity)?;
        match callback.stage {
            // The owner's path: preparation finishes before the owner bound.
            Stage::Waiting => {
                if Instant::now() >= callback.owner.ok_or(Error::State)? {
                    return Err(Error::Timeout);
                }
            }
            // The host's expiry path: the reserve covers one deny only.
            Stage::Expired if decision == PermissionDecision::Deny => {}
            Stage::Expired => return Err(Error::State),
            Stage::Prepared(_) | Stage::Sent | Stage::Cancelled => {
                return Err(Error::PermissionUnavailable);
            }
        }
        let input = callback.input.take().ok_or(Error::State)?;
        let response = match decision {
            PermissionDecision::Allow => json!({"behavior":"allow","updatedInput":input}),
            // A deny never interrupts (ADR-192 decision 4, amending ADR-156):
            // Claude continues its turn and can say what it could not do, as
            // Codex does after a decline. Only Claude's own turn ends it.
            PermissionDecision::Deny => {
                json!({"behavior":"deny","message":"Permission denied by Hagency."})
            }
        };
        let bytes = crate::claude::encode(json!({"type":"control_response","response":{
            "subtype":"success","request_id":id,"response":response}}))?;
        let response_deadline = callback.response.ok_or(Error::State)?;
        self.control.sequence = self
            .control
            .sequence
            .checked_add(1)
            .ok_or(Error::Capacity)?;
        let sequence = self.control.sequence;
        callback.stage = Stage::Prepared(sequence);
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
    /// Keep the SAME host future pinned across Message returns. Control returns
    /// its output once; never poll a completed future. No public read is raced
    /// or cancelled internally; selection happens at cancellation-safe leaf IO.
    pub async fn next_or_control<F: Future + ?Sized>(
        &mut self,
        control: Pin<&mut F>,
    ) -> Result<ControlUpdate<F::Output>, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = operation.driver.control_inner(control).await;
        operation.finish(result)
    }
    async fn control_inner<F: Future + ?Sized>(
        &mut self,
        control: Pin<&mut F>,
    ) -> Result<ControlUpdate<F::Output>, Error> {
        if self.control.policy.is_none() {
            return Err(Error::State);
        }
        let until = self.control.deadline(&self.wire)?;
        match self.wire.next_or_control(control, until).await? {
            io::Controlled::Message(received) => {
                self.observe(&received.message, received.at)?;
                Ok(ControlUpdate::Message(received.message))
            }
            io::Controlled::Control(output) => Ok(ControlUpdate::Control(output)),
        }
    }
    /// Send only following positive original durable grant/begin/recheck. Each
    /// Message return suspends this exact frame: process it, recheck authority,
    /// then continue this same value. No ordinary read resumes its writer.
    pub async fn send_prepared_approval(
        &mut self,
        prepared: &mut PreparedApproval,
    ) -> Result<PreparedUpdate, Error> {
        // The peer can read the whole response and end its turn before this
        // host observes its own flush. A write whose bytes have all left only
        // waits for that flush, so it may complete after the Result was
        // observed; it transmits nothing new. Every other send still needs a
        // running turn.
        let expected = if self.phase == Phase::ResultObserved
            && self.wire.awaits_flush_only(&prepared.frame)
        {
            Phase::ResultObserved
        } else {
            Phase::Running
        };
        let operation = Operation::new(self, expected)?;
        let result = operation.driver.send_inner(prepared).await;
        operation.finish(result)
    }
    async fn send_inner(
        &mut self,
        prepared: &mut PreparedApproval,
    ) -> Result<PreparedUpdate, Error> {
        if !Arc::ptr_eq(&self.source, &prepared.source) {
            return Err(Error::Identity);
        }
        if self.control.policy.is_none() {
            return Err(Error::State);
        }
        let until = self.control.deadline(&self.wire)?;
        self.wire.check(until)?;
        let callback = self
            .control
            .entries
            .get(&prepared.id)
            .ok_or(Error::Identity)?;
        if !matches!(callback.stage,Stage::Prepared(sequence) if sequence==prepared.sequence) {
            return Err(Error::PermissionUnavailable);
        }
        match self.wire.send_prepared(&mut prepared.frame, until).await? {
            io::Controlled::Message(received) => {
                self.observe(&received.message, received.at)?;
                Ok(PreparedUpdate::Message(received.message))
            }
            io::Controlled::Control(receipt) => {
                self.control
                    .entries
                    .get_mut(&prepared.id)
                    .ok_or(Error::Identity)?
                    .stage = Stage::Sent;
                Ok(PreparedUpdate::WriteAccepted(receipt))
            }
        }
    }
}
