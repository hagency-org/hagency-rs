//! `hagency.runtime/v1` sessions against a fake plugin started by the real
//! guardian: separate response and turn-idle timeouts, the tool capability,
//! and host-executed task tools with duplicate and out-of-catalog refusals.
use hagency_plugin::{RuntimeLaunch, RuntimeOutcome, RuntimePluginRef, ToolDescriptor};
use hagency_runtime::owned::{
    Cleanup, OwnedPluginSession, PluginSessionError, PluginTimeouts, PluginToolHost, ToolFuture,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const FAKE_PLUGIN: &str = r#"#!/bin/sh
# Fake hagency.runtime/v1 plugin. FAKE_MODE: plain | silent | tool | busy | mute.
# busy: accepts the turn, then only heartbeats until cancelled.
# mute: answers initialize, then never answers turn/start (a lost response).
results=0
beat=
trap '[ -n "$beat" ] && kill "$beat" 2>/dev/null' EXIT
reply() { printf '{"jsonrpc":"2.0","method":"turn/result","params":{"dispatchId":"dispatch","turnId":"turn-1","outcome":"completed","finalText":"%s","error":null,"usage":null}}\n' "$1"; }
call() { printf '{"jsonrpc":"2.0","method":"tool/call","params":{"dispatchId":"dispatch","turnId":"turn-1","callId":"%s","tool":"%s","input":{"id":"task","call_id":"%s","body":"done"}}}\n' "$1" "$2" "$1"; }
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$FAKE_LOG"
  case "$line" in
    *'"method":"initialize"'*)
      if [ "$FAKE_MODE" = tool ]; then caps='["turn","tool"]'; else caps='["turn"]'; fi
      printf '{"jsonrpc":"2.0","id":"initialize","result":{"protocol":"hagency.runtime/v1","ready":true,"capabilities":%s}}\n' "$caps"
      ;;
    *'"method":"turn/start"'*)
      [ "$FAKE_MODE" = mute ] && continue
      printf '{"jsonrpc":"2.0","id":"turn-start","result":{"accepted":true}}\n'
      case "$FAKE_MODE" in
        busy)
          ( while :; do
              printf '{"jsonrpc":"2.0","method":"progress","params":{"dispatchId":"dispatch","turnId":"turn-1","text":"working"}}\n'
              sleep 0.2
            done ) &
          beat=$!
          ;;
        silent) sleep 3; reply "quiet turn done" ;;
        tool) call c1 complete_task_with_reply ;;
        *) reply "plain answer" ;;
      esac
      ;;
    *'"method":"tool/result"'*)
      results=$((results + 1))
      case "$results" in
        1) call c1 complete_task_with_reply ;;
        2) call c2 shell_exec ;;
        *) reply "tool turn done" ;;
      esac
      ;;
    *'"method":"turn/cancel"'*)
      [ -n "$beat" ] && kill "$beat" 2>/dev/null; beat=
      ;;
    *'"method":"shutdown"'*)
      [ -n "$beat" ] && kill "$beat" 2>/dev/null; beat=
      printf '{"jsonrpc":"2.0","id":"shutdown","result":{"accepted":true}}\n'
      exit 0
      ;;
  esac
done
"#;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let plugin = root.join("plugin");
        fs::write(&plugin, FAKE_PLUGIN).unwrap();
        fs::set_permissions(&plugin, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir_all(root.join("workspace")).unwrap();
        Self { _dir: dir, root }
    }
    fn log(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("log"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn sent(&self, method: &str) -> Vec<Value> {
        self.log()
            .into_iter()
            .filter(|frame| frame["method"] == method)
            .collect()
    }
}

fn guardian() -> PathBuf {
    env!("CARGO_BIN_EXE_hagency-runtime-probe").into()
}
fn launch(root: &Path) -> RuntimeLaunch {
    RuntimeLaunch {
        runtime: RuntimePluginRef {
            plugin_id: "fake.runtime".into(),
            adapter_id: "fake".into(),
            package_sha256: "a".repeat(64),
            settings_sha256: "b".repeat(64),
        },
        framework: "fake".into(),
        executable: root.join("plugin"),
        arguments: vec![],
        package_dir: root.into(),
        readiness: None,
    }
}
fn session(fixture: &Fixture, mode: &str) -> OwnedPluginSession {
    let environment = BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("FAKE_LOG".into(), fixture.root.join("log").into_os_string()),
        ("FAKE_MODE".into(), mode.into()),
    ]);
    OwnedPluginSession::spawn(
        &guardian(),
        &launch(&fixture.root),
        fixture.root.join("workspace"),
        environment,
    )
    .expect("fake plugin starts")
}
fn timeouts(turn_idle_ms: u64) -> PluginTimeouts {
    PluginTimeouts {
        response: Duration::from_millis(2_000),
        turn_idle: Duration::from_millis(turn_idle_ms),
    }
}

