//! Board #102: the live ACTION walk runs clean against this harness's fake.
//!
//! `mockup/live-actions.mjs` clicks and types the operator's real journey (log
//! in, create a resource configuration, decide an engagement, stop an agent,
//! run a task through its whole life, acknowledge an alert, read usage, end
//! access). Nothing above is an API call: the console page's own client is the
//! only thing that talks to the service, so a render-only assertion can never
//! stand in for it.
//!
//! The walk is validated HERE, against the same `Fixture` the other browser
//! lanes use, before it is pointed at a live rig. The contract this test
//! enforces is the walk's own:
//!   * every step reports `{step, ok, detail, screenshot}` — a step that dies
//!     mid-walk cannot silently shrink the journey;
//!   * a failure does not stop the walk (the reports after a failed step are
//!     present), so one run yields the whole console's state;
//!   * a step that failed must carry `gap` — the walk declares, in band, which
//!     steps the FAKE cannot exercise (and which the product cannot perform at
//!     all), so "runs clean" means "no unexplained failure", not "nothing
//!     failed".
//!
//! Feature-gated with the browser lane: it needs the real bundle
//! (`HAGENCY_NATIVE_CONSOLE_ASSETS` from `build:native`) and Chrome.

use salvo::prelude::*;
use serde_json::Value;
use std::{
    net::{SocketAddr, TcpListener as StdListener},
    path::PathBuf,
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

/// The steps the board names, in order — the walk must report every one.
const STEPS: [&str; 8] = [
    "1-login",
    "2-create-resource-configuration",
    "3-approve-pending-engagement",
    "4-agents-stop-then-start",
    "5-tasks-create-comment-transition-delete",
    "6-acknowledge-alert",
    "7-usage-renders-numbers",
    "8-end-access",
];

fn built() -> PathBuf {
    std::env::var_os("HAGENCY_NATIVE_CONSOLE_ASSETS")
        .map(PathBuf::from)
        .expect("native console qualification requires HAGENCY_NATIVE_CONSOLE_ASSETS from build:native; not a skipped test")
        .canonicalize()
        .expect("native console assets must resolve to an actual host path")
}
fn address() -> SocketAddr {
    StdListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
fn driver() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../mockup/live-actions.mjs")
}

#[tokio::test]
async fn native_console_live_actions_walk() {
    let address = address();
    let f = super::fixture::Fixture::new(address, Some(&built()));
    // The current resource wizard binds each new resource to a verified
    // engagement and its eligible Matrix project managers.
    f.domain
        .configure_coordinator(
            serde_json::from_value(serde_json::json!({
                "id":super::fixture::common::registration().fleet_id,
                "server":"example.test","owner":"@provider:example.test",
                "coordinator":"@coordinator:example.test","registrationGeneration":1,
                "delegationRevision":1,"delegationExpiresAtMs":super::fixture::now()+3600000,
                "state":"verified","allowSelfApproval":false,"coordinatorApprovalV1":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    hagency_store::private::write_new(
        &f.root.path().join("state/operator.token"),
        super::fixture::TOKEN.as_bytes(),
    )
    .unwrap();
    let acceptor = salvo::conn::TcpListener::new(address)
        .try_bind()
        .await
        .unwrap();
    let server = salvo::Server::new(acceptor);
    let handle = server.handle();
    let serving = tokio::spawn(server.try_serve(f.app.clone().router()));
    // The harness owns the ticket; the driver receives only the URL and mints
    // nothing itself. One outstanding ticket at a time.
    let url = hagency::console::client::access(&f.root.path().join("state"), address)
        .await
        .unwrap();
    let shots = f.root.path().join("shots");
    std::fs::create_dir_all(&shots).unwrap();
    let mut child =
        Command::new(std::env::var_os("HAGENCY_BROWSER_NODE").unwrap_or_else(|| "node".into()))
            .arg(driver())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("actual browser tooling must exist");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            format!(
                "{}\n",
                serde_json::json!({"base":format!("http://{address}"),"url":url,"shots":shots,"projectManager":"@owner:example.test"})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut steps: Vec<Value> = Vec::new();
    // 180s, like the sibling lanes: the walk drives the whole console on a
    // shared host where a healthy browser walk alone takes ~60s.
    tokio::time::timeout(Duration::from_secs(180), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            if let Ok(value) = serde_json::from_str::<Value>(&line)
                && value.get("step").is_some()
            {
                steps.push(value);
                println!("{line}");
                continue;
            }
            println!("{line}");
        }
    })
    .await
    .expect("the live action walk must finish inside its budget");
    let status = child.wait().await.unwrap();
    assert!(status.success(), "the live action walk exited {status}");

    // Every named step reported — a walk that died early is caught here rather
    // than passing on a short list.
    let reported: Vec<String> = steps
        .iter()
        .filter_map(|s| s["step"].as_str().map(str::to_owned))
        .collect();
    for step in STEPS {
        assert!(
            reported.contains(&step.to_owned()),
            "the walk never reported {step}; it reported {reported:?}"
        );
    }

    // A failure is only acceptable when the walk NAMED it a gap — which
    // includes the steps this fake cannot exercise. An unexplained failure is
    // the defect this assertion exists to catch.
    let unexplained: Vec<String> = steps
        .iter()
        .filter(|s| s["ok"] != Value::Bool(true) && s.get("gap").is_none())
        .map(|s| {
            format!(
                "{}: {}",
                s["step"].as_str().unwrap_or("?"),
                s["detail"].as_str().unwrap_or("(no detail)")
            )
        })
        .collect();
    assert!(
        unexplained.is_empty(),
        "the walk reported failures it did not attribute to a gap:\n  {}",
        unexplained.join("\n  ")
    );

    println!("PASS native console live action walk");
    handle.stop_graceful(Some(Duration::from_secs(2)));
    serving.await.unwrap().unwrap();
    f.close().await;
}
