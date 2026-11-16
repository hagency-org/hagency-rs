//! ADR-196: a plugin runtime that advertises `tool` is offered the
//! coordination catalog only behind the owner gate. A fake runtime drives the
//! real host: each coordination `tool/call` must raise a `plugin/tool` owner
//! approval before the task helper (the offline probe, which counts the calls
//! it answers) hears of it.
use super::*;
use hagency_core::approvals::*;
use hagency_execution::{ApprovalHost, ApprovalRequests};
use hagency_plugin::PluginService;
use hagency_store::PluginRuntimeQualification;

/// Reads its calls from `plan` (one `callId tool` per line, sent in order, one
/// per `tool/result`), then ends the turn. Every host line is logged.
fn package(root: &std::path::Path) -> PathBuf {
    let package = root.join("package");
    fs::create_dir_all(package.join("bin")).unwrap();
    fs::write(
        package.join("hagency-plugin.toml"),
        r#"schema = "hagency.plugin/v1"
id = "example.coordination"
name = "Coordination fixture"
version = "1.0.0"
min_hagency_version = "0.1.0"
platforms = ["linux-x86_64"]

[[runtimes]]
id = "fake"
framework = "fake"
command = ["bin/runtime"]
protocol = "hagency.runtime/v1"
"#,
    )
    .unwrap();
    let script = format!(
        r#"#!/bin/sh
exec 3< '{plan}'
next() {{
  if read -r cid tool <&3; then
    printf '{{"jsonrpc":"2.0","method":"tool/call","params":{{"dispatchId":"%s","turnId":"%s","callId":"%s","tool":"%s","input":{{"call_id":"%s","note":"x"}}}}}}\n' "$d" "$t" "$cid" "$tool" "$cid"
  else
    printf '{{"jsonrpc":"2.0","method":"turn/result","params":{{"dispatchId":"%s","turnId":"%s","outcome":"completed","finalText":"plugin done","error":null,"usage":null}}}}\n' "$d" "$t"
  fi
}}
while IFS= read -r line; do
  printf '%s\n' "$line" >> '{log}'
  case "$line" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":"initialize","result":{{"protocol":"hagency.runtime/v1","ready":true,"capabilities":["turn","tool"]}}}}\n'
      ;;
    *'"method":"turn/start"'*)
      d=${{line#*\"dispatchId\":\"}}; d=${{d%%\"*}}
      t=${{line#*\"turnId\":\"}}; t=${{t%%\"*}}
      printf '{{"jsonrpc":"2.0","id":"turn-start","result":{{"accepted":true}}}}\n'
      next
      ;;
    *'"method":"tool/result"'*) next ;;
    *'"method":"shutdown"'*)
      printf '{{"jsonrpc":"2.0","id":"shutdown","result":{{"accepted":true}}}}\n'
      exit 0
      ;;
  esac
done
"#,
        plan = root.join("plan").display(),
        log = root.join("log").display(),
    );
    let path = package.join("bin/runtime");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    package
}

struct Plugin {
    root: tempfile::TempDir,
    work: PathBuf,
    domain: DomainStore,
    service: PluginService,
}
impl Plugin {
    fn new(plan: &[&str]) -> (Self, RunnerCapability) {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        hagency_store::private::directory(&work).unwrap();
        let work = work.canonicalize().unwrap();
        let state = root.path().join("state");
        fs::write(root.path().join("plan"), plan.join("\n") + "\n").unwrap();
        let package = package(root.path());
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
        approval_fixture::bindings(&mut db, &e.id);
        db.resolve_verified_matrix_session(
            &SessionBinding {
                id: "session".into(),
                engagement_id: e.id,
                room_id: "!project:example.test".into(),
                thread_root: Some("$thread".into()),
            },
            now(),
        )
        .unwrap();
        db.register_workspace("work").unwrap();
        db.create_canonical_task("task", "session", "Coordination task", now())
            .unwrap();
        db.enqueue_dispatch(&DispatchInput {
            id: "dispatch".into(),
            session_id: "session".into(),
            task_id: Some("task".into()),
            resources: vec![ResourceLease {
                id: "work".into(),
                exclusive: true,
            }],
            payload: json!({"instruction": "coordinate"}),
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
    /// Coordination on, the probe as the task helper; approvals optional.
    fn host(&self, approvals: Option<ApprovalHost>) -> Host {
        let mut host = Host::new(
            binary(),
            binary(),
            BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
            BTreeMap::from([("work".into(), self.work.clone())]),
        )
        .unwrap()
        .with_plugin_service(self.service.clone())
        .unwrap()
        .with_task_helper(binary(), "127.0.0.1:13300".parse().unwrap())
        .unwrap()
        .with_coordination_tools()
        .unwrap();
        if let Some(approvals) = approvals {
            host = host.with_approvals(approvals).unwrap();
        }
        host
    }
    fn log(&self) -> String {
        fs::read_to_string(self.root.path().join("log")).unwrap_or_default()
    }
    /// The host's `tool/result` lines, in order.
    fn results(&self) -> Vec<serde_json::Value> {
        self.log()
            .lines()
            .filter(|line| line.contains("\"method\":\"tool/result\""))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.root.path().join("state/domain.sqlite3")).unwrap()
    }
    fn count(&self, query: &str) -> u64 {
        self.sql().query_row(query, [], |r| r.get(0)).unwrap()
    }
    fn state(&self) -> String {
        self.sql()
            .query_row(
                "SELECT state FROM runner_dispatches WHERE id='dispatch'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }
}

fn policy() -> ApprovalHost {
    ApprovalHost::new(4, 2, 20_000, 1500).unwrap()
}
fn start(f: &Plugin, cap: RunnerCapability, host: Host) -> (Operation, ApprovalRequests) {
    let mut operation = Operation::start(f.domain.clone(), cap, host, limits()).unwrap();
    let notices = operation.take_approval_requests().unwrap();
    (operation, notices)
}
async fn notice(notices: &mut ApprovalRequests) -> hagency_execution::ApprovalNotice {
    tokio::time::timeout(Duration::from_secs(10), notices.recv())
        .await
        .expect("no owner approval notice")
        .expect("notice channel closed")
}
async fn choose(f: &Plugin, id: &str, choice: ApprovalChoice) {
    let card = f.domain.private_approval(id.into()).await.unwrap();
    f.domain
        .observe_owner_verdict(OwnerVerdictObservation {
            request_id: id.into(),
            request_digest: card.digest,
            binding_generation: card.binding_generation,
            server_name: "example.test".into(),
            room_id: card.room_id,
            sender_mxid: card.owner_mxid,
            event_id: format!("${id}"),
            encrypted: true,
            choice,
        })
        .await
        .unwrap();
}
/// With the coordination catalog offered, the plugin sees the eight names; a
/// `delegate_task` call raises a `plugin/tool` card with Approve once and Deny
/// only, the helper is not called while the owner has not answered, and
/// `Approve once` runs it exactly once.
#[tokio::test]
async fn native_plugin_coordination_call_requires_owner_approval() {
    let (f, cap) = Plugin::new(&["c1 delegate_task"]);
    let (mut operation, mut notices) = start(&f, cap.clone(), f.host(Some(policy())));
    let request = notice(&mut notices).await;
    // The catalog the host offered at turn/start: the eight names, no graph.
    let start_line = f
        .log()
        .lines()
        .find(|line| line.contains("\"method\":\"turn/start\""))
        .unwrap()
        .to_owned();
    for name in hagency_runtime::task_mcp::COORDINATION_TOOLS {
        assert!(
            start_line.contains(&format!("\"name\":\"{name}\"")),
            "{name}"
        );
    }
    assert!(!start_line.contains("graph"), "{start_line}");
    // The card, before any answer.
    let approval = f
        .domain
        .private_approval(request.request_id.clone())
        .await
        .unwrap();
    assert_eq!(approval.method, "plugin/tool");
    assert_eq!(approval.params["toolName"], "delegate_task");
    assert_eq!(approval.params["input"]["call_id"], "c1");
    assert!(!approval.summary.reusable_scope);
    let card = f
        .domain
        .private_approval_card(request.request_id.clone(), request.owner_expires_at)
        .await
        .unwrap();
    let detail = &card.content()["com.agentchat.approval"];
    assert_eq!(detail["runtime"], "plugin");
    assert_eq!(detail["tool_name"], "delegate_task");
    let actions: Vec<_> = detail["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|action| action["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(actions, ["approve_once", "deny"]);
    // The dispatch is parked on the owner and the helper has not been called:
    // the runtime has been given no result.
    assert_eq!(f.state(), "parked");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(f.results().is_empty(), "{}", f.log());
    assert_eq!(f.state(), "parked");

    choose(&f, &request.request_id, ApprovalChoice::Once).await;
    let report = operation.wait().await.unwrap();
    assert_eq!(report.protocol, Protocol::Completed, "{:?}", report.failure);
    let results = f.results();
    assert_eq!(results.len(), 1, "{}", f.log());
    let result = &results[0]["params"]["result"];
    assert_eq!(result["ok"], true, "{result}");
    // Exactly one helper call ever happened, and it was this one.
    assert_eq!(result["output"]["tool"], "delegate_task");
    assert_eq!(result["output"]["calls"], 1);
    assert_eq!(
        f.count("SELECT COUNT(*) FROM approval_responses WHERE write_accepted=1"),
        1
    );
    assert_eq!(f.count("SELECT COUNT(*) FROM approval_grants"), 0);
    assert_eq!(
        f.count("SELECT COUNT(*) FROM owner_approvals WHERE state='applied'"),
        1
    );
    drop(report);
    f.domain.shutdown().await.unwrap();
}

/// Deny and owner-wait expiry each return a failed `tool/result` and call
/// nothing.
#[tokio::test]
async fn native_plugin_coordination_denial_and_expiry_call_nothing() {
    // Deny.
    let (f, cap) = Plugin::new(&["c1 delegate_task"]);
    let (mut operation, mut notices) = start(&f, cap, f.host(Some(policy())));
    let request = notice(&mut notices).await;
    choose(&f, &request.request_id, ApprovalChoice::Deny).await;
    let report = operation.wait().await.unwrap();
    assert_eq!(report.protocol, Protocol::Completed, "{:?}", report.failure);
    let results = f.results();
    assert_eq!(results.len(), 1, "{}", f.log());
    assert_eq!(results[0]["params"]["result"]["ok"], false);
    assert!(
        results[0]["params"]["result"]["error"]
            .as_str()
            .unwrap()
            .contains("refused by the owner"),
        "{}",
        results[0]
    );
    assert!(!f.log().contains("\"calls\""), "{}", f.log());
    assert_eq!(
        f.count("SELECT COUNT(*) FROM owner_approvals WHERE state='applied'"),
        1
    );
    drop(report);
    f.domain.shutdown().await.unwrap();

    // Expiry: nobody answers within the owner wait.
    let (f, cap) = Plugin::new(&["c1 delegate_task"]);
    let short = ApprovalHost::new(4, 2, 1_000, 1500).unwrap();
    let (mut operation, mut notices) = start(&f, cap, f.host(Some(short)));
    let _ = notice(&mut notices).await;
    let report = operation.wait().await.unwrap();
    assert_eq!(report.protocol, Protocol::Completed, "{:?}", report.failure);
    let results = f.results();
    assert_eq!(results.len(), 1, "{}", f.log());
    assert_eq!(results[0]["params"]["result"]["ok"], false);
    assert!(
        results[0]["params"]["result"]["error"]
            .as_str()
            .unwrap()
            .contains("owner wait ended"),
        "{}",
        results[0]
    );
    assert!(!f.log().contains("\"calls\""), "{}", f.log());
    drop(report);
    f.domain.shutdown().await.unwrap();
}

/// `comment_task` needs no card. A repeated `call_id` raises no second card and
/// is never run twice.
#[tokio::test]
async fn native_plugin_coordination_repeated_call_id_raises_one_card() {
    let (f, cap) = Plugin::new(&["c0 comment_task", "c1 delegate_task", "c1 delegate_task"]);
    let (mut operation, mut notices) = start(&f, cap, f.host(Some(policy())));
    // The first card is c1's: c0 (comment_task) raised none.
    let request = notice(&mut notices).await;
    let approval = f
        .domain
        .private_approval(request.request_id.clone())
        .await
        .unwrap();
    assert_eq!(approval.params["toolName"], "delegate_task");
    choose(&f, &request.request_id, ApprovalChoice::Once).await;
    let report = operation.wait().await.unwrap();
    assert_eq!(report.protocol, Protocol::Completed, "{:?}", report.failure);
    let results = f.results();
    assert_eq!(results.len(), 3, "{}", f.log());
    assert_eq!(
        results[0]["params"]["result"]["output"]["tool"],
        "comment_task"
    );
    assert_eq!(results[0]["params"]["result"]["output"]["calls"], 1);
    assert_eq!(
        results[1]["params"]["result"]["output"]["tool"],
        "delegate_task"
    );
    assert_eq!(results[1]["params"]["result"]["output"]["calls"], 2);
    assert_eq!(results[2]["params"]["result"]["ok"], false);
    assert!(
        results[2]["params"]["result"]["error"]
            .as_str()
            .unwrap()
            .contains("duplicate call_id"),
        "{}",
        results[2]
    );
    assert_eq!(f.count("SELECT COUNT(*) FROM owner_approvals"), 1);
    drop(report);
    f.domain.shutdown().await.unwrap();
}

/// Without an `ApprovalHost` the coordination names are not offered, and a
/// forged `tool/call` is refused without reaching the helper or the owner.
#[tokio::test]
async fn native_plugin_coordination_is_not_offered_without_approvals() {
    let (f, cap) = Plugin::new(&["c1 delegate_task"]);
    let host = f.host(None);
    let report = Operation::start(f.domain.clone(), cap, host, limits())
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert_eq!(report.protocol, Protocol::Completed, "{:?}", report.failure);
    let start_line = f
        .log()
        .lines()
        .find(|line| line.contains("\"method\":\"turn/start\""))
        .unwrap()
        .to_owned();
    assert!(start_line.contains("\"name\":\"get_task\""), "{start_line}");
    for name in hagency_runtime::task_mcp::COORDINATION_TOOLS {
        assert!(!start_line.contains(name), "{name} offered: {start_line}");
    }
    let results = f.results();
    assert_eq!(results.len(), 1, "{}", f.log());
    assert_eq!(results[0]["params"]["result"]["ok"], false);
    assert!(
        results[0]["params"]["result"]["error"]
            .as_str()
            .unwrap()
            .contains("is not a Hagency task tool"),
        "{}",
        results[0]
    );
    assert!(!f.log().contains("\"calls\""), "{}", f.log());
    assert_eq!(f.count("SELECT COUNT(*) FROM owner_approvals"), 0);
    drop(report);
    f.domain.shutdown().await.unwrap();
}
