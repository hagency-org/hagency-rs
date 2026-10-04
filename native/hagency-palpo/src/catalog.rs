//! Coherent domain observations enter the existing immutable publication lane.
use super::*;
use hagency_store::DomainStore;

impl Adapter {
    pub async fn publish_resources_once(
        &self,
        domain: &DomainStore,
        cancel: &CancellationToken,
    ) -> Result<Step, Error> {
        let mut cursor = self.publication.try_lock().map_err(|_| Error::Busy)?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        // Rotation is checked even for an already frozen historical catalog.
        // A changed catalog alone must not overwrite that original pending body.
        domain
            .check_publication_registration(self.registration.clone())
            .await?;
        let Reply::Publication(pending) = self
            .command(Command::PendingPublication(self.scope()))
            .await?
        else {
            return Err(Error::Custody);
        };
        let pending_body = pending
            .as_ref()
            .map(|ticket| serde_json::from_str::<Value>(ticket.wire_body()))
            .transpose()
            .map_err(|_| Error::Custody)?;
        let values = |key: &str| {
            pending_body
                .as_ref()
                .and_then(|b| b.get(key))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        let mut included = values("probeReceipts");
        let mut included_statuses = values("statuses");
        let mut contribution_page: Option<hagency_store::ContributionPage> = pending_body
            .as_ref()
            .and_then(|b| b.get("contributionPage"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| Error::Custody)?;
        let registration = domain
            .provisioning_registration(self.registration.fleet_id.clone())
            .await?;
        let mut command_receipts = match pending.as_ref() {
            Some(ticket) => serde_json::from_str::<Value>(ticket.wire_body())
                .map_err(|_| Error::Custody)?
                .get("commandReceipts")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| Error::Custody)?
                .unwrap_or_default(),
            None => Vec::<hagency_core::project_commands::ProjectReceipt>::new(),
        };
        if pending.is_none() {
            let snapshot = domain.published_catalog(self.registration.clone()).await?;
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let mut body = snapshot.into_update();
            let page = domain
                .contribution_page(self.registration.clone(), cursor.clone())
                .await?;
            body["contributionPage"] = serde_json::to_value(&page).map_err(|_| Error::Custody)?;
            contribution_page = Some(page);
            command_receipts = domain
                .pending_project_receipts(registration.clone())
                .await?;
            if !command_receipts.is_empty() {
                body["commandReceipts"] =
                    serde_json::to_value(&command_receipts).map_err(|_| Error::Custody)?;
            }
            if let Some(source) = &self.receipts {
                included = source.pending().into_iter().take(10).collect();
                if !included.is_empty() {
                    body["probeReceipts"] = Value::Array(included.clone());
                }
                included_statuses = source.statuses().into_iter().take(200).collect();
                if !included_statuses.is_empty() {
                    body["statuses"] = Value::Array(included_statuses.clone());
                }
            }
            let Reply::Publication(Some(_)) = self
                .command(Command::FreezePublication {
                    scope: self.scope(),
                    body,
                })
                .await?
            else {
                return Err(Error::Custody);
            };
        }
        let step = self.publish_checked(Some(domain), cancel).await?;
        if step == Step::Published
            && !included.is_empty()
            && let Some(source) = &self.receipts
        {
            source.published(&included);
        }
        if step == Step::Published
            && let Some(source) = &self.receipts
        {
            source.statuses_published(&included_statuses);
        }
        if step == Step::Published && !command_receipts.is_empty() {
            domain
                .mark_project_receipts_published(registration, command_receipts)
                .await?;
        }
        if step == Step::Published
            && let Some(page) = contribution_page
        {
            // Advance only from the acknowledged frozen bytes. A restart can
            // resend that original page and then continue beyond its last ID.
            *cursor = page.next_after.unwrap_or_default();
        }
        Ok(step)
    }
}
