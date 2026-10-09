//! ADR-193, slice 1: Octos dispatches on a fresh guardian-owned
//! `octos serve --stdio`, against the offline OUP peer the Host launches.
use super::*;

pub(super) fn octos_pool() -> Resource {
    serde_json::from_value(json!({
        "presetId":"pool","seatId":"seat","framework":"octos",
        "model":"kimi-k3","provider":"moonshot","octosProfile":"coding",
        "ceiling":{"tokens":1000,"period":"monthly"}
    }))
    .unwrap()
}
impl Fixture {
    /// The same probe, launched as `octos serve --stdio` by an Octos host. The
    /// host environment carries provider keys the launch must drop.
    pub(super) fn octos_host(&self, mode: &str) -> Host {
        let mut environment = BTreeMap::from([
            ("PATH".into(), "".into()),
            ("HAGENCY_OFFLINE_MODE".into(), mode.into()),
            (
                "HAGENCY_OPERATION_BUDGET_MS".into(),
                limits().operation_ms.to_string().into(),
            ),
            ("ANTHROPIC_API_KEY".into(), "offline-secret".into()),
            ("OPENAI_API_KEY".into(), "offline-secret".into()),
            ("MOONSHOT_API_KEY".into(), "offline-secret".into()),
        ]);
        if let Some(system) = std::env::var_os("SystemRoot") {
            environment.insert("SystemRoot".into(), system);
        }
        Host::new(
            binary(),
            binary(),
            environment,
            BTreeMap::from([("work".into(), self.work.clone())]),
        )
        .unwrap()
        .with_octos_runner(self.root.path().join("octos"))
        .unwrap()
        // The same probe answers as the task helper (decision 5).
        .with_task_helper(binary(), "127.0.0.1:13300".parse().unwrap())
        .unwrap()
    }
    fn octos_requests(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(self.work.join("owned-dispatch.requests"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn latest_octos_counts(&self) -> serde_json::Value {
        let counts: String = self
            .sql()
            .query_row(
                "SELECT latest_counts FROM usage_sources WHERE framework='octos'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str(&counts).unwrap()
    }
}
async fn run(f: &Fixture, mode: &str) -> hagency_execution::Report {
    let mut operation = Operation::start(
        f.domain.clone(),
        f.cap.clone(),
        f.octos_host(mode),
        limits(),
    )
    .unwrap();
    operation.wait().await.unwrap()
}

/// ADR-193: one `octos serve --stdio` on the dispatch's workspace and the
/// agent's own private instance directory, network denied, no provider key.
/// The session is fresh and the profile the resource names; the reply is the
/// turn's last text once Octos is idle, and usage is recorded at the terminal
/// and at idle, so the dispatch settles Completed.
#[tokio::test]
async fn native_octos_dispatch_completes_at_idle_with_its_reply() {
    let f = Fixture::configured_resource(false, false, octos_pool());
    let report = run(&f, "normal").await;
    assert_eq!(report.protocol, Protocol::Completed);
    assert_eq!(report.text.as_deref(), Some("octos fixture reply"));
    let Cleanup::Observed(cleanup) = report.cleanup else {
        panic!("cleanup observation missing");
    };
    assert!(cleanup.scope.leader_exited);
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        assert!(cleanup.scope.whole_tree_stopped);
        assert!(!report.retains_process_custody());
        assert_eq!(report.failure, None);
        assert_eq!(report.settlement, Settlement::Completed);
        assert_eq!(f.state(), "completed");
        assert_eq!(f.count("SELECT COUNT(*) FROM resource_leases"), 0);
    }
    let argv: Vec<String> =
        serde_json::from_slice(&fs::read(f.work.join("owned-dispatch.argv")).unwrap()).unwrap();
    let work = f.work.to_str().unwrap();
    assert_eq!(
        argv[..6],
        [
            "serve",
            "--stdio",
            "--cwd",
            work,
            "--no-network",
            "--instance-data-dir"
        ]
    );
    let instance = PathBuf::from(&argv[6]);
    assert_eq!(
        instance.parent(),
        Some(f.root.path().join("octos").as_path())
    );
    assert_eq!(instance.file_name().unwrap().len(), 16);
    hagency_store::private::directory(&instance).unwrap();
    // Hagency's own settings (decision 3), private in the instance directory,
    // and exactly what the child read: no project or user config.
    let config = instance.join("hagency-octos-config.json");
    assert_eq!(argv[7..], ["--config", config.to_str().unwrap()]);
    assert_eq!(
        hagency_store::private::read_secret(&config).unwrap(),
        hagency_runtime::octos::CONFIG
    );
    assert_eq!(
        fs::read(f.work.join("owned-dispatch.config")).unwrap(),
        hagency_runtime::octos::CONFIG
    );
    // Not a checkout: Octos's policy file is there, and no `.git` was made.
    assert!(f.work.join(".octos-workspace.toml").exists());
    assert!(!f.work.join(".git").exists());
    let environment: serde_json::Value =
        serde_json::from_slice(&fs::read(f.work.join("owned-dispatch.environment")).unwrap())
            .unwrap();
    assert_eq!(
        environment,
        json!({"no_model_download":"1","provider_keys":[],"home":false})
    );
    let requests = f.octos_requests();
    let open = &requests[2]["params"];
    let session = open["session_id"].as_str().unwrap();
    assert!(session.starts_with("coding:local:hagency-"));
    assert_eq!(open["profile_id"], "coding");
    assert_eq!(open["cwd"], work);
    assert_eq!(
        requests[1]["params"]["update"],
        json!({"mode":"workspace_write","network":"deny","approval_policy":"on-request"})
    );
    // Hagency's task tools, on the session before its turn (decision 5):
    // the helper's own tools as `hagency.<tool>`, none outward.
    let register = &requests[3]["params"];
    assert_eq!(requests[3]["method"], "peer/tools/register");
    assert_eq!(register["session_id"], session);
    assert!(register.get("generic_tools").is_none());
    let names: Vec<&str> = register["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        hagency_runtime::task_mcp::owned_task_tools(false, false)
            .iter()
            .map(|tool| format!("hagency.{tool}"))
            .collect::<Vec<_>>()
    );
    for tool in register["tools"].as_array().unwrap() {
        assert_eq!(tool["outward"], false);
        let read = [
            "hagency.get_task",
            "hagency.list_tasks",
            "hagency.read_conversation",
        ]
        .contains(&tool["name"].as_str().unwrap());
        assert_eq!(tool["risk"], if read { "read" } else { "act" });
    }
    let start = &requests[4]["params"];
    assert_eq!(requests[4]["method"], "turn/start");
    assert_eq!(start["session_id"], session);
    assert!(hagency_runtime::octos::uuid(
        start["turn_id"].as_str().unwrap()
    ));
    // The one turn is the task-tool guidance, then the dispatch payload
    // byte for byte, as for Claude Code.
    let prompt = fs::read_to_string(f.work.join("owned-dispatch.prompt")).unwrap();
    let payload =
        hagency_core::canonical::encode_payload(&json!({"instruction":"do the offline work",
        "task_id":"impostor","cwd":"/model/override","model":"model-override","done":true}))
        .unwrap();
    assert!(prompt.ends_with(&format!("\n\nAssigned task input:\n{payload}")));
    assert!(prompt.contains("hagency_complete_task_with_reply"));
    // The capability reaches the helper only, never Octos.
    let names: Vec<String> =
        serde_json::from_slice(&fs::read(f.work.join("owned-dispatch.environment-names")).unwrap())
            .unwrap();
    assert!(!names.iter().any(|name| name.starts_with("HAGENCY_RUNNER")));
    let usage = report.usage_status();
    assert!(usage.bound && usage.attached && usage.failure.is_none());
    assert_eq!(usage.acknowledged, 2);
    assert_eq!(
        f.count("SELECT COUNT(*) FROM usage_sources WHERE framework='octos'"),
        1
    );
    assert_eq!(
        f.latest_octos_counts(),
        json!({"input":10,"output":7,"cacheWrite":30,"cacheRead":20})
    );
    drop(report);
    f.domain.shutdown().await.unwrap();
}

/// ADR-193 decision 5: an Octos host tool call reaches the dispatch's task
/// helper with the dispatch's own task and capability, and its answer, or its
/// refusal, goes back to Octos as `peer/tool/result`. The dispatch goes on.
#[tokio::test]
async fn native_octos_host_tools_reach_the_task_helper() {
    let f = Fixture::configured_resource(false, false, octos_pool());
    let report = run(&f, "host-tools").await;
    assert_eq!(report.protocol, Protocol::Completed);
    let results: Vec<_> = f
        .octos_requests()
        .into_iter()
        .filter(|request| request["method"] == "peer/tool/result")
        .collect();
    assert_eq!(results.len(), 2);
    let ok = &results[0]["params"];
    assert_eq!(ok["call_id"], "ptc-1");
    assert_eq!(ok["ok"], true);
    assert_eq!(ok["data"]["tool"], "get_task");
    assert_eq!(ok["data"]["arguments"], json!({"task_id":"offline-task"}));
    assert_eq!(ok["data"]["task"], "task");
    assert_eq!(ok["data"]["capability"], true);
    assert_eq!(ok["data"]["address"], "127.0.0.1:13300");
    let refused = &results[1]["params"];
    assert_eq!(refused["call_id"], "ptc-2");
    assert_eq!(refused["ok"], false);
    assert_eq!(
        refused["error"],
        json!({"kind":"tool_error","message":"offline refusal"})
    );
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        assert_eq!(report.settlement, Settlement::Completed);
    }
    drop(report);
    f.domain.shutdown().await.unwrap();
}

/// ADR-193, operator decision 3: sub-agents keep working after the turn; the
/// kernel's continuation turn belongs to the same dispatch, the reply is the
/// last turn's, and the session's totals (with the sub-agent) close usage.
#[tokio::test]
async fn native_octos_background_work_settles_with_the_last_turns_reply() {
    let f = Fixture::configured_resource(false, false, octos_pool());
    let report = run(&f, "background").await;
    assert_eq!(report.protocol, Protocol::Completed);
    assert_eq!(report.text.as_deref(), Some("octos background reply"));
    assert_eq!(report.usage_status().acknowledged, 3);
    assert_eq!(
        f.latest_octos_counts(),
        json!({"input":915,"output":610,"cacheWrite":90,"cacheRead":60})
    );
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        assert_eq!(report.settlement, Settlement::Completed);
    }
    drop(report);
    f.domain.shutdown().await.unwrap();
}

/// ADR-193 decision 6: an errored terminal settles as a failed turn, and its
/// error code is kept with the attempt; it is never a reply.
#[tokio::test]
async fn native_octos_errored_idle_settles_as_a_protocol_failure() {
    let f = Fixture::configured_resource(false, false, octos_pool());
    let report = run(&f, "error").await;
    assert_eq!(report.protocol, Protocol::Failed);
    assert_eq!(report.text, None);
    assert_eq!(report.turn_failure, "Errored: provider_unavailable");
    assert_eq!(report.failure, Some(Failure::Protocol));
    f.quarantined();
    drop(report);
    f.domain.shutdown().await.unwrap();
}

/// ADR-193, slice 1: an Octos approval reaches no owner card yet. It refuses
/// the dispatch and the child is stopped, never left waiting for an answer.
#[tokio::test]
async fn native_octos_approval_without_cards_refuses_the_dispatch() {
    let f = Fixture::configured_resource(false, false, octos_pool());
    let report = run(&f, "approval").await;
    assert_eq!(report.failure, Some(Failure::UnsupportedApproval));
    assert!(matches!(report.cleanup, Cleanup::Observed(_)));
    f.quarantined();
    drop(report);
    f.domain.shutdown().await.unwrap();
}

/// ADR-193: an Octos host refuses a Codex dispatch by name, and a Codex host
/// an Octos dispatch, before any workspace or process work.
#[tokio::test]
async fn native_octos_host_and_codex_host_refuse_each_others_dispatch() {
    let f = Fixture::new();
    let report = run(&f, "normal").await;
    assert_eq!(
        report.failure,
        Some(Failure::UnsupportedRunner {
            framework: "codex".into()
        })
    );
    assert!(!f.work.join("owned-dispatch.argv").exists());
    drop(report);
    f.domain.shutdown().await.unwrap();

    let f = Fixture::configured_resource(false, false, octos_pool());
    let mut operation = Operation::start(
        f.domain.clone(),
        f.cap.clone(),
        f.host("usage-gate", "not-work", true),
        limits(),
    )
    .unwrap();
    let report = operation.wait().await.unwrap();
    assert_eq!(
        report.failure,
        Some(Failure::UnsupportedRunner {
            framework: "octos".into()
        })
    );
    assert_eq!(
        report.settlement,
        Settlement::Negative(OwnedObservation::Unstarted)
    );
    drop(report);
    f.domain.shutdown().await.unwrap();
}

/// ADR-193 decision 3: in a workspace that is a Git checkout, the one file
/// Octos writes there, its workspace policy, is kept out of Git: added to the
/// checkout's own exclude list after what is there, before Octos starts.
#[tokio::test]
async fn native_octos_workspace_policy_stays_out_of_git() {
    let f = Fixture::configured_resource(false, false, octos_pool());
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.email", "t@e.com"],
        vec!["config", "user.name", "T"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(&args)
                .current_dir(&f.work)
                .status()
                .unwrap()
                .success()
        );
    }
    // `git init` writes `info/` only from its templates, which not every
    // install has.
    fs::create_dir_all(f.work.join(".git/info")).unwrap();
    fs::write(f.work.join(".git/info/exclude"), "*.log").unwrap();
    let report = run(&f, "normal").await;
    assert_eq!(report.protocol, Protocol::Completed);
    assert!(f.work.join(".octos-workspace.toml").exists());
    assert_eq!(
        fs::read_to_string(f.work.join(".git/info/exclude")).unwrap(),
        "*.log\n/.octos-workspace.toml\n"
    );
    assert!(
        std::process::Command::new("git")
            .args(["check-ignore", "--quiet", ".octos-workspace.toml"])
            .current_dir(&f.work)
            .status()
            .unwrap()
            .success()
    );
    drop(report);
    f.domain.shutdown().await.unwrap();
}

