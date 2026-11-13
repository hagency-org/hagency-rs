//! A plugin runtime that finishes a turn with plain final text (no
//! `complete_task_with_reply` tool, e.g. the Herdr runtime) must still answer
//! the Matrix room: the owned completion moves the task to Done and records
//! that text as the execution's final reply. Sessions without a Matrix route,
//! and every built-in runner, keep the plain completion exactly as before.
mod common;
use common::*;
use hagency_core::{project::RuntimePluginIdentity, replies::*, tasks::*};
use hagency_store::{DomainRepository, DomainStore, EffectOutcome, PluginRuntimeQualification};
use serde_json::json;
use std::collections::BTreeSet;

struct Fixture {
    root: tempfile::TempDir,
    db: DomainRepository,
    cap: RunnerCapability,
    fingerprint: String,
}

/// An owned dispatch on `session`, started and ready to complete. With
/// `matrix`, the session is a verified Matrix project-room route; without it,
/// the session is a plain registered session with no room to answer. With
/// `plugin`, the resource runs a qualified plugin runtime; without it, a
/// built-in runner.
fn fixture(matrix: bool) -> Fixture {
    fixture_on(matrix, true)
}
fn fixture_on(matrix: bool, plugin: bool) -> Fixture {
    // The async writer completes with the wall clock, so the claimed
    // capability must be current at that clock, not at a synthetic epoch.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let root = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    let mut pool = resource("pool", "seat", 1000);
    if plugin {
        pool.framework = "herdr".into();
        pool.model = "herdr".into();
        pool.roles = vec!["coding".into()];
        pool.runtime_plugin = Some(RuntimePluginIdentity {
            plugin_id: "example.runtime".into(),
            adapter_id: "herdr".into(),
            package_sha256: "a".repeat(64),
            settings_sha256: "b".repeat(64),
        });
        db.put_plugin_runtime_qualification(&PluginRuntimeQualification {
            plugin_id: "example.runtime".into(),
            adapter_id: "herdr".into(),
            package_sha256: "a".repeat(64),
            settings_sha256: "b".repeat(64),
            framework: "herdr".into(),
            model: "herdr".into(),
            provider: "herdr".into(),
            reasoning: None,
            roles: vec!["coding".into()],
            evidence_digest: "c".repeat(64),
            qualified_at: 1,
            evidence: json!({"passed": true}),
        })
        .unwrap();
        pool.provider = Some("herdr".into());
        pool.reasoning = None;
    }
    db.put_resource(&pool).unwrap();
    let proof = proof(&request("allocated", "Worker", &pool, 100));
    let engagement = db.admit(&proof, 1000).unwrap();
    db.approve("approved", &proof, 1000).unwrap();
    let effect = db.claim_effect().unwrap().unwrap();
    db.observe_effect(
        &effect.id,
        effect.fence,
        &EffectOutcome::Applied {
            receipt: "offline_identity".into(),
        },
    )
    .unwrap();
    let binding = SessionBinding {
        id: "session".into(),
        engagement_id: engagement.id.clone(),
        room_id: "!project:example.test".into(),
        thread_root: None,
    };
    if matrix {
        db.observe_matrix_transport(
            &MatrixTransportObservation {
                engagement_id: engagement.id.clone(),
                registration_generation: 1,
                generation: 1,
                sender_mxid: "@worker:example.test".into(),
                device_id: "DEVICE_1".into(),
            },
            now,
        )
        .unwrap();
        db.observe_matrix_room(
            &MatrixRoomObservation {
                engagement_id: engagement.id.clone(),
                registration_generation: 1,
                transport_generation: 1,
                room_id: binding.room_id.clone(),
                generation: 1,
                privacy: RoomPrivacy::Group {},
                joined: BTreeSet::from([
                    "@worker:example.test".into(),
                    "@owner:example.test".into(),
                ]),
                invite_only: true,
                encrypted: false,
            },
            now,
        )
        .unwrap();
        db.resolve_verified_matrix_session(&binding, now).unwrap();
    } else {
        db.register_session(&binding).unwrap();
    }
    db.register_workspace("workspace").unwrap();
    db.create_canonical_task("task", "session", "Plugin turn", now)
        .unwrap();
    db.enqueue_dispatch(&DispatchInput {
        id: "dispatch".into(),
        session_id: "session".into(),
        task_id: Some("task".into()),
        resources: vec![ResourceLease {
            id: "workspace".into(),
            exclusive: true,
        }],
        payload: json!({"instruction":"Reply with exactly: PLUGIN_OK"}),
    })
    .unwrap();
    let cap = db
        .claim_dispatch("runner", now, 60_000, 120_000, 1)
        .unwrap()
        .unwrap();
    let scope = db.owned_dispatch_scope(&cap, now).unwrap();
    let started = db
        .start_owned_dispatch(&cap, scope.fingerprint(), now)
        .unwrap();
    let fingerprint = started.fingerprint().to_owned();
    Fixture {
        root,
        db,
        cap,
        fingerprint,
    }
}