#[derive(Default)]
struct RecordingTools {
    prepared: bool,
    calls: Vec<(String, Value)>,
}
impl PluginToolHost for RecordingTools {
    fn prepare(&mut self) -> ToolFuture<'_, Result<Vec<ToolDescriptor>, ()>> {
        Box::pin(async move {
            self.prepared = true;
            Ok(vec![ToolDescriptor {
                name: "complete_task_with_reply".into(),
                description: "Finish the assigned task with a Matrix reply".into(),
                input_schema: json!({"type":"object"}),
                read_only: false,
            }])
        })
    }
    fn call<'a>(
        &'a mut self,
        tool: &'a str,
        input: Value,
    ) -> ToolFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            self.calls.push((tool.to_owned(), input));
            Ok(json!({"status":"done"}))
        })
    }
}

async fn run(
    session: &mut OwnedPluginSession,
    timeouts: PluginTimeouts,
    tools: Option<&mut dyn PluginToolHost>,
) -> Result<hagency_runtime::owned::PluginTurn, PluginSessionError> {
    run_with(session, timeouts, tools, Arc::new(AtomicBool::new(false))).await
}
async fn run_with(
    session: &mut OwnedPluginSession,
    timeouts: PluginTimeouts,
    tools: Option<&mut dyn PluginToolHost>,
    cancel: Arc<AtomicBool>,
) -> Result<hagency_runtime::owned::PluginTurn, PluginSessionError> {
    session
        .run_turn(
            "dispatch".into(),
            "turn-1".into(),
            "{\"instruction\":\"x\"}".into(),
            "/workspace".into(),
            Some(json!({"target":"agent","ownership":"existing"})),
            timeouts,
            cancel,
            tools,
        )
        .await
}
fn whole_tree_stopped(cleanup: Cleanup) -> bool {
    matches!(cleanup, Cleanup::Observed(report) if report.scope.whole_tree_stopped)
}

/// A cancellation sends `turn/cancel` then `shutdown` and never yields a turn;
/// a lost response is a timeout, never a completed turn. In both cases the
/// session ends only with an observed whole-tree stop, which is what the
/// host's settlement requires before it may claim anything.
#[tokio::test]
async fn native_plugin_runtime_cancel_and_cleanup_unknown_refuse_settlement() {
    // Cancellation while the runtime is busy (heartbeating, no result).
    let fixture = Fixture::new();
    let mut busy = session(&fixture, "busy");
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        flag.store(true, Ordering::Release);
    });
    let started = std::time::Instant::now();
    let error = run_with(&mut busy, timeouts(60_000), None, cancel)
        .await
        .unwrap_err();
    assert!(matches!(error, PluginSessionError::Cancelled), "{error:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cancellation must not wait for the turn idle bound"
    );
    assert_eq!(fixture.sent("turn/cancel").len(), 1);
    assert_eq!(fixture.sent("shutdown").len(), 1);
    assert!(whole_tree_stopped(busy.stop()), "{:?}", busy.cleanup());

    // A lost response: the turn is never accepted.
    let fixture = Fixture::new();
    let mut mute = session(&fixture, "mute");
    let error = run(&mut mute, timeouts(60_000), None).await.unwrap_err();
    assert!(matches!(error, PluginSessionError::Timeout), "{error:?}");
    assert!(fixture.sent("turn/start").len() == 1);
    assert!(fixture.sent("shutdown").is_empty());
    // The child is still alive here: cleanup is not yet proven.
    assert!(!whole_tree_stopped(mute.cleanup()));
    assert!(whole_tree_stopped(mute.stop()), "{:?}", mute.cleanup());
}

