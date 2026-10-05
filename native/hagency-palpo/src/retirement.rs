//! Host-configured retirement endpoint with an exact closed verification receipt.
use crate::{CancellationToken, Error, HostConfig, http::Http};
use serde_json::{Value, json};

pub struct RetirementClient {
    http: Http,
    fleet: String,
    server: String,
}
impl RetirementClient {
    pub fn new(config: &HostConfig) -> Result<Self, Error> {
        Ok(Self {
            http: Http::new(config)?,
            fleet: config.activation.registration.fleet_id.clone(),
            server: config.activation.registration.side_id.clone(),
        })
    }
    pub async fn retire(
        &self,
        request_id: &str,
        agent_mxid: &str,
        cancel: &CancellationToken,
    ) -> Result<String, Error> {
        if request_id.is_empty()
            || request_id.len() > 128
            || request_id.chars().any(char::is_control)
            || agent_mxid.len() > 512
            || !agent_mxid.starts_with(&format!("@{}_", self.fleet))
            || !agent_mxid.ends_with(&format!(":{}", self.server))
        {
            return Err(Error::Config);
        }
        let body = json!({"requestId":request_id,"agentMxid":agent_mxid});
        let result = self
            .http
            .request("retire-agent", None, Some(body.to_string()), cancel)
            .await?
            .success()?;
        verify(&result, &self.fleet, request_id, agent_mxid)?;
        // Only the identity proof is retained. Local stop and usage settlement
        // belong to the domain worker, never a remote response.
        Ok(json!({"fleetId":self.fleet,"requestId":request_id,"mxid":agent_mxid,"matrixIdentity":"deactivated","appserviceAccess":"revoked","joinedRooms":[]}).to_string())
    }
}
fn verify(value: &Value, fleet: &str, request: &str, mxid: &str) -> Result<(), Error> {
    if value["ok"] != true
        || value["fleetId"] != fleet
        || value["requestId"] != request
        || value["agent"]["mxid"] != mxid
        || value["agent"]["state"] != "retired"
        || value["agent"]["matrixIdentity"] != "deactivated"
        || value["agent"]["appserviceAccess"] != "revoked"
        || value["agent"]["joinedRooms"]
            .as_array()
            .is_none_or(|r| !r.is_empty())
    {
        return Err(Error::Wire);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_retirement_requires_every_exact_remote_identity_field() {
        let valid = json!({"ok":true,"fleetId":"fleet","requestId":"request","agent":{"mxid":"@agent:test","state":"retired","matrixIdentity":"deactivated","appserviceAccess":"revoked","joinedRooms":[]}});
        assert!(verify(&valid, "fleet", "request", "@agent:test").is_ok());
        for pointer in [
            "/ok",
            "/fleetId",
            "/requestId",
            "/agent/mxid",
            "/agent/state",
            "/agent/matrixIdentity",
            "/agent/appserviceAccess",
            "/agent/joinedRooms",
        ] {
            let mut changed = valid.clone();
            *changed.pointer_mut(pointer).unwrap() = Value::Null;
            assert_eq!(
                verify(&changed, "fleet", "request", "@agent:test"),
                Err(Error::Wire),
                "{pointer}"
            );
        }
        let mut joined = valid.clone();
        joined["agent"]["joinedRooms"] = json!(["!remaining:test"]);
        assert_eq!(
            verify(&joined, "fleet", "request", "@agent:test"),
            Err(Error::Wire)
        );
        assert_eq!(
            verify(&valid, "other", "request", "@agent:test"),
            Err(Error::Wire)
        );
        assert_eq!(
            verify(&valid, "fleet", "other", "@agent:test"),
            Err(Error::Wire)
        );
        assert_eq!(
            verify(&valid, "fleet", "request", "@other:test"),
            Err(Error::Wire)
        );
    }
}
