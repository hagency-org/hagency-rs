//! One identity-cleanup executor per authenticated Palpo profile. It remains
//! available without a provider runtime and only inspects the original effect.
use crate::cleanup_state::{self, Backoff};
use hagency_palpo::{CancellationToken, RetirementClient};
use hagency_store::{DomainStore, Effect, EffectOutcome};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

pub(super) async fn run(
    client: &RetirementClient,
    domain: &DomainStore,
    fleet: &str,
    cancel: &CancellationToken,
) {
    // The loop still ticks every 5 s (a cheap local read); each effect is
    // retried only when its own backoff is due (ADR-199).
    let mut backoff = Backoff::default();
    let mut pass_error = false;
    while !cancel.is_cancelled() {
        match pass(client, domain, fleet, cancel, &mut backoff, wall_ms).await {
            Err(error) if !pass_error => {
                pass_error = true;
                tracing::warn!(?error, "identity retirement remains pending");
            }
            Err(error) => tracing::debug!(?error, "identity retirement remains pending"),
            Ok(()) => pass_error = false,
        }
        cleanup_state::publish(backoff.snapshot());
        tokio::select! {_=cancel.cancelled()=>return,_=tokio::time::sleep(Duration::from_secs(5))=>{}}
    }
}
async fn pass(
    client: &RetirementClient,
    domain: &DomainStore,
    fleet: &str,
    cancel: &CancellationToken,
    backoff: &mut Backoff,
    clock: impl Fn() -> u64,
) -> Result<(), hagency_store::Error> {
    let registration = domain.provisioning_registration(fleet.to_owned()).await?;
    let ids = domain
        .pending_identity_retirements(fleet.to_owned())
        .await?;
    backoff.retain_only(&ids);
    for id in ids {
        if cancel.is_cancelled() {
            return Ok(());
        }
        if !backoff.due(&id, clock()) {
            continue;
        }
        let effect = match domain.claim_effect_for(format!("retire_{id}")).await? {
            Some(effect) => Some(effect),
            None => domain.inspect_retirement_effect(id.clone()).await?,
        };
        let Some(effect) = effect else {
            continue;
        };
        let (outcome, failure) = match binding(&effect, &registration) {
            Ok((request, mxid)) => settle(client.retire(&request, &mxid, cancel).await),
            Err(_) => (EffectOutcome::Unknown, Some("binding_invalid".to_owned())),
        };
        match failure {
            Some(kind) => {
                if backoff.failure(&id, &kind, clock()) {
                    tracing::warn!(kind, "remote identity removal is unconfirmed");
                } else {
                    tracing::debug!(kind, "remote identity removal is still unconfirmed");
                }
            }
            None if matches!(outcome, EffectOutcome::Applied { .. }) => backoff.success(&id),
            None => {}
        }
        // Claim/inspection is already durable. Never abandon an observed proof
        // to a cancellation select before the domain has recorded it.
        domain
            .observe_effect(effect.id, effect.fence, outcome)
            .await?;
    }
    Ok(())
}
/// Only a verified receipt is `Applied`. A 503 or any other non-proof answer
/// stays `Unknown` and is reported as a failure kind for the backoff.
fn settle(result: Result<String, hagency_palpo::Error>) -> (EffectOutcome, Option<String>) {
    match result {
        Ok(receipt) => (EffectOutcome::Applied { receipt }, None),
        // A live profile import can cancel this worker without closing
        // the domain store. Preserve uncertainty now so the replacement
        // generation can inspect it without requiring a process restart.
        Err(hagency_palpo::Error::Cancelled) => (EffectOutcome::Unknown, None),
        Err(error) => (EffectOutcome::Unknown, Some(error.to_string())),
    }
}
fn binding(
    effect: &Effect,
    registration: &hagency_core::authority::Registration,
) -> Result<(String, String), ()> {
    let request: hagency_core::authority::ProjectRequest =
        serde_json::from_value(effect.payload["request"].clone()).map_err(|_| ())?;
    request.validate(registration).map_err(|_| ())?;
    if effect.kind != "retire"
        || effect.payload["registrationGeneration"] != registration.generation
        || request.engagement_id().map_err(|_| ())? != effect.engagement_id
    {
        return Err(());
    }
    Ok((
        request.request_id,
        format!(
            "@{}_{}:{}",
            registration.fleet_id, effect.engagement_id, registration.server_name
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retirement_503_is_never_marked_applied() {
        let (outcome, failure) = settle(Err(hagency_palpo::Error::Remote(503)));
        assert!(matches!(outcome, EffectOutcome::Unknown));
        assert_eq!(failure.as_deref(), Some("remote service returned HTTP 503"));
        let (outcome, failure) = settle(Err(hagency_palpo::Error::Wire));
        assert!(matches!(outcome, EffectOutcome::Unknown));
        assert!(failure.is_some());
        let (outcome, failure) = settle(Err(hagency_palpo::Error::Cancelled));
        assert!(matches!(outcome, EffectOutcome::Unknown) && failure.is_none());
        let (outcome, failure) = settle(Ok("proof".into()));
        assert!(matches!(outcome, EffectOutcome::Applied { .. }) && failure.is_none());
    }
}
