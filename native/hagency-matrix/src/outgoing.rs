pub(crate) mod state;
use crate::collector::observe;
use crate::{CancellationToken, Collector, Error, collector::Inner, sdk::Owner};
use hagency_core::{commands::CommandNoticeClaimed, ingress::VerifiedNoticeClaim, replies::*};
use hagency_matrix_format::MatrixContent;
use serde_json::{Value, json};
use state::{Attempt, Command, Kind, Phase, Write};
use std::{collections::BTreeSet, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutgoingState {
    Idle,
    Delivered,
    Uncertain,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingSummary {
    pub id: Option<String>,
    pub state: OutgoingState,
    pub replayed: bool,
}
pub(crate) enum Source {
    Final(ReplyClaim),
    Notice(Box<VerifiedNoticeClaim>),
    Command(Box<CommandNoticeClaimed>),
    File(Box<crate::upload::publication::FileSource>),
    Resume,
}

/// Parse an activity notice's kind into its dispatch id and, when the
/// send should EDIT an earlier revision, the anchor event id. The kind is
/// `activity:<dispatch_id>:<revision>[:<anchor>]` (task #1): dispatch ids
/// are colon-free (`identifier()`), the revision is numeric, and the
/// anchor — a Matrix event id — may itself contain colons, so it is taken
/// as the remainder. Every other notice kind parses to None.
fn parse_activity_notice(kind: &str) -> Option<(String, Option<String>)> {
    let rest = kind.strip_prefix("activity:")?;
    let (dispatch, rest) = rest.split_once(':')?;
    let anchor = rest
        .split_once(':')
        .map(|(_revision, anchor)| anchor.to_owned());
    Some((dispatch.to_owned(), anchor))
}

/// The activity envelope (lib/matrix-activity.js:4-13): the notice always
/// carries `io.hagency.activity: {dispatch_id}`; when an anchor exists the
/// send becomes an EDIT — body prefixed `* `, `m.new_content` the plain
/// content, `m.relates_to` the replace relation (which replaces the thread
/// relation, exactly as the TS override does).
fn apply_activity_envelope(mut content: Value, dispatch_id: &str, anchor: Option<&str>) -> Value {
    content["io.hagency.activity"] = json!({"dispatch_id": dispatch_id});
    if let Some(anchor) = anchor {
        let plain = content.clone();
        let starred = format!("* {}", content["body"].as_str().unwrap_or_default());
        content["body"] = json!(starred);
        content["m.new_content"] = plain;
        content["m.relates_to"] = json!({"rel_type":"m.replace","event_id":anchor});
    }
    content
}
impl Collector {
    /// Existing host claim only. No caller-selected Matrix path or content.
    pub async fn send_final(
        &self,
        claim: ReplyClaim,
        cancel: &CancellationToken,
    ) -> Result<OutgoingSummary, Error> {
        self.outgoing_job(Source::Final(claim), cancel).await
    }
    pub async fn send_notice(
        &self,
        claim: VerifiedNoticeClaim,
        cancel: &CancellationToken,
    ) -> Result<OutgoingSummary, Error> {
        self.outgoing_job(Source::Notice(Box::new(claim)), cancel)
            .await
    }
    /// Answer a `!` command line in the room that carried it, as this agent.
    /// Same custody and retry discipline as a final reply: the claim is
    /// host-minted, one send per claim, and an unknown outcome is inspected
    /// rather than re-sent (`lib/bot-commands.js` `reply`/`sendInto`).
    pub async fn send_command_notice(
        &self,
        claimed: CommandNoticeClaimed,
        cancel: &CancellationToken,
    ) -> Result<OutgoingSummary, Error> {
        self.outgoing_job(Source::Command(Box::new(claimed)), cancel)
            .await
    }
    /// Settles journaled acceptance. Uncertain/prepared work is inspect-only;
    /// reopening this method never starts another HTTP write.
    pub async fn resume_outgoing_custody(
        &self,
        cancel: &CancellationToken,
    ) -> Result<OutgoingSummary, Error> {
        self.outgoing_job(Source::Resume, cancel).await
    }
    async fn outgoing_job(
        &self,
        source: Source,
        cancel: &CancellationToken,
    ) -> Result<OutgoingSummary, Error> {
        let permit = self
            .inner
            .busy
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let inner = self.inner.clone();
        let cancel = cancel.child_token();
        let resuming = matches!(source, Source::Resume);
        #[cfg(test)]
        let observation = crate::collector::observation::current();
        let job = async move {
            let _permit = permit;
            // Task #9 (ADR-183, TS pollOneRouterOutbox bridge-matrix.js:6036-
            // 6055): a resume re-sends the journaled write with the SAME
            // transaction id once. A first send stays bounded at 45 s and parks
            // in WritePossible for the next resume to finish. The retry loop is
            // NOT here: TS polls the outbox again on a later pass, and this
            // collector's resume is likewise one pass. A non-permanent failure
            // (lost connection, timeout, 429 that outlived its in-request
            // tries, 5xx) must not loop in-process — the send stays `sending`,
            // so the resume parks it `Uncertain` for the next pass to re-send.
            // A permanent verdict (4xx other than 429) or a local refusal
            // surfaces to the caller.
            if resuming {
                match inner.outgoing(Source::Resume, &cancel).await {
                    Ok(summary) => Ok(summary),
                    Err(Error::Cancelled) => Err(Error::Cancelled),
                    Err(error) if retryable(&error) => Ok(OutgoingSummary {
                        id: None,
                        state: OutgoingState::Uncertain,
                        replayed: false,
                    }),
                    Err(error) => Err(error),
                }
            } else {
                let work = inner.outgoing(source, &cancel);
                tokio::pin!(work);
                tokio::select! {
                    result = &mut work => result,
                    _ = tokio::time::sleep(Duration::from_secs(45)) => {
                        cancel.cancel();
                        // Accepted results still finish bounded SDK journal custody.
                        work.await
                    },
                }
            }
        };
        #[cfg(test)]
        let job = crate::collector::observation::owned(observation, job);
        tokio::spawn(job).await.map_err(|_| Error::OutcomeUnknown)?
    }
}
/// Task #9 (TS `isPermanentRouterMatrixFailure`, bridge-matrix.js:6029-6033):
/// did this write attempt end in a failure the retained product re-sends? A
/// permanent Matrix verdict is an HTTP 4xx other than 429 (`M_FORBIDDEN` 403,
/// `M_BAD_JSON` 400, `M_NOT_FOUND` 404, `401`); it surfaces to the caller and
/// stops the retry loop, exactly as TS posts it to `../failed`. Everything
/// else about the transport — a connection that never dialled, a timeout, a
/// 429 that outlived its in-request tries, a 5xx, or a 200 the client could
/// not accept — is non-permanent and is re-sent with the same transaction id,
/// which Matrix dedups. A local custody fault (storage, SDK poisoning,
/// generation, recipients) is not classified here: it surfaces, and the
/// driver parks it as an unknown outcome just as a parked retry would.
fn retryable(error: &Error) -> bool {
    match error {
        Error::Transport
        | Error::Timeout
        | Error::InvalidJson
        | Error::Wire
        | Error::Headers
        | Error::BodyTooLarge
        | Error::Redirect => true,
        Error::Remote(status) => *status == 429 || !(400..=499).contains(status),
        Error::Unauthorized => false,
        _ => false,
    }
}
/// Task #9 (TS `isPermanentRouterMatrixFailure`, bridge-matrix.js:6029-6033):
/// a permanent Matrix verdict is an HTTP 4xx other than 429 — `M_FORBIDDEN`
/// (403), `M_BAD_JSON` (400), `M_NOT_FOUND` (404), 401. Only this exact verdict
/// is journalled as permanent; a cancellation, a local custody fault or a
/// transport outcome is not, so it stays re-sendable.
fn permanent_refusal(error: &Error) -> bool {
    matches!(error, Error::Unauthorized)
        || matches!(error, Error::Remote(status) if (400..=499).contains(status) && *status != 429)
}
impl Inner {
    pub(crate) async fn outgoing(
        &self,
        mut source: Source,
        cancel: &CancellationToken,
    ) -> Result<OutgoingSummary, Error> {
        observe!(OwnerLock);
        let mut guard = self.owner.lock().await;
        if guard.is_none() {
            // Resume uses protected old identity/journal solely for receipt
            // recovery; it cannot restore transport observations or send.
            if matches!(source, Source::Resume) {
                observe!(OpenOwner);
                *guard = Some(Owner::open_existing(&self.config).await?);
            } else {
                let expected = self.expected_transport().await?;
                observe!(Whoami);
                if let Err(error) = self.whoami(cancel).await {
                    #[cfg(test)]
                    crate::collector::observation::primary(error.clone());
                    return self.fence_observation(expected, error).await;
                }
                observe!(OpenOwner);
                *guard = Some(Owner::open(&self.config).await?);
            }
        }
        let owner = guard.as_ref().ok_or(Error::Storage)?;
        let view = owner.outgoing(Command::Read).await?;
        if let Source::Resume = source {
            // A journaled send the host itself has since cancelled is no longer
            // wanted by anyone. Re-sending it would be the silent redo ADR-059
            // forbids, and parking it forever blocks every later send of this
            // agent (its replies included). Release the slot instead: nothing
            // is re-sent, nothing is recorded as delivered, and the domain row
            // keeps its own `uncertain`/`cancelled` word as the evidence.
            if let Some(attempt) = &view.attempt
                && attempt.phase != Phase::Complete
                && self.abandon_cancelled(owner, attempt).await?
            {
                return Ok(OutgoingSummary {
                    id: Some(attempt.id.clone()),
                    state: OutgoingState::Idle,
                    replayed: true,
                });
            }
            return match view.attempt {
                Some(attempt) if attempt.phase == Phase::Complete => {
                    self.settle_outgoing(owner, attempt, true).await
                }
                // Task #9 (TS pollOneRouterOutbox, bridge-matrix.js:6036-6055):
                // a journaled send that never reached Complete is re-sent with
                // the SAME transaction id until the homeserver accepts it —
                // Matrix dedups the replay. Phases whose pending action is a
                // Matrix write (or the idempotent keys query) are resumable;
                // crypto-side uncertainties stay inspect-only, parked for a
                // human, never silently redone.
                Some(attempt)
                    if matches!(
                        attempt.phase,
                        Phase::Ready | Phase::QueryPrepared | Phase::WritePossible
                    ) =>
                {
                    self.resend_outgoing(owner, attempt, cancel).await
                }
                Some(attempt) => Ok(OutgoingSummary {
                    id: Some(attempt.id),
                    state: OutgoingState::Uncertain,
                    replayed: true,
                }),
                None => self.resume_settled_file_publication(owner).await,
            };
        }
        if view.attempt.is_some() {
            return Err(Error::OutcomeUnknown);
        }
        let mut activity: Option<(String, Option<String>)> = None;
        let (kind, id, fence, domain_digest, route, transaction_id, body, reply_to, incidental) =
            match &source {
                Source::Final(claim) => {
                    let historical = owner
                        .outgoing(Command::Lookup {
                            id: claim.id.clone(),
                            fence: claim.fence,
                        })
                        .await?;
                    if let Some(receipt) = historical.receipts.first() {
                        if receipt.kind != Kind::Final {
                            return Err(Error::Conflict);
                        }
                        if self
                            .domain
                            .final_reply_history_conflicts(claim.id.clone(), claim.fence)
                            .await?
                        {
                            return Err(Error::Conflict);
                        }
                        return Ok(OutgoingSummary {
                            id: Some(claim.id.clone()),
                            state: OutgoingState::Delivered,
                            replayed: true,
                        });
                    }
                    observe!(OutgoingPreview);
                    let send = self.domain.preview_final_reply(claim.clone()).await?;
                    (
                        Kind::Final,
                        send.id,
                        claim.fence,
                        send.digest,
                        send.route,
                        send.transaction_id,
                        send.body,
                        send.reply_to,
                        send.incidental,
                    )
                }
                Source::Notice(claim) => {
                    observe!(OutgoingPreview);
                    let receipt = self
                        .domain
                        .verified_notice_receipt(claim.claim.notice.id.clone())
                        .await?;
                    let historical = owner
                        .outgoing(Command::Lookup {
                            id: receipt.id.clone(),
                            fence: receipt.fence,
                        })
                        .await?;
                    if let Some(original) = historical.receipts.first() {
                        if original.kind != Kind::Notice {
                            return Err(Error::Conflict);
                        }
                        if receipt.state != "delivered" {
                            return Err(Error::Conflict);
                        }
                        return Ok(OutgoingSummary {
                            id: Some(receipt.id),
                            state: OutgoingState::Delivered,
                            replayed: true,
                        });
                    }
                    if receipt.state != "claimed" {
                        return Err(Error::Domain("notice_receipt_not_claimed"));
                    }
                    activity = parse_activity_notice(&claim.claim.notice.kind);
                    (
                        Kind::Notice,
                        claim.claim.notice.id.clone(),
                        receipt.fence,
                        claim.digest.clone(),
                        claim.route.clone(),
                        claim.claim.notice.transaction_id.clone(),
                        claim.claim.notice.body.clone(),
                        None,
                        false,
                    )
                }
                Source::Command(claimed) => {
                    observe!(OutgoingPreview);
                    let receipt = self
                        .domain
                        .command_notice_receipt(claimed.claim.notice.id.clone())
                        .await?;
                    let historical = owner
                        .outgoing(Command::Lookup {
                            id: receipt.id.clone(),
                            fence: receipt.fence,
                        })
                        .await?;
                    if let Some(original) = historical.receipts.first() {
                        if original.kind != Kind::Command {
                            return Err(Error::Conflict);
                        }
                        if receipt.state != "delivered" {
                            return Err(Error::Conflict);
                        }
                        return Ok(OutgoingSummary {
                            id: Some(receipt.id),
                            state: OutgoingState::Delivered,
                            replayed: true,
                        });
                    }
                    if receipt.state != "claimed" {
                        return Err(Error::Domain("command_notice_receipt_not_claimed"));
                    }
                    (
                        Kind::Command,
                        claimed.claim.notice.id.clone(),
                        receipt.fence,
                        claimed.digest.clone(),
                        claimed.route.clone(),
                        claimed.claim.notice.transaction_id.clone(),
                        claimed.claim.notice.body.clone(),
                        // A command answer names nobody: it renders from the route's
                        // thread root alone, exactly as the retained bridge sent it.
                        None,
                        false,
                    )
                }
                Source::File(file) => {
                    let l = &file.locator;
                    self.domain
                        .validate_file_publication(file.cap.clone(), file.claim.clone())
                        .await?;
                    (
                        Kind::File,
                        l.delivery_id.clone(),
                        l.fence,
                        l.content_digest.clone(),
                        l.route.clone(),
                        l.transaction_id.clone(),
                        String::new(),
                        None,
                        false,
                    )
                }
                Source::Resume => unreachable!(),
            };
        let joined = self.outgoing_preflight(&route, cancel).await?;
        let draft = if let Source::File(file) = &mut source {
            let mut start = file.start.take().ok_or(Error::Conflict)?;
            start.joined = joined;
            owner
                .outgoing(Command::StartFile(Box::new(start)))
                .await?
                .attempt
                .ok_or(Error::Storage)?
        } else {
            let mut content =
                json!({"msgtype":if kind==Kind::Notice {"m.notice"}else{"m.text"},"body":body});
            // The retained bridge attached the handler's own html rendering
            // (`reply`, :397-404: `format` + `formatted_body` only when that
            // handler had one). A command answer carries it through verbatim —
            // a truthy `formatted_body` is trusted passthrough, never
            // re-rendered (`hagency-matrix-format`).
            if let Source::Command(claimed) = &source
                && let Some(html) = &claimed.claim.notice.html
            {
                content["format"] = json!("org.matrix.custom.html");
                content["formatted_body"] = json!(html);
            }
            if let Some(relation) = state::reply_relation(
                route.thread_root.as_deref(),
                reply_to.as_deref(),
                matches!(route.privacy, hagency_core::replies::RoomPrivacy::Group {}),
                incidental,
            ) {
                content["m.relates_to"] = relation;
            }
            if let Some((dispatch, anchor)) = &activity {
                content = apply_activity_envelope(content, dispatch, anchor.as_deref());
            }
            let content = MatrixContent::new(content)
                .and_then(|c| c.formatted())
                .map_err(|_| Error::Capacity)?
                .into_value();
            let content_digest = state::hash(state::encode(&content, state::MAX_EVENT)?.as_bytes());
            let draft = Attempt {
                kind,
                id: id.clone(),
                fence,
                domain_digest,
                route,
                reply_to,
                incidental,
                transaction_id,
                content,
                content_digest,
                identity: String::new(),
                joined,
                phase: Phase::BeforeBegin,
                query_id: None,
                query_body: None,
                query_response: None,
                keys_digest: None,
                writes: vec![],
                index: 0,
                permanent_failure: false,
                file: None,
            };
            owner
                .outgoing(Command::Start(Box::new(draft.clone())))
                .await?;
            draft
        };
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        observe!(OutgoingBegin);
        match &source {
            Source::Final(claim) => {
                let begun = self.domain.begin_final_reply_send(claim.clone()).await?;
                if begun.id != id
                    || begun.digest != draft.domain_digest
                    || begun.route != draft.route
                    || begun.transaction_id != draft.transaction_id
                    || begun.body != body
                {
                    return Err(Error::Conflict);
                }
            }
            Source::Notice(claim) => {
                let begun = self
                    .domain
                    .begin_verified_task_notice_send(id.clone(), claim.claim.token.clone())
                    .await?;
                if begun.fence != fence
                    || begun.digest != draft.domain_digest
                    || begun.route != draft.route
                    || begun.notice.transaction_id != draft.transaction_id
                    || begun.notice.body != body
                {
                    return Err(Error::Conflict);
                }
            }
            Source::Command(claimed) => {
                let begun = self
                    .domain
                    .begin_command_notice_send(
                        claimed.claim.notice.id.clone(),
                        claimed.claim.token.clone(),
                    )
                    .await?;
                if begun.fence != fence
                    || begun.digest != draft.domain_digest
                    || begun.route != draft.route
                    || begun.notice.transaction_id != draft.transaction_id
                    || begun.notice.body != body
                {
                    return Err(Error::Conflict);
                }
            }
            Source::File(file) => {
                self.domain
                    .validate_file_publication(file.cap.clone(), file.claim.clone())
                    .await?;
            }
            Source::Resume => unreachable!(),
        }
        #[cfg(test)]
        if self
            .outgoing_fault
            .compare_exchange(
                4,
                0,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
        {
            return Err(Error::OutcomeUnknown);
        }
        let mut attempt = owner
            .outgoing(Command::Begun)
            .await?
            .attempt
            .ok_or(Error::Storage)?;
        if attempt.route.encrypted {
            attempt = owner
                .outgoing(Command::Query)
                .await?
                .attempt
                .ok_or(Error::Storage)?;
            observe!(OutgoingQueryHttp);
            let response = self
                .http
                .post(
                    &["_matrix", "client", "v3", "keys", "query"],
                    attempt.query_body.clone().ok_or(Error::Storage)?,
                    cancel,
                )
                .await?
                .success()?;
            attempt = match owner.outgoing(Command::Encrypt(response)).await {
                Ok(view) => view.attempt.ok_or(Error::Storage)?,
                Err(Error::Recipients) => {
                    self.retire_outgoing_room(&attempt.route).await?;
                    return Err(Error::Recipients);
                }
                Err(Error::Identity) => {
                    #[cfg(test)]
                    crate::collector::observation::primary(Error::Identity);
                    return self
                        .fence_observation(self.config.identity.transport.clone(), Error::Identity)
                        .await;
                }
                Err(error) => return Err(error),
            };
        }
        while attempt.index < attempt.writes.len() {
            let joined = self.outgoing_preflight(&attempt.route, cancel).await?;
            if joined != attempt.joined {
                return Err(Error::Generation);
            }
            if attempt.route.encrypted {
                observe!(OutgoingQueryHttp);
                let keys = self
                    .http
                    .post(
                        &["_matrix", "client", "v3", "keys", "query"],
                        attempt.query_body.clone().ok_or(Error::Storage)?,
                        cancel,
                    )
                    .await?
                    .success()?;
                if Some(state::hash(
                    state::encode(&keys, state::MAX_QUERY)?.as_bytes(),
                )) != attempt.keys_digest
                {
                    self.retire_outgoing_room(&attempt.route).await?;
                    return Err(Error::Recipients);
                }
            }
            self.validate_outgoing(&source, attempt.fence).await?;
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let index = attempt.index;
            let write = attempt.writes[index].clone();
            // From this durable point onward, even connect errors/timeouts are
            // uncertain. A stable transaction ID alone is not retry authority.
            owner.outgoing(Command::Possible(index)).await?;
            #[cfg(test)]
            if self
                .outgoing_fault
                .compare_exchange(
                    5,
                    0,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_ok()
            {
                self.outgoing_reached.notify_one();
                self.outgoing_continue.notified().await;
            }
            // SDK persistence may have queued while a host retired the scope or
            // the lease expired. Recheck after that await, immediately before IO.
            self.validate_outgoing(&source, attempt.fence).await?;
            observe!(OutgoingWriteHttp, Some(index));
            let value = self
                .write_outgoing(owner, &attempt.route, write, cancel)
                .await?;
            // No cancellation gate between actual accepted response and custody.
            attempt = owner
                .outgoing(Command::Accept(index, value))
                .await?
                .attempt
                .ok_or(Error::Storage)?;
            if attempt.phase == Phase::Complete {
                break;
            }
        }
        self.settle_outgoing(owner, attempt, false).await
    }
    /// Task #9 (TS `pollOneRouterOutbox`, bridge-matrix.js:6036-6055): a
    /// journaled send that never reached Complete is re-sent with the SAME
    /// journaled transaction id — Matrix dedups the replay — until it is
    /// delivered. One bounded pass per resume; the caller's poll cadence and
    /// 1 s -> 60 s backoff provide the "until delivered". A pending keys
    /// query is an idempotent read and is replayed first. `ResponseStored`
    /// (an accepted to-device leg awaiting local olm marking) stays
    /// host-inspection custody: its journal continuation is not a Matrix
    /// write and no command replays it.
    async fn resend_outgoing(
        &self,
        owner: &Owner,
        mut attempt: Attempt,
        cancel: &CancellationToken,
    ) -> Result<OutgoingSummary, Error> {
        // Task #9 (TS pollOneRouterOutbox, bridge-matrix.js:6036-6055): the
        // journaled write already carries its final ciphertext (or plaintext)
        // and its transaction id, so a resume re-sends it with the SAME txn id
        // and nothing else — no preflight and no keys re-query, which
        // `collect()` already re-verified before this resume. The one
        // genuinely resumable crypto state is QueryPrepared, where the keys
        // query was prepared but its response never reached Encrypt: re-fetch
        // it (an idempotent read), then fall through to the re-PUT. A keys or
        // recipients uncertainty (CryptoApplying / Quarantined) is never
        // resumed here; it stays parked for the human.
        if attempt.phase == Phase::QueryPrepared {
            observe!(OutgoingQueryHttp);
            let response = self
                .http
                .post(
                    &["_matrix", "client", "v3", "keys", "query"],
                    attempt.query_body.clone().ok_or(Error::Storage)?,
                    cancel,
                )
                .await?
                .success()?;
            attempt = match owner.outgoing(Command::Encrypt(response)).await {
                Ok(view) => view.attempt.ok_or(Error::Storage)?,
                Err(Error::Recipients) => {
                    self.retire_outgoing_room(&attempt.route).await?;
                    return Err(Error::Recipients);
                }
                Err(error) => return Err(error),
            };
        }
        // Task #9: before re-putting, re-check that the journaled send is still
        // authorized. A resume no longer holds the claim secret, so it asks the
        // domain whether the row is still `sending` and uncancelled. A retirement
        // or operator cancellation already moved it off `sending` (parked as
        // `uncertain` for a human), so re-sending it would be exactly the
        // silent redo ADR-059 forbids: park it instead. A file send keeps its
        // own custody — an uncertain publication is settled by the publication
        // pipeline from its receipts, never re-put here, so it parks too.
        let send_current = match attempt.kind {
            Kind::Final => {
                self.domain
                    .final_reply_send_current(attempt.id.clone(), attempt.fence)
                    .await?
            }
            Kind::Notice => {
                self.domain
                    .verified_notice_send_current(attempt.id.clone(), attempt.fence)
                    .await?
            }
            Kind::Command => {
                self.domain
                    .command_notice_send_current(attempt.id.clone(), attempt.fence)
                    .await?
            }
            Kind::File => false,
        };
        if !send_current {
            return Ok(OutgoingSummary {
                id: Some(attempt.id),
                state: OutgoingState::Uncertain,
                replayed: true,
            });
        }
        // Task #9 (TS `isPermanentRouterMatrixFailure`, bridge-matrix.js:6029-6033):
        // a write that already ended in a permanent refusal (an HTTP 4xx other than
        // 429) is not re-put. TS posts such a command to `../failed` and stops
        // retrying forever; the journaled mark is that stop, so a resume parks the
        // send visibly for a human instead of replaying a verdict that will not
        // change.
        if attempt.permanent_failure {
            return Ok(OutgoingSummary {
                id: Some(attempt.id),
                state: OutgoingState::Uncertain,
                replayed: true,
            });
        }
        while attempt.index < attempt.writes.len() {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let index = attempt.index;
            let write = attempt.writes[index].clone();
            // Task #9 re-sends the room outbox (write.room), which Matrix
            // dedups by transaction id. A to-device write is the olm key
            // share: once it is durably WritePossible its send outcome is
            // uncertain and must never be silently redone (ADR-059), so park
            // it for a human instead of re-sending it.
            if !write.room && attempt.phase == Phase::WritePossible {
                return Ok(OutgoingSummary {
                    id: Some(attempt.id),
                    state: OutgoingState::Uncertain,
                    replayed: true,
                });
            }
            // A resumed WritePossible is already durably marked; a resumed
            // Ready is marked now, exactly like a first send.
            if attempt.phase == Phase::Ready {
                owner.outgoing(Command::Possible(index)).await?;
            }
            observe!(OutgoingWriteHttp, Some(index));
            let value = self
                .write_outgoing(owner, &attempt.route, write, cancel)
                .await?;
            // No cancellation gate between actual accepted response and custody.
            attempt = owner
                .outgoing(Command::Accept(index, value))
                .await?
                .attempt
                .ok_or(Error::Storage)?;
            if attempt.phase == Phase::Complete {
                break;
            }
        }
        self.settle_outgoing(owner, attempt, true).await
    }
    async fn validate_outgoing(&self, source: &Source, fence: u64) -> Result<(), Error> {
        observe!(OutgoingValidate);
        match source {
            Source::Final(claim) => self.domain.validate_final_reply_send(claim.clone()).await?,
            Source::Notice(claim) => {
                self.domain
                    .validate_verified_task_notice_send(
                        claim.claim.notice.id.clone(),
                        claim.claim.token.clone(),
                        fence,
                    )
                    .await?
            }
            Source::Command(claimed) => {
                self.domain
                    .validate_command_notice_send(
                        claimed.claim.notice.id.clone(),
                        claimed.claim.token.clone(),
                        fence,
                    )
                    .await?
            }
            Source::File(file) => {
                self.domain
                    .validate_file_publication(file.cap.clone(), file.claim.clone())
                    .await?;
            }
            Source::Resume => return Err(Error::OutcomeUnknown),
        }
        Ok(())
    }
    async fn outgoing_preflight(
        &self,
        route: &ReplyRoute,
        cancel: &CancellationToken,
    ) -> Result<BTreeSet<String>, Error> {
        let t = &self.config.identity.transport;
        if route.engagement_id != t.engagement_id
            || route.registration_generation != t.registration_generation
            || route.transport_generation != t.generation
            || route.sender_mxid != t.sender_mxid
            || route.device_id != t.device_id
            || route.server_name != self.config.identity.server_name
        {
            return Err(Error::Generation);
        }
        let rooms = self.host_rooms();
        let target = rooms
            .iter()
            .find(|r| r.room_id == route.room_id && r.privacy == route.privacy)
            .ok_or(Error::Generation)?;
        if self.observed_room_generation(target).await? != route.room_generation {
            return Err(Error::Generation);
        }
        let expected = self.expected_transport().await?;
        observe!(Whoami);
        if let Err(error) = self.whoami(cancel).await {
            #[cfg(test)]
            crate::collector::observation::primary(error.clone());
            return self.fence_observation(expected, error).await;
        }
        let observation = self.collect_room_observation(target, cancel).await?;
        if observation.generation != route.room_generation {
            return Err(Error::Generation);
        }
        if observation.encrypted != route.encrypted
            || observation.joined.len() > 16
            || (matches!(route.privacy, RoomPrivacy::Direct { .. }) && !route.encrypted)
        {
            return Err(Error::Unsupported);
        }
        Ok(observation.joined)
    }
    /// Release the journaled send when the domain says the host cancelled it
    /// and the journal holds no crypto leg (`Command::Abandon` re-checks).
    /// False leaves the send exactly as it was: parked or resumable.
    async fn abandon_cancelled(&self, owner: &Owner, attempt: &Attempt) -> Result<bool, Error> {
        if attempt.route.encrypted
            || attempt.writes.iter().any(|write| !write.room)
            || !matches!(
                attempt.phase,
                Phase::BeforeBegin | Phase::Ready | Phase::WritePossible
            )
        {
            return Ok(false);
        }
        let cancelled = match attempt.kind {
            Kind::Final => {
                self.domain
                    .final_reply_send_cancelled(attempt.id.clone(), attempt.fence)
                    .await?
            }
            Kind::Notice => {
                self.domain
                    .verified_notice_send_cancelled(attempt.id.clone(), attempt.fence)
                    .await?
            }
            Kind::Command => {
                self.domain
                    .command_notice_send_cancelled(attempt.id.clone(), attempt.fence)
                    .await?
            }
            Kind::File => false,
        };
        if !cancelled {
            return Ok(false);
        }
        owner.outgoing(Command::Abandon).await?;
        Ok(true)
    }
    async fn retire_outgoing_room(&self, route: &ReplyRoute) -> Result<(), Error> {
        observe!(OutgoingRetireRoom);
        self.domain
            .invalidate_matrix_room(MatrixRoomInvalidation {
                engagement_id: route.engagement_id.clone(),
                registration_generation: route.registration_generation,
                transport_generation: route.transport_generation,
                room_id: route.room_id.clone(),
                generation: route
                    .room_generation
                    .checked_add(1)
                    .ok_or(Error::Capacity)?,
                reason: "Matrix current recipient proof changed".into(),
            })
            .await
            .map_err(Error::from)
    }
    /// Task #9: one journaled write, with the permanent-refusal fact recorded.
    /// A write answered with an HTTP 4xx other than 429 is what TS calls
    /// `isPermanentRouterMatrixFailure` (bridge-matrix.js:6029-6033): TS posts
    /// that command to `../failed` and stops retrying. Recording it on the
    /// journal is the same stop, in the state a resume reads — a later resume
    /// then parks the send for a human instead of re-putting it forever.
    /// Everything else (a lost connection, a timeout, a 5xx, an unacceptable
    /// 200) is left unmarked and is re-sent with the same transaction id, which
    /// Matrix dedups.
    async fn write_outgoing(
        &self,
        owner: &Owner,
        route: &ReplyRoute,
        write: Write,
        cancel: &CancellationToken,
    ) -> Result<Value, Error> {
        let result = if write.room {
            // The kick moment (board #11, parity bridge-matrix.js:10888-10950):
            // the retained bridge retries a send that failed on membership —
            // re-invite, rejoin, resend. Native's kick fact surfaces here, at the
            // write itself (403): the preflights still saw the agent joined, so
            // nothing has retired the room scope, and the rejoin restores exactly
            // the membership the route was drafted against. The retry reuses the
            // same transaction id, so a server that somehow accepted before
            // refusing dedupes. A 403 that is not membership (a dead token) fails
            // the rejoin the same way and keeps the refusal — TS discriminates by
            // error text; the rejoin POST is the native discriminator.
            let send = [
                "_matrix",
                "client",
                "v3",
                "rooms",
                &route.room_id,
                "send",
                &write.event_type,
                &write.transaction_id,
            ];
            let sent = self
                .http
                .put(&send, write.body.clone(), cancel)
                .await
                .and_then(|response| response.success());
            match sent {
                Ok(value) => Ok(value),
                Err(Error::Unauthorized) => {
                    // The invite half of the retained invite-then-join pair
                    // (bridge-matrix.js:10912-10918): a kicked member needs a
                    // fresh invite no agent can mint for itself. The
                    // representative credential rides the config from the
                    // provisioning custody that verified it; without one,
                    // recovery stays join-only.
                    let invited = match &self.representative {
                        Some(representative) => representative
                            .post(
                                &["_matrix", "client", "v3", "rooms", &route.room_id, "invite"],
                                serde_json::to_string(&json!({"user_id":route.sender_mxid}))
                                    .map_err(|_| Error::Capacity)?,
                                cancel,
                            )
                            .await
                            .is_ok_and(|response| response.status == 200),
                        None => true,
                    };
                    let restored = invited
                        && crate::identity_polish::agent_rejoin(&self.http, &route.room_id, cancel)
                            .await
                            .is_ok();
                    let retried = if restored {
                        self.http
                            .put(&send, write.body.clone(), cancel)
                            .await
                            .and_then(|response| response.success())
                    } else {
                        Err(Error::Unauthorized)
                    };
                    match retried {
                        Ok(value) => Ok(value),
                        Err(error) => {
                            eprintln!(
                                "{}",
                                crate::identity_polish::send_retry_warning(
                                    &route.room_id,
                                    "membership was lost and the rejoin did not restore it",
                                )
                            );
                            Err(error)
                        }
                    }
                }
                Err(error) => Err(error),
            }
        } else {
            self.http
                .put(
                    &[
                        "_matrix",
                        "client",
                        "v3",
                        "sendToDevice",
                        &write.event_type,
                        &write.transaction_id,
                    ],
                    write.body,
                    cancel,
                )
                .await?
                .success()
        };
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                // Task #9: only a refusal that survived the membership retry is
                // journalled as permanent. A lost dial, a timeout or a 5xx is
                // left unmarked and re-sent with the same transaction id.
                if permanent_refusal(&error) {
                    // Best effort: the permanent verdict is the fact the caller
                    // must see; losing the marker only costs one deduped re-PUT.
                    let _ = owner.outgoing(Command::Refused).await;
                }
                Err(error)
            }
        }
    }
    async fn settle_outgoing(
        &self,
        owner: &Owner,
        attempt: Attempt,
        replayed: bool,
    ) -> Result<OutgoingSummary, Error> {
        #[cfg(test)]
        let fault = self
            .outgoing_fault
            .swap(0, std::sync::atomic::Ordering::SeqCst);
        #[cfg(test)]
        if fault == 1 {
            return Err(Error::Busy);
        }
        #[cfg(test)]
        if fault == 3 {
            self.outgoing_reached.notify_one();
            self.outgoing_continue.notified().await;
        }
        observe!(OutgoingSettle);
        let observation = ReplyReconciliation::Delivered(attempt.observation()?);
        match attempt.kind {
            Kind::Final => {
                self.domain
                    .reconcile_final_reply(attempt.id.clone(), attempt.fence, observation)
                    .await?;
            }
            Kind::Notice => {
                self.domain
                    .reconcile_verified_task_notice(attempt.id.clone(), attempt.fence, observation)
                    .await?;
            }
            Kind::Command => {
                self.domain
                    .reconcile_command_notice(attempt.id.clone(), attempt.fence, observation)
                    .await?;
            }
            Kind::File => {
                let binding = attempt.file.as_ref().ok_or(Error::Storage)?;
                let settlement = self
                    .domain
                    .restore_file_delivery_settlement_for_content(
                        binding.locator.clone(),
                        binding.metadata().clone(),
                        binding.captured().clone(),
                    )
                    .await?
                    .ok_or(Error::OutcomeUnknown)?;
                let receipt = attempt.receipt()?;
                self.domain
                    .record_file_delivery_settlement(
                        std::sync::Arc::new(settlement),
                        hagency_core::file_delivery::FileDeliveryAcceptance {
                            transaction_id: attempt.transaction_id.clone(),
                            content_digest: attempt.domain_digest.clone(),
                            event_id: attempt.observation()?.event_id,
                            receipt_id: format!("file_receipt_{}", receipt.attempt_digest),
                            receipt_digest: receipt.attempt_digest,
                        },
                    )
                    .await?;
            }
        }
        #[cfg(test)]
        if fault == 2 {
            return Err(Error::OutcomeUnknown);
        }
        owner.outgoing(Command::Settle).await?;
        let summary = OutgoingSummary {
            id: Some(attempt.id.clone()),
            state: OutgoingState::Delivered,
            replayed,
        };
        if attempt.kind == Kind::File {
            let binding = attempt.file.as_ref().ok_or(Error::Storage)?;
            self.complete_file_publication(
                &binding.locator,
                binding.metadata(),
                binding.captured(),
                &summary,
            )?;
        }
        Ok(summary)
    }
}

#[cfg(test)]
#[path = "../tests/outgoing/mod.rs"]
mod tests;
