//! Import the fleet configuration a Palpo HAgency owner downloads (TS parity:
//! `mockup/lib/fleet-credential-import.js` `parseFleetCredentialImport` and
//! `lib/fleet-outbound-config.js` `normalizeOutboundTransport`).
//!
//! The download is `{fleetId, serverName, credentialVersion: 1, registration,
//! transport}`: the App Service registration Palpo installed for this fleet and
//! the outbound machine credential. The import writes three things and nothing
//! else:
//! - the six-field fleet `registrations` row through the store's sole writer,
//!   with the reception left UNBOUND — Palpo's connection probe binds it later
//!   (`lib/fleet-protocol.js` sets `receptionRoomId` only there);
//! - `palpo-transport.json` + `palpo.machine_token`, the inputs of
//!   `serve --palpo-transport`;
//! - `palpo-appservice.json`, the App Service tokens the fleet's identities
//!   act with.
//!
//! Every rule below is the TS rule; a refusal names the field, never a value.
use hagency_core::authority::Registration;
use hagency_store::{DomainRepository, Repository, private};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the configuration is not the owner download from Palpo ({0})")]
    Invalid(&'static str),
    #[error("the state directory refused the import: {0}")]
    Store(#[from] hagency_store::Error),
}

/// Local part of the fleet's private approval bot inside its exclusive
/// namespace `^@<fleetId>_[a-z0-9_]+:<server>$`.
const APPROVAL_LOCALPART: &str = "approval";

pub struct Imported {
    pub fleet_id: String,
    pub server_name: String,
    pub representative: String,
    pub approval_bot: String,
    pub endpoint: String,
    /// The bound reception room, empty until Palpo's connection probe.
    pub reception: String,
}

fn text<'a>(value: &'a Value, key: &str, field: &'static str) -> Result<&'a str, Error> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(Error::Invalid(field))
}

fn token(value: &str, field: &'static str) -> Result<(), Error> {
    if value.trim().is_empty() || value.len() > 4096 {
        return Err(Error::Invalid(field));
    }
    Ok(())
}

