//! The owner gate on a plugin runtime's relayed coordination calls (ADR-196).
//! A plugin's `tool/call` for a coordination tool that reaches another session
//! is not run on the host's word: it becomes one durable owner request
//! (`plugin/tool`, Approve once or Deny only), the same store protocol the
//! Octos and Claude coordinators drive (`octos.rs`, `claude.rs`): request,
//! owner card, wait for the persisted choice (an unanswered request is denied
//! at its owner bound), authorize, begin, recheck. Only an approved call
//! reaches the task helper, and the helper's answer is then recorded. A denied
//! or expired call returns a failed `tool/result` and the helper never hears
//! of it. The run's parked-capacity reservation lives in the shared
//! `ApprovalRun`, so the host's stop paths release it as for any approval.
use super::{
    ApprovalNotice, observe_applied,
    state::{ApprovalRun, wall_now},
};
use crate::{AuthoritySite, Failure};
use hagency_core::{
    approvals::{
        ApprovalApplication, ApprovalChoice, ApprovalRpcId, HostApprovalContext,
        HostApprovalRequest,
    },
    execution::PLUGIN_TOOL_METHOD,
    tasks::RunnerCapability,
};
use hagency_runtime::task_mcp::COORDINATION_TOOLS;
use hagency_store::{ApprovalResponseGrant, ApprovalResponseObservation, DomainStore};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;

/// Coordination tools that reach another session need the owner on every call.
/// `comment_task` is ADR-021 task maintenance, pre-approved as for Codex.
pub(crate) fn gated(tool: &str) -> bool {
    tool != "comment_task" && COORDINATION_TOOLS.contains(&tool)
}

