//! One identity-cleanup executor per authenticated Palpo profile. It remains
//! available without a provider runtime and only inspects the original effect.
use hagency_palpo::{CancellationToken, RetirementClient};
use hagency_store::{DomainStore, Effect, EffectOutcome};
use std::time::Duration;

pub(super) async fn run(
    client: &RetirementClient,
    domain: &DomainStore,
    fleet: &str,
    cancel: &CancellationToken,
) {
    while !cancel.is_cancelled() {
        if let Err(error) = pass(client, domain, fleet, cancel).await {
            tracing::warn!(?error, "identity retirement remains pending");
        }
        tokio::select! {_=cancel.cancelled()=>return,_=tokio::time::sleep(Duration::from_secs(5))=>{}}
    }
}
async fn pass(
    client: &RetirementClient,
    domain: &DomainStore,
    fleet: &str,
    cancel: &CancellationToken,
) -> Result<(), hagency_store::Error> {
    let registration = domain.provisioning_registration(fleet.to_owned()).await?;
    for id in domain
        .pending_identity_retirements(fleet.to_owned())
        .await?
    {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let effect = match domain.claim_effect_for(format!("retire_{id}")).await? {
            Some(effect) => Some(effect),
            None => domain.inspect_retirement_effect(id).await?,
        };
        let Some(effect) = effect else {
            continue;
        };
        let outcome = match binding(&effect, &registration) {
            Ok((request, mxid)) => match client.retire(&request, &mxid, cancel).await {
                Ok(receipt) => EffectOutcome::Applied { receipt },
                // A live profile import can cancel this worker without closing
                // the domain store. Preserve uncertainty now so the replacement
                // generation can inspect it without requiring a process restart.
                Err(hagency_palpo::Error::Cancelled) => EffectOutcome::Unknown,
                Err(error) => {
                    tracing::warn!(?error, "remote identity removal is unconfirmed");
                    EffectOutcome::Unknown
                }
            },
            Err(_) => EffectOutcome::Unknown,
        };
        // Claim/inspection is already durable. Never abandon an observed proof
        // to a cancellation select before the domain has recorded it.
        domain
            .observe_effect(effect.id, effect.fence, outcome)
            .await?;
    }
    Ok(())
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