pub fn parse(raw: &str) -> Result<(Registration, Value, Value, String, u64), Error> {
    if raw.len() > 65536 {
        return Err(Error::Invalid("size"));
    }
    let envelope: Value = serde_json::from_str(raw).map_err(|_| Error::Invalid("json"))?;
    if !envelope.is_object() {
        return Err(Error::Invalid("json"));
    }
    if envelope.get("credentialVersion").is_some_and(|v| v != 1) {
        return Err(Error::Invalid("credentialVersion"));
    }
    let registration = envelope
        .get("registration")
        .ok_or(Error::Invalid("registration"))?;
    let fleet_id = text(registration, "id", "registration.id")?;
    let suffix = fleet_id
        .strip_prefix("hf_")
        .ok_or(Error::Invalid("registration.id"))?;
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid("registration.id"));
    }
    if envelope.get("fleetId").is_some_and(|v| v != fleet_id) {
        return Err(Error::Invalid("fleetId"));
    }
    let server_name = text(&envelope, "serverName", "serverName")?;
    let sender = text(
        registration,
        "sender_localpart",
        "registration.sender_localpart",
    )?;
    if sender != format!("{fleet_id}_representative") {
        return Err(Error::Invalid("registration.sender_localpart"));
    }
    let escaped: String = server_name
        .chars()
        .flat_map(|c| {
            let special = ".*+?^${}()|[]\\".contains(c);
            special
                .then_some('\\')
                .into_iter()
                .chain(std::iter::once(c))
        })
        .collect();
    let namespace = format!("^@{fleet_id}_[a-z0-9_]+:{escaped}$");
    let namespaces = registration
        .get("namespaces")
        .ok_or(Error::Invalid("registration.namespaces"))?;
    let users = namespaces.get("users").and_then(Value::as_array);
    let empty = |key| {
        namespaces
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    };
    if !users.is_some_and(|u| {
        u.len() == 1
            && u[0].get("exclusive") == Some(&json!(true))
            && u[0].get("regex") == Some(&json!(namespace))
    }) || !empty("rooms")
        || !empty("aliases")
    {
        return Err(Error::Invalid("registration.namespaces"));
    }
    let as_token = text(registration, "as_token", "registration.as_token")?;
    let hs_token = text(registration, "hs_token", "registration.hs_token")?;
    token(as_token, "registration.as_token")?;
    token(hs_token, "registration.hs_token")?;
    if as_token == hs_token {
        return Err(Error::Invalid("registration tokens"));
    }
    let url = text(registration, "url", "registration.url")?;
    let parsed = reqwest::Url::parse(url).map_err(|_| Error::Invalid("registration.url"))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(Error::Invalid("registration.url"));
    }
    // The outbound machine credential (normalizeOutboundTransport).
    let transport = envelope.get("transport").ok_or(Error::Invalid(
        "transport: only an outbound fleet can be imported",
    ))?;
    let keys_ok = transport.as_object().is_some_and(|o| {
        o.keys()
            .all(|k| ["mode", "url", "token", "generation"].contains(&k.as_str()))
    });
    let machine = text(transport, "token", "transport.token")?;
    let generation = transport
        .get("generation")
        .and_then(Value::as_u64)
        .ok_or(Error::Invalid("transport.generation"))?;
    if !keys_ok
        || transport.get("mode") != Some(&json!("outbound"))
        || machine.len() < 16
        || machine.len() > 4096
        || machine.chars().any(char::is_whitespace)
        || machine == as_token
        || machine == hs_token
        || !(1..=hagency_core::JSON_SAFE_MAX).contains(&generation)
    {
        return Err(Error::Invalid("transport"));
    }
    let endpoint = reqwest::Url::parse(text(transport, "url", "transport.url")?)
        .map_err(|_| Error::Invalid("transport.url"))?;
    let local = matches!(
        endpoint.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]")
    );
    if !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || endpoint.path().trim_end_matches('/') != format!("/api/fleet/v2/{fleet_id}")
        || !(endpoint.scheme() == "https" || endpoint.scheme() == "http" && local)
    {
        return Err(Error::Invalid("transport.url"));
    }
    let endpoint = endpoint.as_str().trim_end_matches('/').to_owned();
    let row = Registration {
        fleet_id: fleet_id.to_owned(),
        generation: envelope
            .get("engagement")
            .and_then(|e| e.get("registrationGeneration"))
            .and_then(Value::as_u64)
            .unwrap_or(1),
        server_name: server_name.to_owned(),
        reception_room_id: String::new(),
        representative_mxid: format!("@{sender}:{server_name}"),
        approval_bot_mxid: format!("@{fleet_id}_{APPROVAL_LOCALPART}:{server_name}"),
    };
    row.validate().map_err(|_| Error::Invalid("serverName"))?;
    let appservice = json!({
        "as_token": as_token, "hs_token": hs_token, "sender_localpart": sender,
        "namespace": namespace, "url": url,
    });
    Ok((row, appservice, json!(machine), endpoint, generation))
}

/// Import is an explicit resource-owner delegation, not a machine command.
/// A downloaded "verified" label cannot replace this installation's own probe.
pub(crate) fn coordinator_profile(
    raw: &str,
    registration: &Registration,
) -> Result<Option<hagency_store::coordinator::ServerEngagement>, Error> {
    let envelope: Value = serde_json::from_str(raw).map_err(|_| Error::Invalid("json"))?;
    let Some(value) = envelope.get("engagement") else {
        return Ok(None);
    };
    let mut engagement: hagency_store::coordinator::ServerEngagement =
        serde_json::from_value(value.clone()).map_err(|_| Error::Invalid("engagement"))?;
    if engagement.id.as_str() != registration.fleet_id
        || engagement.server.as_str() != registration.server_name
        || u64::from(engagement.registration_generation) != registration.generation
        || !matches!(
            value["state"].as_str(),
            Some("approved" | "configuring" | "verifying" | "verified")
        )
    {
        return Err(Error::Invalid("engagement binding"));
    }
    engagement.state =
        serde_json::from_value(json!(if registration.reception_room_id.is_empty() {
            "configuring"
        } else {
            "verified"
        }))
        .map_err(|_| Error::Invalid("engagement state"))?;
    Ok(Some(engagement))
}

