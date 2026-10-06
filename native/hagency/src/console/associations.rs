//! Start an association from the local owner console, then observe its durable
//! progress. Matrix identities are confirmed only by the authenticated Rinx host.
use super::{Error, body, engagements::check_lifecycle, failed, recheck};
use salvo::prelude::*;

pub(super) fn router() -> Router {
    Router::with_path("palpo/associations")
        .get(list)
        .post(start)
}
fn live(depot: &Depot) -> Result<crate::bootstrap::palpo::Live, Error> {
    depot
        .get_typed::<crate::App>()
        .ok()
        .and_then(|a| a.palpo_live().cloned())
        .ok_or(Error::Unavailable)
}
#[handler]
async fn list(depot: &mut Depot, res: &mut Response) {
    let result = live(depot).and_then(|l| l.pairing_status().map_err(|_| Error::Unavailable));
    if let Err(e) = recheck(depot) {
        failed(res, e);
        return;
    }
    match result {
        Ok(v) => res.render(Json(v)),
        Err(e) => failed(res, e),
    }
}
#[handler]
async fn start(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !check_lifecycle(depot, res) {
        return;
    }
    let result = async {
        let raw = body(req, 8192).await?;
        let input = serde_json::from_slice(&raw).map_err(|_| Error::Invalid)?;
        live(depot)?
            .create_pairing(input)
            .await
            .map_err(|e| match e {
                crate::bootstrap::association::Error::Invalid(_) => Error::Invalid,
                _ => Error::Unavailable,
            })
    }
    .await;
    if let Err(e) = recheck(depot) {
        failed(res, e);
        return;
    }
    match result {
        Ok(v) => res.render(Json(v)),
        Err(e) => failed(res, e),
    }
}
