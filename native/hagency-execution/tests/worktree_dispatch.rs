//! Board #78's per-agent workspace settings never reach the host from a
//! requester. A request naming them is refused at verification, and an agent
//! whose stored record still carries them (admitted before that refusal) runs
//! every thread in its shared workspace, driven through the production
//! owned-dispatch path (`Operation::start`): no folder of the requester's is
//! made and no bootstrap command of theirs runs.
//!
//! The child (the offline `hagency-execution-probe`) writes its request log
//! (`owned-dispatch.requests`) into its OWN working directory and echoes that
//! directory back as `thread/start.cwd` (ADR-116), so where it ran is
//! observable.

#[path = "../../hagency-store/tests/common/mod.rs"]
mod common;
use common::*;
use hagency_core::authority::ProjectRequest;
use hagency_core::tasks::*;
use hagency_execution::{Host, Limits, Operation, Protocol, ordinary_launch_path};
use hagency_store::{DomainRepository, DomainStore, EffectOutcome};
use serde_json::json;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn binary() -> PathBuf {
    env!("CARGO_BIN_EXE_hagency-execution-probe").into()
}
fn limits() -> Limits {
    Limits {
        operation_ms: 25_000,
        response_ms: 1500,
    }
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn thread_cwd(requests: &Path) -> String {
    let text = fs::read_to_string(requests).unwrap();
    let values: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    values
        .iter()
        .find(|v| v["method"] == "thread/start")
        .unwrap()["params"]["cwd"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn native_stored_workspace_settings_never_reach_the_host() {
    let root = tempfile::tempdir().unwrap();

    // The agent's own workspace root. TS resolves the worktree repository from
    // the agent workdir (backend-v2.js:2057 `agent.workdir || agent.homeDir`),
    // so THIS directory is the repository `git worktree add` branches from.
    let work = root.path().join("agent-workspace");
    hagency_store::private::directory(&work).unwrap();
    let work = work.canonicalize().unwrap();
    git(&work, &["init"]);
    git(&work, &["config", "user.email", "t@e.com"]);
    git(&work, &["config", "user.name", "T"]);
    fs::write(work.join("README.md"), "base\n").unwrap();
    git(&work, &["add", "README.md"]);
    git(&work, &["commit", "-m", "base"]);
    let worktrees = root.path().join("worktrees");
    // A bootstrap that leaves a folder behind if it ever runs.
    let ran = root.path().join("bootstrap-ran");
    let bootstrap = json!(["git", "init", ran.to_string_lossy()]);

    // One agent's store: register, admit, approve, provision.
    let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    let pool = resource("pool", "seat", 1000);
    db.put_resource(&pool).unwrap();
    let plain = request("allocation", "Worker", &pool, 100);
    // A requester naming any workspace setting is refused before admission.
    for (key, setting) in [
        ("workspaceMode", json!("worktree")),
        ("worktreesDir", json!(worktrees.to_string_lossy())),
        ("worktreeBootstrap", bootstrap.clone()),
    ] {
        let mut value = serde_json::to_value(&plain).unwrap();
        value["agentDefinition"][key] = setting;
        let request: ProjectRequest = serde_json::from_value(value).unwrap();
        assert!(
            hagency_core::authority::verify_request(
                &registration(),
                request.clone(),
                observation(&request)
            )
            .is_err(),
            "{key}"
        );
    }
    let proof = proof(&plain);
    let engagement = db.admit(&proof, 1000).unwrap();
    db.approve("approved", &proof, 1000).unwrap();
    let effect = db.claim_effect().unwrap().unwrap();
    db.observe_effect(
        &effect.id,
        effect.fence,
        &EffectOutcome::Applied {
            receipt: "offline".into(),
        },
    )
    .unwrap();
    // Two thread sessions of the SAME engagement (differ only in thread root).
    db.register_session(&SessionBinding {
        id: "session-a".into(),
        engagement_id: engagement.id.clone(),
        room_id: "!project:example.test".into(),
        thread_root: Some("$thread-a".into()),
    })
    .unwrap();
    db.register_session(&SessionBinding {
        id: "session-b".into(),
        engagement_id: engagement.id.clone(),
        room_id: "!project:example.test".into(),
        thread_root: Some("$thread-b".into()),
    })
    .unwrap();
    db.register_workspace("work").unwrap();
    // The record an agent admitted before the refusal still carries.
    rusqlite::Connection::open(root.path().join("state/domain.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE engagements SET projection=json_set(projection,'$.workspaceMode','worktree','$.worktreesDir',?2,'$.worktreeBootstrap',json(?3)) WHERE id=?1",
            rusqlite::params![
                engagement.id,
                worktrees.to_string_lossy(),
                bootstrap.to_string()
            ],
        )
        .unwrap();
    let domain = DomainStore::start(db, 16).unwrap();

    // The shared engagement workspace is the ONLY registered root.
    let host = |env: BTreeMap<OsString, OsString>| {
        Host::new(
            binary(),
            binary(),
            env,
            BTreeMap::from([("work".into(), work.clone())]),
        )
        .unwrap()
    };
    let env = || {
        let mut env = BTreeMap::from([
            (OsString::from("PATH"), OsString::from("")),
            (
                OsString::from("HAGENCY_OFFLINE_MODE"),
                OsString::from("normal"),
            ),
            (
                OsString::from("HAGENCY_OPERATION_BUDGET_MS"),
                OsString::from(limits().operation_ms.to_string()),
            ),
        ]);
        if let Some(system) = std::env::var_os("SystemRoot") {
            env.insert(OsString::from("SystemRoot"), system);
        }
        env
    };
    let dispatch = |id: &str, session: &str, task: &str| DispatchInput {
        id: id.into(),
        session_id: session.into(),
        task_id: Some(task.into()),
        resources: vec![ResourceLease {
            id: "work".into(),
            exclusive: true,
        }],
        payload: json!({"instruction": "offline"}),
    };

    // Thread A: full dispatch to a clean completion.
    domain
        .create_canonical_task(
            "task-a".into(),
            "session-a".into(),
            "Thread A".into(),
            now(),
        )
        .await
        .unwrap();
    domain
        .enqueue_dispatch(dispatch("dispatch-a", "session-a", "task-a"))
        .await
        .unwrap();
    let cap_a = domain
        .claim_dispatch("runner-a".into(), now(), 60_000, 60_000, 1)
        .await
        .unwrap()
        .unwrap();
    let mut op_a = Operation::start(domain.clone(), cap_a, host(env()), limits()).unwrap();
    let report_a = op_a.wait().await.unwrap();
    assert_eq!(report_a.protocol, Protocol::Completed);

    // Thread B: another thread of the same agent.
    domain
        .create_canonical_task(
            "task-b".into(),
            "session-b".into(),
            "Thread B".into(),
            now(),
        )
        .await
        .unwrap();
    domain
        .enqueue_dispatch(dispatch("dispatch-b", "session-b", "task-b"))
        .await
        .unwrap();
    let cap_b = domain
        .claim_dispatch("runner-b".into(), now(), 60_000, 60_000, 1)
        .await
        .unwrap()
        .unwrap();
    let mut op_b = Operation::start(domain.clone(), cap_b, host(env()), limits()).unwrap();
    let report_b = op_b.wait().await.unwrap();
    assert_eq!(report_b.protocol, Protocol::Completed);

    // Both threads ran in the shared workspace; neither the requester's
    // worktrees folder nor their bootstrap's folder exists.
    assert!(!worktrees.exists(), "no worktrees folder may be created");
    assert!(!ran.exists(), "the stored bootstrap must never run");
    let shared = ordinary_launch_path(&work).unwrap();
    assert_eq!(thread_cwd(&work.join("owned-dispatch.requests")), shared);

    domain.shutdown().await.unwrap();
}