#[tokio::test]
async fn a_quiet_turn_is_bounded_by_turn_idle_not_by_the_response_timeout() {
    let fixture = Fixture::new();
    let mut quiet = session(&fixture, "silent");
    let turn = run(&mut quiet, timeouts(5_000), None)
        .await
        .expect("a 3 s quiet turn fits a 5 s idle bound despite the 2 s response bound");
    assert_eq!(turn.outcome, RuntimeOutcome::Completed);
    assert_eq!(turn.final_text.as_deref(), Some("quiet turn done"));
    quiet.stop();

    let fixture = Fixture::new();
    let mut stalled = session(&fixture, "silent");
    let error = run(&mut stalled, timeouts(1_000), None).await.unwrap_err();
    assert!(matches!(error, PluginSessionError::Timeout), "{error:?}");
    stalled.stop();
}

#[tokio::test]
async fn host_executes_task_tools_once_and_refuses_duplicates_and_unknown_tools() {
    let fixture = Fixture::new();
    let mut plugin = session(&fixture, "tool");
    let mut tools = RecordingTools::default();
    let turn = run(&mut plugin, timeouts(5_000), Some(&mut tools))
        .await
        .expect("tool turn completes");
    plugin.stop();
    assert_eq!(turn.final_text.as_deref(), Some("tool turn done"));
    assert!(tools.prepared);
    assert_eq!(
        tools.calls.len(),
        1,
        "a repeated call_id is never executed twice"
    );
    assert_eq!(tools.calls[0].0, "complete_task_with_reply");
    assert_eq!(tools.calls[0].1["call_id"], "c1");

    let start = &fixture.sent("turn/start")[0];
    assert_eq!(
        start["params"]["tools"][0]["name"],
        "complete_task_with_reply"
    );
    let results = fixture.sent("tool/result");
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["params"]["callId"], "c1");
    assert_eq!(
        results[0]["params"]["result"],
        json!({"ok":true,"output":{"status":"done"}})
    );
    assert_eq!(results[1]["params"]["result"]["ok"], false);
    assert!(
        results[1]["params"]["result"]["error"]
            .as_str()
            .unwrap()
            .contains("duplicate call_id c1")
    );
    assert_eq!(results[2]["params"]["callId"], "c2");
    assert!(
        results[2]["params"]["result"]["error"]
            .as_str()
            .unwrap()
            .contains("shell_exec is not a Hagency task tool")
    );
}

#[tokio::test]
async fn a_runtime_without_the_tool_capability_never_starts_host_tools() {
    let fixture = Fixture::new();
    let mut plugin = session(&fixture, "plain");
    let mut tools = RecordingTools::default();
    let turn = run(&mut plugin, timeouts(5_000), Some(&mut tools))
        .await
        .expect("plain turn completes");
    plugin.stop();
    assert_eq!(turn.final_text.as_deref(), Some("plain answer"));
    assert!(
        !tools.prepared,
        "the helper is not started for a runtime that cannot use it"
    );
    let start = &fixture.sent("turn/start")[0];
    assert!(
        start["params"].get("tools").is_none(),
        "turn/start frame unchanged: {start}"
    );
}

#[tokio::test]
async fn tool_calls_are_refused_when_the_host_serves_no_tools() {
    let fixture = Fixture::new();
    let mut plugin = session(&fixture, "tool");
    let turn = run(&mut plugin, timeouts(5_000), None)
        .await
        .expect("the turn still completes");
    plugin.stop();
    assert_eq!(turn.final_text.as_deref(), Some("tool turn done"));
    assert!(
        fixture.sent("turn/start")[0]["params"]
            .get("tools")
            .is_none()
    );
    for result in fixture.sent("tool/result") {
        assert_eq!(result["params"]["result"]["ok"], false, "{result}");
    }
}