/// Why a gated call did not reach the helper. Every case sent nothing.
enum Refusal {
    Denied,
    Expired,
    Failed(&'static str),
}
impl Refusal {
    fn message(self, tool: &str) -> String {
        match self {
            Self::Denied => format!(
                "{tool} was refused by the owner; nothing was sent. The refusal is final for \
                 this call_id: report it instead of repeating the call."
            ),
            Self::Expired => format!(
                "{tool} was not approved before the owner wait ended; nothing was sent. \
                 Report it instead of repeating the call."
            ),
            Self::Failed(reason) => {
                format!("{tool} could not be approved ({reason}); nothing was sent.")
            }
        }
    }
}
impl From<Failure> for Refusal {
    fn from(failure: Failure) -> Self {
        Self::Failed(match failure {
            Failure::Deadline => "the approval deadline",
            Failure::ApprovalCapacity => "approval capacity is full",
            Failure::LostAuthority { .. } => "the dispatch authority changed",
            _ => "the approval could not be recorded",
        })
    }
}

/// An owner-approved call that has begun its answer and not yet run.
pub(crate) struct Permit {
    grant: ApprovalResponseGrant,
    application: ApprovalApplication,
}

/// One dispatch's gate. It borrows the run's approval state for the dispatch.
pub(crate) struct PluginGate<'a> {
    run: &'a mut ApprovalRun,
    domain: &'a DomainStore,
    cap: &'a RunnerCapability,
    expected: &'a str,
    context: HostApprovalContext,
    /// The bound of every wait, and the context's wall-clock end.
    until: Instant,
    expires_at: u64,
    /// True while the dispatch is parked on the owner. The dispatch lease is
    /// renewed by `maintain_owned_approval` then, and the plain renewal, which
    /// only accepts a started dispatch, stands down.
    waiting: Arc<AtomicBool>,
    next: u64,
}
impl<'a> PluginGate<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        run: &'a mut ApprovalRun,
        domain: &'a DomainStore,
        cap: &'a RunnerCapability,
        expected: &'a str,
        context: HostApprovalContext,
        until: Instant,
        expires_at: u64,
        waiting: Arc<AtomicBool>,
    ) -> Self {
        Self {
            run,
            domain,
            cap,
            expected,
            context,
            until,
            expires_at,
            waiting,
            next: 0,
        }
    }

    /// Ask the owner for one call. `Ok` is permission to run it once; `Err` is
    /// the failed tool result's message, with nothing sent.
    pub(crate) async fn authorize(&mut self, tool: &str, input: &Value) -> Result<Permit, String> {
        self.waiting.store(true, Ordering::Release);
        let outcome = self.request(tool, input).await;
        self.waiting.store(false, Ordering::Release);
        outcome.map_err(|refusal| refusal.message(tool))
    }

    async fn request(&mut self, tool: &str, input: &Value) -> Result<Permit, Refusal> {
        let (domain, cap) = (self.domain, self.cap);
        if self.run.scope.is_none() {
            self.run.callbacks.context = Some(self.context.clone());
            self.run.scope = Some(
                domain
                    .bind_owned_approval_context(
                        cap.clone(),
                        self.expected.into(),
                        self.context.clone(),
                        self.until.into_std(),
                        self.expires_at,
                    )
                    .await
                    .map_err(|error| Failure::lost(AuthoritySite::ApprovalBind, &error))?,
            );
        }
        let policy = self.run.callbacks.host.octos_policy();
        let (owner_wait, reserve) = (
            Duration::from_millis(policy.owner_wait_ms),
            Duration::from_millis(policy.response_reserve_ms),
        );
        let owner_deadline = Instant::now() + owner_wait;
        let response_deadline = owner_deadline + reserve;
        if response_deadline > self.until {
            return Err(Failure::Deadline.into());
        }
        let wall = wall_now()?;
        let owner_expires_at = wall
            .checked_add(policy.owner_wait_ms)
            .ok_or(Failure::Deadline)?;
        let expires_at = owner_expires_at
            .checked_add(policy.response_reserve_ms)
            .ok_or(Failure::Deadline)?;
        self.next += 1;
        let item = format!("plugin_call_{}", self.next);
        let request = HostApprovalRequest {
            context_id: self.context.id.clone(),
            upstream_id: ApprovalRpcId::String(item.clone()),
            item_id: item.clone(),
            method: PLUGIN_TOOL_METHOD.into(),
            params: json!({"threadId":self.context.thread_id,"turnId":self.context.turn_id,
                "itemId":item,"toolName":tool,"input":input}),
            expires_at,
        };
        if self.run.callbacks.parked.is_none() {
            self.run.callbacks.parked = Some(self.run.callbacks.host.reserve_parked()?);
        }
        // The reservation is retained before the request, as for Octos.
        self.run
            .callbacks
            .parked
            .as_mut()
            .ok_or(Failure::ApprovalCapacity)?
            .possible();
        let summary = domain
            .request_owner_approval(cap.clone(), request)
            .await
            .map_err(|error| Failure::lost(AuthoritySite::ApprovalRequest, &error))?;
        let id = summary.id.clone();
        // A matching grant would decide it already; this method derives none.
        if summary.choice.is_none() {
            self.run
                .callbacks
                .notices
                .as_ref()
                .ok_or(Failure::ApprovalCapacity)?
                .try_send(ApprovalNotice {
                    request_id: id.clone(),
                    owner_expires_at,
                })
                .map_err(|_| Failure::ApprovalCapacity)?;
        }
        // Wait for the owner's persisted choice. An unanswered request is
        // denied by the host at its owner bound, then read like any choice.
        let mut expired = false;
        let choice = loop {
            if !expired && Instant::now() >= owner_deadline {
                match domain
                    .deny_for_owner_wait_expiry(id.clone(), owner_expires_at)
                    .await
                {
                    // `State`: the owner decided first; read below.
                    Ok(_) | Err(hagency_store::Error::State) => {}
                    Err(error) => {
                        return Err(Failure::lost(AuthoritySite::ApprovalExpiry, &error).into());
                    }
                }
                expired = true;
            }
            let scope = self.run.scope.as_ref().ok_or(Failure::Admission)?;
            let status = domain
                .maintain_owned_approval(scope)
                .await
                .map_err(|error| Failure::lost(AuthoritySite::ApprovalMaintain, &error))?;
            if let Some(choice) = status
                .approvals
                .iter()
                .find(|approval| approval.id == id)
                .and_then(|approval| approval.choice)
            {
                break choice;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let allow = choice != ApprovalChoice::Deny;
        if allow && Instant::now() >= owner_deadline {
            return Err(Failure::Deadline.into());
        }
        let grant = domain
            .authorize_approval_response(cap.clone(), id.clone())
            .await
            .map_err(|error| Failure::lost(AuthoritySite::ApprovalResponse, &error))?;
        let application = grant.application().clone();
        if application.id != id
            || application.connection_id != self.context.connection_id
            || application.upstream_id != ApprovalRpcId::String(item.clone())
            || application.thread_id != self.context.thread_id
            || application.turn_id != self.context.turn_id
            || application.item_id != item
            || application.allow != allow
        {
            return Err(Failure::lost_io(AuthoritySite::ApprovalApplication).into());
        }
        let mut grants = [grant];
        domain
            .begin_approval_responses(cap.clone(), &mut grants, response_deadline.into_std())
            .await
            .map_err(|error| Failure::lost(AuthoritySite::ApprovalBegin, &error))?;
        let [mut grant] = grants;
        domain
            .check_approval_response(cap.clone(), &grant)
            .await
            .map_err(|error| Failure::lost(AuthoritySite::ApprovalCheck, &error))?;
        if allow {
            return Ok(Permit { grant, application });
        }
        // Denied: the answer is final here and nothing is sent, so its
        // acceptance and its taking effect are recorded now.
        self.record(&mut grant, &application, false, "plugin tool refused")
            .await;
        Err(if expired {
            Refusal::Expired
        } else {
            Refusal::Denied
        })
    }

    /// The approved call ran: record the helper's answer and free the
    /// reservation. `unknown` is a helper that stopped answering, whose effect
    /// cannot be told: the dispatch is parked and keeps its reservation.
    pub(crate) async fn answered(&mut self, mut permit: Permit, unknown: bool) {
        self.record(
            &mut permit.grant,
            &permit.application,
            unknown,
            "plugin tool answered",
        )
        .await;
    }

    async fn record(
        &mut self,
        grant: &mut ApprovalResponseGrant,
        application: &ApprovalApplication,
        unknown: bool,
        evidence: &str,
    ) {
        let observation = if unknown {
            ApprovalResponseObservation::OutcomeUnknown
        } else {
            ApprovalResponseObservation::WriteAccepted
        };
        if let Err(error) = self
            .domain
            .observe_approval_response(grant, observation)
            .await
        {
            tracing::warn!(approval = %application.id, error = ?error,
                "plugin tool approval response not recorded");
        }
        if unknown {
            return;
        }
        observe_applied(self.domain, application, evidence).await;
        if let Some(parked) = &mut self.run.callbacks.parked {
            parked.release();
        }
        self.run.callbacks.parked = None;
    }
}