/// ADR-193 decision 7: only an Octos resource names a profile, and an Octos
/// resource without one is not provisionable. An Octos host takes no managed
/// account and no local folder binding.
#[test]
fn native_octos_resource_names_its_profile() {
    let pool = octos_pool();
    pool.validate().unwrap();
    assert!(pool.provisionable());
    let mut unnamed = octos_pool();
    unnamed.octos_profile = None;
    unnamed.validate().unwrap();
    assert!(!unnamed.provisionable());
    for profile in ["", "-lead", "a:b", "../coding", "a b"] {
        let mut invalid = octos_pool();
        invalid.octos_profile = Some(profile.into());
        assert!(invalid.validate().is_err(), "{profile}");
    }
    let mut codex = resource("pool", "seat", 1000);
    codex.octos_profile = Some("coding".into());
    assert!(codex.validate().is_err());
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    hagency_store::private::directory(&work).unwrap();
    let work = work.canonicalize().unwrap();
    let host = || {
        Host::new(
            binary(),
            binary(),
            BTreeMap::from([("PATH".into(), "".into())]),
            BTreeMap::from([("work".into(), work.clone())]),
        )
        .unwrap()
    };
    assert!(host().with_octos_runner("relative".into()).is_err());
    let octos = host().with_octos_runner(root.path().join("octos")).unwrap();
    assert_eq!(octos.runner(), hagency_execution::Runner::Octos);
    assert!(octos.with_claude_runner().is_err());
    assert!(
        host()
            .with_claude_runner()
            .unwrap()
            .with_octos_runner(root.path().join("octos"))
            .is_err()
    );
}