fn sql(root: &tempfile::TempDir) -> rusqlite::Connection {
    rusqlite::Connection::open(root.path().join("state/domain.sqlite3")).unwrap()
}
fn task_status(root: &tempfile::TempDir) -> String {
    sql(root)
        .query_row(
            "SELECT json_extract(config,'$.status') FROM canonical_tasks WHERE id='task'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}
fn replies(root: &tempfile::TempDir) -> Vec<(String, String, String)> {
    let db = sql(root);
    let mut statement = db
        .prepare("SELECT source_dispatch_id,state,body FROM final_replies ORDER BY rowid")
        .unwrap();
    statement
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Completes the owned dispatch with `output` and returns the final-reply
/// claim the driver would deliver.
async fn complete(
    f: Fixture,
    output: serde_json::Value,
) -> (tempfile::TempDir, Option<ReplyClaim>) {
    let store = DomainStore::start(f.db, 8).unwrap();
    store
        .complete_owned_dispatch(f.cap.clone(), f.fingerprint, output)
        .await
        .expect("a plain-text completion is accepted");
    let claim = store
        .claim_final_reply_for_dispatch(f.cap, 60_000)
        .await
        .unwrap();
    store.shutdown().await.unwrap();
    (f.root, claim)
}

#[tokio::test]
async fn plugin_final_text_becomes_the_matrix_reply() {
    let (root, claim) = complete(fixture(true), json!({"upstream_text":"  PLUGIN_OK\n"})).await;
    assert!(claim.is_some(), "the driver can claim the reply to deliver");
    assert_eq!(task_status(&root), "done");
    assert_eq!(
        replies(&root),
        vec![("dispatch".into(), "claimed".into(), "PLUGIN_OK".into())],
        "exactly one reply, trimmed, bound to the completing dispatch"
    );
}

#[tokio::test]
async fn completion_without_text_records_no_reply() {
    for output in [
        json!({}),
        json!({"upstream_text":""}),
        json!({"upstream_text":"   "}),
    ] {
        let (root, claim) = complete(fixture(true), output.clone()).await;
        assert!(claim.is_none(), "no reply for {output}");
        assert!(replies(&root).is_empty(), "no reply row for {output}");
        assert_eq!(
            task_status(&root),
            "in_progress",
            "the task keeps the plain completion for {output}"
        );
    }
}

#[tokio::test]
async fn session_without_a_matrix_route_keeps_the_plain_completion() {
    let (root, claim) = complete(fixture(false), json!({"upstream_text":"PLUGIN_OK"})).await;
    assert!(claim.is_none());
    assert!(replies(&root).is_empty());
    assert_eq!(task_status(&root), "in_progress");
}

#[tokio::test]
async fn long_final_text_is_bounded() {
    let text = "x".repeat(20_000);
    let (root, claim) = complete(fixture(true), json!({ "upstream_text": text })).await;
    assert!(claim.is_some());
    let (_, _, body) = replies(&root).remove(0);
    assert_eq!(body.chars().count(), 16_000);
}

/// A built-in runner (Codex, Claude, Octos) answers the room only through
/// `complete_task_with_reply`; its plain upstream text is never a reply.
#[tokio::test]
async fn builtin_runner_final_text_is_not_a_reply() {
    let (root, claim) = complete(
        fixture_on(true, false),
        json!({"upstream_text":"CODEX_TEXT"}),
    )
    .await;
    assert!(claim.is_none());
    assert!(replies(&root).is_empty());
    assert_eq!(task_status(&root), "in_progress");
}
