//! Recovery of an account step interrupted after its register request was
//! sent. In App Service mode the exact-user identity and login are idempotent,
//! so recovery finishes the step without retrying create; password
//! registration stays inspection-only and refuses without any network write.
use super::*;
use crate::collector::fixtures as common;
use common::{Fake, TOKEN};
use hagency_core::authority::Registration;
use hagency_store::{Effect, EffectState, private};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const KEY: [u8; 32] = [31; 32];
const REGISTRATION_TOKEN: &str = "synthetic_registration_credential";
const AS_TOKEN: &str = "synthetic-side-application-service-token";
const BROKEN_REPLY: &[u8] = b"HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{";

struct PrivateState {
    _temp: tempfile::TempDir,
    path: PathBuf,
}
impl PrivateState {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("private");
        private::directory(&path).unwrap();
        Self { _temp: temp, path }
    }
}
fn registration() -> Registration {
    common::domain::registration()
}
fn effect() -> Effect {
    Effect {
        id: "provision_target".into(),
        engagement_id: format!("en_{}", "a".repeat(32)),
        kind: "provision".into(),
        state: EffectState::Started,
        fence: 1,
        payload: json!({"resource":{"presetId":"offline"}}),
    }
}
fn identity(effect: &Effect) -> (String, String) {
    (
        format!(
            "@{}_{}:{}",
            registration().fleet_id,
            effect.engagement_id,
            registration().server_name
        ),
        format!("DEVICE_{}", effect.engagement_id),
    )
}
fn root(state: &Path, effect: &Effect) -> PathBuf {
    state.join(format!("agent-matrix-{}", effect.id))
}
fn as_operation(fake: &Fake, state: &Path, effect: &Effect) -> TokenAccountProvision {
    TokenAccountProvision::application_service(
        &registration(),
        effect,
        &fake.endpoint,
        ApplicationServiceCredential::new(AS_TOKEN, &format!("{}_", registration().fleet_id))
            .unwrap(),
        state.to_owned(),
        KEY,
        common::load_limits(),
    )
    .unwrap()
    .with_root_pem(include_bytes!("../fixtures/ca.pem"))
    .unwrap()
}
fn password_operation(fake: &Fake, state: &Path, effect: &Effect) -> TokenAccountProvision {
    TokenAccountProvision::new(
        &registration(),
        effect,
        &fake.endpoint,
        REGISTRATION_TOKEN,
        state.to_owned(),
        KEY,
        common::load_limits(),
    )
    .unwrap()
    .with_root_pem(include_bytes!("../fixtures/ca.pem"))
    .unwrap()
}
/// The App Service peer: namespace probes are exclusive, the exact user exists,
/// and login issues the one private device.
fn as_reply(request: &common::Request, effect: &Effect) -> (u16, Value) {
    let (user, device) = identity(effect);
    if request.method == "GET" {
        if let Some(query) = request.target.split_once('?').map(|(_, q)| q) {
            if query.contains("hagency_namespace_probe_") {
                return (403, json!({"errcode":"M_EXCLUSIVE"}));
            }
            if query.contains("user_id=") {
                return (200, json!({"user_id":user,"is_guest":false}));
            }
        }
        if request.headers.get("authorization") == Some(&format!("Bearer {TOKEN}")) {
            return (
                200,
                json!({"user_id":user,"device_id":device,"is_guest":false}),
            );
        }
        return (
            200,
            json!({"user_id":registration().representative_mxid,"is_guest":false}),
        );
    }
    assert!(request.target.ends_with("/login"), "{}", request.target);
    (
        200,
        json!({"user_id":user,"device_id":device,"access_token":TOKEN}),
    )
}

/// Drives the first account attempt until its register request, then loses
/// the reply so custody stops at "possible" without "initial".
async fn interrupt_register(
    fake: &mut Fake,
    state: &Path,
    effect: &Effect,
    work: impl std::future::Future<Output = Result<ProvisionedTokenAccount, Error>>,
    application_service: bool,
) {
    let cancel_guard = work;
    let (result, ()) = tokio::join!(cancel_guard, async {
        loop {
            let request = fake.next().await;
            if request.target.ends_with("/register") {
                assert!(root(state, effect).join("possible").exists());
                request.raw(BROKEN_REPLY.to_vec());
                break;
            }
            assert!(application_service, "password mode registers first");
            let response = as_reply(&request, effect);
            request.json(response.0, response.1);
        }
    });
    assert_eq!(result.err(), Some(Error::OutcomeUnknown));
    assert!(root(state, effect).join("possible").exists());
    assert!(!root(state, effect).join("initial").exists());
    assert!(!root(state, effect).join("complete").exists());
}

#[tokio::test]
async fn app_service_recovery_finishes_an_interrupted_account_step() {
    let state = PrivateState::new();
    let effect = effect();
    let mut fake = Fake::start(true).await;
    let cancel = CancellationToken::new();
    let first = as_operation(&fake, &state.path, &effect).execute(&cancel);
    interrupt_register(&mut fake, &state.path, &effect, first, true).await;

    let recovery = as_operation(&fake, &state.path, &effect)
        .for_recovery()
        .execute(&cancel);
    tokio::pin!(recovery);
    let mut logins = 0;
    let account = loop {
        tokio::select! {
            result = &mut recovery => break result.expect("recovery finishes the account step"),
            request = fake.next() => {
                assert!(
                    !request.target.ends_with("/register"),
                    "recovery must not retry the lost create"
                );
                if request.method == "POST" {
                    logins += 1;
                }
                let response = as_reply(&request, &effect);
                request.json(response.0, response.1);
            }
        }
    };
    assert_eq!(logins, 1, "exactly one idempotent App Service login");
    assert_eq!(account.sender_mxid(), identity(&effect).0);
    assert_eq!(account.device_id(), identity(&effect).1);
    assert!(root(&state.path, &effect).join("initial").exists());
    assert!(root(&state.path, &effect).join("complete").exists());
    fake.close().await;
}

#[tokio::test]
async fn password_recovery_of_an_interrupted_account_step_stays_inspection_only() {
    let state = PrivateState::new();
    let effect = effect();
    let mut fake = Fake::start(true).await;
    let cancel = CancellationToken::new();
    let first = password_operation(&fake, &state.path, &effect).execute(&cancel);
    interrupt_register(&mut fake, &state.path, &effect, first, false).await;

    let recovery = password_operation(&fake, &state.path, &effect)
        .for_recovery()
        .execute(&cancel);
    tokio::pin!(recovery);
    let result = tokio::select! {
        result = &mut recovery => result,
        request = fake.next() => panic!("inspection emitted a request: {} {}", request.method, request.target),
    };
    assert_eq!(result.err(), Some(Error::Storage));
    assert!(!root(&state.path, &effect).join("initial").exists());
    assert!(!root(&state.path, &effect).join("complete").exists());
    fake.close().await;
}