/// The Matrix client API the fleet's App Service identities act through.
pub(crate) fn homeserver(homeserver: &str) -> Result<String, Error> {
    let origin = reqwest::Url::parse(homeserver).map_err(|_| Error::Invalid("homeserver"))?;
    if origin.scheme() != "https" && !matches!(origin.host_str(), Some("127.0.0.1" | "localhost")) {
        return Err(Error::Invalid("homeserver must be https"));
    }
    Ok(origin.as_str().trim_end_matches('/').to_owned())
}

/// Stable per-engagement credential directories. Keep the first legacy profile
/// in place; a second profile must never overwrite it, even on the same server.
pub(crate) fn profile_directory(state: &Path, fleet: &str) -> Result<PathBuf, Error> {
    if fleet.len() != 35
        || !fleet.starts_with("hf_")
        || !fleet[3..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid("fleetId"));
    }
    let legacy = state.join("palpo-transport.json");
    if present(&legacy)? {
        let raw = private::read_secret(&legacy)?;
        let current: Value =
            serde_json::from_slice(&raw).map_err(|_| Error::Invalid("existing profile"))?;
        if current["registration"]["fleetId"] == fleet {
            return Ok(state.to_owned());
        }
    } else if !present(&state.join("palpo-engagements"))? {
        return Ok(state.to_owned());
    }
    let root = state.join("palpo-engagements");
    private::directory(&root)?;
    let directory = root.join(fleet);
    if !present(&directory)?
        && std::fs::read_dir(&root)
            .map_err(|_| Error::Invalid("profile directory"))?
            .count()
            >= 32
    {
        return Err(Error::Invalid("too many engagements"));
    }
    private::directory(&directory)?;
    Ok(directory)
}

pub(crate) fn profile_directories(state: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut result = Vec::new();
    if present(&state.join("palpo-transport.json"))? {
        result.push(state.to_owned());
    }
    let root = state.join("palpo-engagements");
    if !present(&root)? {
        return Ok(result);
    }
    private::directory(&root)?;
    for entry in std::fs::read_dir(root).map_err(|_| Error::Invalid("profile directory"))? {
        let entry = entry.map_err(|_| Error::Invalid("profile directory"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| Error::Invalid("profile directory"))?;
        if name.len() != 35
            || !name.starts_with("hf_")
            || !name[3..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Invalid("profile directory"));
        }
        private::directory(&entry.path())?;
        if present(&entry.path().join("palpo-transport.json"))? {
            result.push(entry.path());
        }
        if result.len() > 33 {
            return Err(Error::Invalid("too many engagements"));
        }
    }
    result.sort();
    Ok(result)
}

fn present(path: &Path) -> Result<bool, Error> {
    match path.symlink_metadata() {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(Error::Invalid("profile unreadable")),
    }
}

/// The three private files `serve --palpo-transport` reads; one writer for the
/// CLI and the console import.
pub(crate) fn write(
    state: &Path,
    registration: &Registration,
    appservice: &Value,
    machine: &Value,
    endpoint: &str,
    generation: u64,
) -> Result<(), Error> {
    let transport = json!({
        "profile": "palpo_v2_resources_v1", "endpoint": endpoint,
        "registration": registration, "machine_generation": generation,
    });
    let encode =
        |value: &Value| serde_json::to_vec_pretty(value).map_err(|_| Error::Invalid("encode"));
    private::replace(&state.join("palpo-transport.json"), &encode(&transport)?)?;
    private::replace(
        &state.join("palpo.machine_token"),
        machine.as_str().unwrap_or_default().as_bytes(),
    )?;
    private::replace(&state.join("palpo-appservice.json"), &encode(appservice)?)?;
    Ok(())
}

