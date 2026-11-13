//! A plugin runtime's readiness probe runs before the dispatch starts. A
//! provider that cannot take a turn (for Herdr: no server, no target agent)
//! requeues the dispatch as never started, so its session is never
//! quarantined and nothing is launched; a ready probe starts it as before.
use super::*;
use hagency_plugin::PluginService;
use hagency_store::PluginRuntimeQualification;

/// The probe's answer is read from `probe-answer` beside the package, so one
/// test can flip it. The runtime appends a line to `launched` on every start.
fn package(root: &std::path::Path) -> PathBuf {
    let package = root.join("package");
    fs::create_dir_all(package.join("bin")).unwrap();
    fs::write(
        package.join("hagency-plugin.toml"),
        r#"schema = "hagency.plugin/v1"
id = "example.ready"
name = "Readiness fixture"
version = "1.0.0"
min_hagency_version = "0.1.0"
platforms = ["linux-x86_64"]

[[runtimes]]
id = "fake"
framework = "fake"
command = ["bin/runtime"]
protocol = "hagency.runtime/v1"
readiness = ["bin/probe"]
"#,
    )
    .unwrap();
    let answer = root.join("probe-answer");
    let launched = root.join("launched");
    for (name, body) in [
        (
            "probe",
            format!("#!/bin/sh\ncat >/dev/null\ncat '{}'\n", answer.display()),
        ),
        (
            "runtime",
            format!("#!/bin/sh\necho run >> '{}'\n", launched.display()),
        ),
    ] {
        let path = package.join("bin").join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
    package
}

struct Plugin {
    root: tempfile::TempDir,
    work: PathBuf,
    domain: DomainStore,
    service: PluginService,
}
impl Plugin {
    fn new(ready: bool) -> (Self, RunnerCapability) {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        hagency_store::private::directory(&work).unwrap();
        let work = work.canonicalize().unwrap();
        let state = root.path().join("state");
        let package = package(root.path());
        Self::answer(root.path(), ready);
        let mut db = DomainRepository::open(&state).unwrap();
        let service = PluginService::open_with_guardian(&state, binary()).unwrap();
        let record = service.link(&package).unwrap();
        service.enable(&record.id).unwrap();
        let settings = record.settings_sha256.clone();
        let identity = hagency_core::project::RuntimePluginIdentity {
            plugin_id: record.id.clone(),
            adapter_id: "fake".into(),
            package_sha256: record.package_sha256.clone(),
            settings_sha256: settings.clone(),
        };
        db.register(&registration()).unwrap();
        let mut pool = resource("pool", "seat", 1000);
        pool.framework = "fake".into();
        pool.model = "fake".into();
        pool.provider = Some("fake".into());
        pool.reasoning = None;
        pool.roles = vec!["coding".into()];
        pool.runtime_plugin = Some(identity);
        db.put_plugin_runtime_qualification(&PluginRuntimeQualification {
            plugin_id: record.id.clone(),
            adapter_id: "fake".into(),
            package_sha256: record.package_sha256,
            settings_sha256: settings,
            framework: "fake".into(),
            model: "fake".into(),
            provider: "fake".into(),
            reasoning: None,
            roles: vec!["coding".into()],
            evidence_digest: "c".repeat(64),
            qualified_at: 1,
            evidence: json!({"passed": true}),
        })
        .unwrap();
        db.put_resource(&pool).unwrap();
        let proof = proof(&request("allocation", "Worker", &pool, 100));
        let e = db.admit(&proof, 1000).unwrap();
        db.approve("approved", &proof, 1000).unwrap();
        let effect = db.claim_effect().unwrap().unwrap();
        db.observe_effect(
            &effect.id,
            effect.fence,
            &EffectOutcome::Applied {
                receipt: "offline_provisioned".into(),
            },
        )
        .unwrap();
        db.register_session(&SessionBinding {
            id: "session".into(),
            engagement_id: e.id,
            room_id: "!project:example.test".into(),
            thread_root: Some("$thread".into()),
        })
        .unwrap();
        db.register_workspace("work").unwrap();
        db.create_canonical_task("task", "session", "Readiness task", now())
            .unwrap();
        db.enqueue_dispatch(&DispatchInput {
            id: "dispatch".into(),
            session_id: "session".into(),
            task_id: Some("task".into()),
            resources: vec![ResourceLease {
                id: "work".into(),
                exclusive: true,
            }],
            payload: json!({"instruction": "work"}),
        })
        .unwrap();
        let cap = db
            .claim_dispatch("owned_host", now(), 60_000, 60_000, 1)
            .unwrap()
            .unwrap();
        let domain = DomainStore::start(db, 16).unwrap();
        (
            Self {
                root,
                work,
                domain,
                service,
            },
            cap,
        )
    }
    fn answer(root: &std::path::Path, ready: bool) {
        let body = if ready {
            json!({"ready": true})
        } else {
            json!({"ready": false, "reason": "herdr server is not running"})
        };
        fs::write(root.join("probe-answer"), body.to_string()).unwrap();
    }
    fn host(&self) -> Host {
        Host::new(
            binary(),
            binary(),
            BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
            BTreeMap::from([("work".into(), self.work.clone())]),
        )
        .unwrap()
        .with_plugin_service(self.service.clone())
        .unwrap()
    }
    fn launches(&self) -> usize {
        fs::read_to_string(self.root.path().join("launched")).map_or(0, |text| text.lines().count())
    }
    fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.root.path().join("state/domain.sqlite3")).unwrap()
    }
    fn dispatch(&self) -> (String, u64) {
        self.sql()
            .query_row(
                "SELECT state,not_before FROM runner_dispatches WHERE id='dispatch'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }
    fn quarantined(&self) -> bool {
        self.sql()
            .query_row(
                "SELECT quarantined FROM runner_sessions WHERE id='session'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }
}

/// Not ready: no runtime launch, the dispatch is queued again behind a
/// backoff, the attempt is a spawn failure, and the session stays usable.
/// Then the same dispatch, claimed after the backoff with a ready probe, is
/// launched: the probe is what held it back.
#[tokio::test]
async fn native_plugin_runtime_not_ready_requeues_without_starting() {
    let (f, cap) = Plugin::new(false);
    let claimed_at = now();
    let report = Operation::start(f.domain.clone(), cap.clone(), f.host(), limits())
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert_eq!(
        report.failure,
        Some(Failure::RuntimeNotReady {
            reason: "herdr server is not running".into()
        })
    );
    assert_eq!(f.launches(), 0, "the runtime never started");
    let (state, not_before) = f.dispatch();
    assert_eq!(state, "queued");
    assert!(
        not_before >= claimed_at + 30_000,
        "requeued behind the not-ready backoff"
    );
    assert!(!f.quarantined(), "a deferred dispatch quarantines nothing");
    let outcome: String = f
        .sql()
        .query_row(
            "SELECT outcome FROM runner_attempts WHERE dispatch_id='dispatch' AND fence=?1",
            [cap.fence],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(outcome, "spawn_failed");

    // Control: once ready, the next claim after the backoff launches it.
    Plugin::answer(f.root.path(), true);
    let next = f
        .domain
        .claim_dispatch("owned_host".into(), not_before + 1, 60_000, 60_000, 1)
        .await
        .unwrap()
        .expect("the requeued dispatch is claimable after its backoff");
    assert_eq!(next.dispatch_id, "dispatch");
    let _ = Operation::start(f.domain.clone(), next, f.host(), limits())
        .unwrap()
        .wait()
        .await;
    assert_eq!(f.launches(), 1, "a ready probe lets the runtime start");
    f.domain.shutdown().await.unwrap();
}