/// Import into an initialized private state (service stopped or not yet run).
pub fn run(
    state: &Path,
    file: &Path,
    homeserver: &str,
    reception: Option<&str>,
) -> Result<Imported, Error> {
    let origin =
        self::homeserver(homeserver).map_err(|_| Error::Invalid("--homeserver must be https"))?;
    private::read_secret(&state.join("operator.token"))?;
    let raw = std::fs::read_to_string(file).map_err(|_| Error::Invalid("file unreadable"))?;
    let (registration, mut appservice, machine, endpoint, generation) = parse(&raw)?;
    appservice["homeserver"] = json!(origin);
    let _custody = Repository::open(state)?;
    let mut domain = DomainRepository::open(state)?;
    // A re-import of the same fleet keeps a reception an earlier probe bound.
    let current = domain
        .provisioning_registration(&registration.fleet_id)
        .ok();
    let mut registration = registration;
    if let Some(current) = current {
        registration.reception_room_id = current.reception_room_id;
    } else if let Some(room) = reception {
        registration.reception_room_id = room.to_owned();
    }
    let policy = coordinator_profile(&raw, &registration)?;
    domain.import_coordinator_registration(&registration, policy.as_ref())?;
    let profile = profile_directory(state, &registration.fleet_id)?;
    write(
        &profile,
        &registration,
        &appservice,
        &machine,
        &endpoint,
        generation,
    )?;
    Ok(Imported {
        fleet_id: registration.fleet_id.clone(),
        server_name: registration.server_name.clone(),
        representative: registration.representative_mxid.clone(),
        approval_bot: registration.approval_bot_mxid.clone(),
        endpoint,
        reception: registration.reception_room_id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const FLEET: &str = "hf_0123456789abcdef0123456789abcdef";
    fn download() -> Value {
        json!({"fleetId": FLEET, "serverName": "example.test", "credentialVersion": 1,
            "registration": {"id": FLEET, "url": "http://relay:8090/api/relay/v2/x", "as_token": "as-token-value",
                "hs_token": "hs-token-value", "sender_localpart": format!("{FLEET}_representative"),
                "namespaces": {"users": [{"exclusive": true, "regex": format!("^@{FLEET}_[a-z0-9_]+:example\\.test$")}],
                    "aliases": [], "rooms": []}, "rate_limited": true, "receive_ephemeral": false},
            "transport": {"mode": "outbound", "url": format!("https://palpo.example/api/fleet/v2/{FLEET}"),
                "token": "machine-token-0123456789", "generation": 1}})
    }

    #[test]
    fn native_palpo_multiple_profiles_same_server_preserve_independent_credentials() {
        let root = tempfile::tempdir().unwrap();
        let first = download();
        let (one, as_one, machine_one, endpoint_one, generation) =
            parse(&first.to_string()).unwrap();
        let primary = profile_directory(root.path(), &one.fleet_id).unwrap();
        assert_eq!(primary, root.path());
        write(
            &primary,
            &one,
            &as_one,
            &machine_one,
            &endpoint_one,
            generation,
        )
        .unwrap();
        let saved = private::read_secret(&primary.join("palpo-appservice.json")).unwrap();
        let mut second = first
            .to_string()
            .replace(FLEET, "hf_ffffffffffffffffffffffffffffffff");
        second = second
            .replace("as-token-value", "second-as-token")
            .replace("hs-token-value", "second-hs-token");
        let (two, as_two, machine_two, endpoint_two, generation) = parse(&second).unwrap();
        let secondary = profile_directory(root.path(), &two.fleet_id).unwrap();
        assert_ne!(primary, secondary);
        write(
            &secondary,
            &two,
            &as_two,
            &machine_two,
            &endpoint_two,
            generation,
        )
        .unwrap();
        assert_eq!(
            profile_directory(root.path(), &one.fleet_id).unwrap(),
            primary
        );
        assert_eq!(
            profile_directory(root.path(), &two.fleet_id).unwrap(),
            secondary
        );
        assert_eq!(
            private::read_secret(&primary.join("palpo-appservice.json")).unwrap(),
            saved
        );
        assert_eq!(profile_directories(root.path()).unwrap().len(), 2);
        assert_eq!(
            super::super::palpo::Prepared::load_all(root.path())
                .unwrap()
                .len(),
            2
        );
        assert!(profile_directory(root.path(), "../wrong").is_err());
    }

    #[test]
    fn native_palpo_downloaded_verified_label_does_not_prove_connection() {
        let mut envelope = download();
        envelope["engagement"] = json!({"id":FLEET,"server":"example.test","owner":"@owner:example.test","coordinator":"@coordinator:example.test",
            "registrationGeneration":2,"delegationRevision":1,"delegationExpiresAtMs":9000000000000u64,"state":"verified","allowSelfApproval":false,"coordinatorApprovalV1":true});
        let raw = envelope.to_string();
        let registration = parse(&raw).unwrap().0;
        assert_eq!(registration.generation, 2);
        let policy = coordinator_profile(&raw, &registration).unwrap().unwrap();
        assert_eq!(serde_json::to_value(policy.state).unwrap(), "configuring");
        envelope["engagement"]["id"] = json!("different");
        assert!(coordinator_profile(&envelope.to_string(), &registration).is_err());
    }
    #[test]
    fn native_palpo_import_accepts_the_owner_download_unbound() {
        let (row, appservice, _, endpoint, generation) = parse(&download().to_string()).unwrap();
        assert_eq!(
            row.reception_room_id, "",
            "reception stays unbound until the probe"
        );
        assert_eq!(
            row.representative_mxid,
            format!("@{FLEET}_representative:example.test")
        );
        assert_eq!(
            row.approval_bot_mxid,
            format!("@{FLEET}_approval:example.test")
        );
        assert_eq!(
            endpoint,
            format!("https://palpo.example/api/fleet/v2/{FLEET}")
        );
        assert_eq!(generation, 1);
        assert_eq!(
            appservice["sender_localpart"],
            format!("{FLEET}_representative")
        );
    }
    #[test]
    fn native_palpo_import_refuses_what_ts_refuses() {
        #[allow(clippy::type_complexity)]
        let cases: Vec<(&str, Box<dyn Fn(&mut Value)>)> = vec![
            (
                "callback fleet",
                Box::new(|v| {
                    v.as_object_mut().unwrap().remove("transport");
                }),
            ),
            (
                "broad namespace",
                Box::new(|v| v["registration"]["namespaces"]["users"][0]["regex"] = json!("^@.*$")),
            ),
            (
                "non-exclusive",
                Box::new(|v| {
                    v["registration"]["namespaces"]["users"][0]["exclusive"] = json!(false)
                }),
            ),
            (
                "other fleet endpoint",
                Box::new(|v| {
                    v["transport"]["url"] = json!(
                        "https://palpo.example/api/fleet/v2/hf_ffffffffffffffffffffffffffffffff"
                    )
                }),
            ),
            (
                "plain http remote",
                Box::new(|v| {
                    v["transport"]["url"] =
                        json!(format!("http://palpo.example/api/fleet/v2/{FLEET}"))
                }),
            ),
            (
                "machine token reuses as_token",
                Box::new(|v| v["transport"]["token"] = json!("as-token-value")),
            ),
            (
                "wrong sender",
                Box::new(|v| v["registration"]["sender_localpart"] = json!("someone")),
            ),
            ("version 2", Box::new(|v| v["credentialVersion"] = json!(2))),
        ];
        for (name, change) in cases {
            let mut value = download();
            change(&mut value);
            assert!(parse(&value.to_string()).is_err(), "{name}");
        }
    }
}
