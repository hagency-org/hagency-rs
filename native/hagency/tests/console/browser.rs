use super::fixture::*;
use salvo::prelude::*;
use serde_json::json;
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

fn built() -> PathBuf {
    let path = std::env::var_os("HAGENCY_NATIVE_CONSOLE_ASSETS").map(PathBuf::from).expect("native console qualification requires HAGENCY_NATIVE_CONSOLE_ASSETS from build:native; not a skipped test");
    // The bundle may be spelled through a symlinked ancestor (this host's
    // `.../home/hl` -> `hl.noindex`; macOS's `/var` -> `/private/var`). The
    // product deliberately refuses such an alias — `Assets::load` walks every
    // component with `open_dir_nofollow`, because HTTP never resolves a
    // filesystem path — so the spawned executable is handed the actual host
    // path. The in-process fixtures already canonicalize for the same reason.
    path.canonicalize()
        .expect("native console assets must resolve to an actual host path")
}
fn address() -> SocketAddr {
    StdListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
fn node() -> PathBuf {
    std::env::var_os("HAGENCY_BROWSER_NODE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("node"))
}
fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../mockup/scripts/native-console-browser.mjs")
}

#[tokio::test]
async fn native_coordinator_ledger_browser_shares_resource_allocation_and_shows_delivered_refusals()
{
    let address = address();
    let f = Fixture::new(address, Some(&built()));
    hagency_store::private::write_new(
        &f.root.path().join("state/operator.token"),
        TOKEN.as_bytes(),
    )
    .unwrap();
    let fleet = common::registration().fleet_id;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let policy = json!({"id":fleet,"server":"example.test","owner":"@provider:example.test","coordinator":"@coordinator:example.test",
        "registrationGeneration":1,"delegationRevision":1,"delegationExpiresAtMs":now+3600000,"state":"verified","allowSelfApproval":false,"coordinatorApprovalV1":true});
    f.domain
        .configure_coordinator(serde_json::from_value(policy).unwrap())
        .await
        .unwrap();
    let resource = native_resource("browser_ledger_resource");
    f.domain.put_resource(resource.clone()).await.unwrap();
    let definition = common::request("delivered_browser", "RefusedVisible", &resource, 100);
    let digest =
        hagency_core::canonical::digest(&serde_json::to_value(&definition).unwrap()).unwrap();
    let mut payload = serde_json::to_value(definition).unwrap();
    payload["coordinatorApproval"] = json!({"context":{"version":1,"commandId":"browser_decision","serverEngagementId":fleet,"registrationGeneration":1,
        "delegationRevision":1,"actor":"@coordinator:example.test","issuedAtMs":now,"expiresAtMs":now+300000},
        "request":{"id":"delivered_browser","revision":1,"serverEngagementId":fleet,"projectId":"project_one","projectRevision":1,"resourceAllocationId":"browser_grant",
        "projectOwner":"@owner:example.test","requester":"@owner:example.test","definitionDigest":digest,"requestedTokens":100},"allocatedTokens":100});
    assert_eq!(
        f.domain
            .receive_coordinator_agent(fleet.clone(), payload)
            .await
            .unwrap()["state"],
        "refused"
    );
    let settled_resource = native_resource("browser_settlement_resource");
    f.domain
        .put_resource(settled_resource.clone())
        .await
        .unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(90);
    let access = hagency_store::ResourceConfigurationAccess::new(until, Default::default());
    let grant=serde_json::from_value(json!({"id":"settlement_grant","serverEngagementId":fleet,"resourceId":settled_resource.id(),"revision":1,"allocatedTokens":500,"eligibleManagers":["@owner:example.test"]})).unwrap();
    f.domain
        .contribute_resource(access.prepare_contribution(grant, until).unwrap())
        .await
        .unwrap();
    let context = |id: &str| json!({"version":1,"commandId":id,"serverEngagementId":fleet,"registrationGeneration":1,"delegationRevision":1,"actor":"@coordinator:example.test","issuedAtMs":now,"expiresAtMs":now+300000});
    let definition = json!({"name":"Settlement project","roomId":"!project:example.test","ownerDmRoomId":"!private:example.test"});
    let project=serde_json::from_value(json!({"context":context("settlement_project"),"request":{"id":"settlement_project_request","revision":1,"serverEngagementId":fleet,"projectId":"project_one","owner":"@owner:example.test","requester":"@owner:example.test","definitionDigest":hagency_core::canonical::digest(&definition).unwrap(),"resourceAllocations":["settlement_grant"]}})).unwrap();
    f.domain
        .approve_coordinator_project(project, definition)
        .await
        .unwrap();
    let request = common::request(
        "settlement_browser",
        "SettlementVisible",
        &settled_resource,
        200,
    );
    let mut observation = common::observation(&request);
    observation.observed_at_ms = now;
    let proof = hagency_core::authority::verify_request(
        &common::registration(),
        request.clone(),
        observation,
    )
    .unwrap();
    f.domain
        .verify_coordinator_project(proof.clone())
        .await
        .unwrap();
    f.domain.admit(proof.clone(), now).await.unwrap();
    let command=serde_json::from_value(json!({"context":context("settlement_allocation"),"request":{"id":"settlement_browser","revision":1,"serverEngagementId":fleet,"projectId":"project_one","projectRevision":1,"resourceAllocationId":"settlement_grant","projectOwner":"@owner:example.test","requester":"@owner:example.test","definitionDigest":hagency_core::canonical::digest(&serde_json::to_value(&request).unwrap()).unwrap(),"requestedTokens":200},"allocatedTokens":200})).unwrap();
    let settled_agent = f
        .domain
        .approve_coordinated_agent(command, proof)
        .await
        .unwrap();
    // This fixture never starts an execution; retirement cancels provisioning.
    f.domain
        .revoke("retire_settlement_fixture".into(), settled_agent.id.clone())
        .await
        .unwrap();
    let server = Server::new(TcpListener::new(address).try_bind().await.unwrap());
    let handle = server.handle();
    let serving = tokio::spawn(server.try_serve(f.app.clone().router()));
    let url = hagency::console::client::access(&f.root.path().join("state"), address)
        .await
        .unwrap();
    let mut child = Command::new(node())
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../mockup/scripts/native-coordinator-ledger-browser.mjs"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(format!("{}\n",json!({"base":format!("http://{address}"),"url":url,"fleet":fleet,"resource":resource.id(),
        "output":PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/coordinator-ledger-browser")})).as_bytes()).await.unwrap();
    let output = tokio::time::timeout(Duration::from_secs(90), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let grants = f
        .domain
        .server_engagement_resources(fleet.clone(), String::new(), 50)
        .await
        .unwrap();
    assert_eq!(grants.len(), 2);
    let edited = grants
        .iter()
        .find(|g| g["resourceId"] == resource.id())
        .unwrap();
    assert_eq!(edited["allocatedTokens"], 4000);
    assert_eq!(edited["revision"], 2);
    let settlement = f
        .domain
        .coordinator_settlement(settled_agent.id)
        .await
        .unwrap();
    assert_eq!(settlement["state"], "settled");
    assert_eq!(settlement["releasedTokens"], 200);
    let authority = f
        .domain
        .coordinator_authority(fleet)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&authority).unwrap()["state"],
        "suspended"
    );
    assert_eq!(u64::from(authority.delegation_revision), 2);
    handle.stop_graceful(Some(Duration::from_secs(2)));
    serving.await.unwrap().unwrap();
    f.close().await;
}
/// Wait for the spawned executable to admit on `address`; when it refuses
/// to start, SAY WHY — the child's exit status and its stderr. A bare
/// "process exited" line made #83 need this re-run at all. The stderr read
/// uses a blocking thread: the pipe belongs to a dead child, so there is
/// nothing left to await.
async fn await_admission(server: &mut tokio::process::Child, address: SocketAddr) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            return String::new();
        }
        if let Some(status) = server.try_wait().unwrap() {
            // The child is dead, so the pipe reads to EOF immediately —
            // awaiting it cannot hang; it just drains what was written.
            let mut stderr = String::new();
            if let Some(mut pipe) = server.stderr.take() {
                use tokio::io::AsyncReadExt;
                let _ = pipe.read_to_string(&mut stderr).await;
            }
            return format!(
                "native console process exited before admission: {status}; stderr:\n{stderr}"
            );
        }
        assert!(
            std::time::Instant::now() < deadline,
            "native console startup exceeded the 10s budget"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn native_console_browser() {
    let address = address();
    let f = Fixture::new(address, Some(&built()));
    hagency_store::private::write_new(
        &f.root.path().join("state/operator.token"),
        TOKEN.as_bytes(),
    )
    .unwrap();
    let acceptor = TcpListener::new(address).try_bind().await.unwrap();
    let server = Server::new(acceptor);
    let handle = server.handle();
    let serving = tokio::spawn(server.try_serve(f.app.clone().router()));
    let url = hagency::console::client::access(&f.root.path().join("state"), address)
        .await
        .unwrap();
    let mut child = Command::new(node())
        .arg(script())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("actual browser tooling must exist");
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(
            format!(
                "{}\n",
                json!({"base":format!("http://{address}"),"url":url,"engagement":f.engagement})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut created = false;
    // 180s, like the regression lane: the four Chrome lanes run in parallel
    // on a shared 16-core host (load 100+), where a healthy walk alone
    // takes ~60s and the old 60s budget starved mid-logout.
    tokio::time::timeout(Duration::from_secs(180), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            if line == "CREATE_ENGAGEMENT" {
                assert!(!created);
                let id = f.new_engagement().await;
                input
                    .write_all(format!("{}\n", json!({"engagement":id})).as_bytes())
                    .await
                    .unwrap();
                created = true;
            } else {
                println!("{line}");
            }
        }
        assert!(
            child.wait().await.unwrap().success(),
            "real browser assertions failed"
        );
    })
    .await
    .expect("real browser deadline");
    assert!(
        created,
        "browser must discover an engagement created while it is running"
    );
    handle.stop_graceful(Some(Duration::from_secs(2)));
    serving.await.unwrap().unwrap();
    f.close().await;
}

#[tokio::test]
async fn native_console_executable() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    let binary = env!("CARGO_BIN_EXE_hagency");
    let init = Command::new(binary)
        .args(["init", "--state-dir"])
        .arg(&state)
        .env_clear()
        .env("PATH", &empty_path)
        .output()
        .await
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let (db, engagement) = seed(&state);
    drop(db);
    let address = address();
    let mut server = Command::new(binary)
        .args(["serve", "--state-dir"])
        .arg(&state)
        .args(["--listen", &address.to_string(), "--console-assets"])
        .arg(built())
        .env_clear()
        .env("PATH", &empty_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if tokio::net::TcpStream::connect(address).await.is_ok() {
                break;
            }
            assert!(
                server.try_wait().unwrap().is_none(),
                "{}",
                await_admission(&mut server, address).await
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native executable startup");
    let link = Command::new(binary)
        .args(["console-access", "--state-dir"])
        .arg(&state)
        .args(["--listen", &address.to_string()])
        .env_clear()
        .env("PATH", &empty_path)
        .output()
        .await
        .unwrap();
    assert!(
        link.status.success(),
        "native access command must work without Node on PATH"
    );
    let url = String::from_utf8(link.stdout).unwrap().trim().to_owned();
    assert!(!url.contains(TOKEN));
    let mut browser = Command::new(node())
        .arg(script())
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    browser.stdin.take().unwrap().write_all(format!("{}\n", json!({"base":format!("http://{address}"),"url":url,"engagement":engagement,"executable":true})).as_bytes()).await.unwrap();
    // The walk grew with the roster/project-sides/verdict lanes (#43/#44),
    // and the shared host runs at high load: 45s starved the executable
    // walk mid-logout while its own driver was healthy. 90s is still a
    // bounded budget for a walk whose healthy finish is ~60s.
    assert!(
        tokio::time::timeout(Duration::from_secs(90), browser.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    server.kill().await.unwrap();
    server.wait().await.unwrap();
}

fn resource_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../mockup/scripts/native-console-resources-browser.mjs")
}

fn agents_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../mockup/scripts/native-console-agents-browser.mjs")
}

/// Board #106, the acceptance: ONE served binary, the operator's own journey
/// through the UI — stop a serving agent, watch the row become stopped and
/// offer Start, then start it and watch it serve again. The console page's
/// client is the only thing that talks to the service; no step calls an API.
///
/// A served binary, not the in-process fixture: the live run that found this
/// used the packaged bundle behind the real `serve`, and the defect was
/// exactly a control the page never rendered.
#[tokio::test]
async fn native_console_agents_stop_then_start() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    let binary = env!("CARGO_BIN_EXE_hagency");
    let init = Command::new(binary)
        .args(["init", "--state-dir"])
        .arg(&state)
        .env_clear()
        .env("PATH", &empty_path)
        .output()
        .await
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    // The seeded UsageWorker holds a live `started` dispatch, so the stop has
    // real work to fence and the store records the durable stopped row
    // (the same state the stop/start store test pins).
    let (db, engagement) = seed(&state);
    drop(db);
    let address = address();
    let mut server = Command::new(binary)
        .args(["serve", "--state-dir"])
        .arg(&state)
        .args(["--listen", &address.to_string(), "--console-assets"])
        .arg(built())
        .env_clear()
        .env("PATH", &empty_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if tokio::net::TcpStream::connect(address).await.is_ok() {
                break;
            }
            assert!(
                server.try_wait().unwrap().is_none(),
                "{}",
                await_admission(&mut server, address).await
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native executable startup");
    let link = Command::new(binary)
        .args(["console-access", "--state-dir"])
        .arg(&state)
        .args(["--listen", &address.to_string()])
        .env_clear()
        .env("PATH", &empty_path)
        .output()
        .await
        .unwrap();
    assert!(
        link.status.success(),
        "native access command must work without Node on PATH"
    );
    let url = String::from_utf8(link.stdout).unwrap().trim().to_owned();
    assert!(!url.contains(TOKEN));
    let mut browser = Command::new(node())
        .arg(agents_script())
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("actual browser tooling must exist");
    browser
        .stdin
        .take()
        .unwrap()
        .write_all(
            format!(
                "{}\n",
                json!({"base":format!("http://{address}"),"url":url,"engagement":engagement})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    // 120s: the shared host runs a browser walk at ~60s when healthy, and a
    // starved walk must not be mistaken for the defect this test exists for.
    assert!(
        tokio::time::timeout(Duration::from_secs(120), browser.wait())
            .await
            .unwrap()
            .unwrap()
            .success(),
        "the served-binary stop-then-start walk failed"
    );
    server.kill().await.unwrap();
    server.wait().await.unwrap();
}
#[tokio::test]
async fn native_console_resources_browser() {
    let address = address();
    let f = Fixture::new(address, Some(&built()));
    let resource = native_resource("private_browser_pool");
    f.domain.put_resource(resource.clone()).await.unwrap();
    // Brief 18: the fixture's usage pool (a bound usage source with real
    // observations) is the MEASURED headroom arm; `resource` itself is the
    // unmeasured arm (a declared ceiling, no engagement, no observation).
    let measured = common::resource("private_usage_pool", "private_usage_seat", 1000);
    hagency_store::private::write_new(
        &f.root.path().join("state/operator.token"),
        TOKEN.as_bytes(),
    )
    .unwrap();
    let server = Server::new(TcpListener::new(address).try_bind().await.unwrap());
    let handle = server.handle();
    let serving = tokio::spawn(server.try_serve(f.app.clone().router()));
    let url = hagency::console::client::access(&f.root.path().join("state"), address)
        .await
        .unwrap();
    let mut child = Command::new(node())
        .arg(resource_script())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("actual browser tooling must exist");
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(
            format!(
                "{}\n",
                json!({"base":format!("http://{address}"),"url":url,"resource":resource.id(),"measured":measured.id()})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut created = native_resource("private_created_after_build");
    let lock = rusqlite::Connection::open(f.root.path().join("state/domain.sqlite3")).unwrap();
    let mut steps = Vec::new();
    tokio::time::timeout(Duration::from_secs(180), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let answer = match line.as_str() {
                "CREATE_RESOURCE" => {
                    f.domain.put_resource(created.clone()).await.unwrap();
                    json!({"resource":created.id()})
                }
                "EDIT_RESOURCE" => {
                    created.ceiling.as_mut().unwrap().tokens = Some(6000.try_into().unwrap());
                    f.domain.edit_resource(created.clone(), None).await.unwrap();
                    json!({"ok":true})
                }
                "HOLD_STORE" => {
                    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
                    json!({"ok":true})
                }
                "RELEASE_STORE" => {
                    lock.execute_batch("COMMIT").unwrap();
                    json!({"ok":true})
                }
                _ => {
                    println!("{line}");
                    continue;
                }
            };
            steps.push(line);
            input
                .write_all(format!("{answer}\n").as_bytes())
                .await
                .unwrap();
        }
        assert!(
            child.wait().await.unwrap().success(),
            "real resource browser assertions failed"
        );
    })
    .await
    .expect("real resource browser deadline");
    assert_eq!(
        steps,
        [
            "CREATE_RESOURCE",
            "EDIT_RESOURCE",
            "HOLD_STORE",
            "RELEASE_STORE"
        ]
    );
    handle.stop_graceful(Some(Duration::from_secs(2)));
    serving.await.unwrap().unwrap();
    f.close().await;
}

#[tokio::test]
async fn native_console_resources_executable() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    let binary = env!("CARGO_BIN_EXE_hagency");
    let init = Command::new(binary)
        .args(["init", "--state-dir"])
        .arg(&state)
        .env_clear()
        .env("PATH", &empty_path)
        .output()
        .await
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let (mut db, _) = seed(&state);
    let resource = native_resource("private_executable_resource");
    db.put_resource(&resource).unwrap();
    drop(db);
    // Brief 21: the executable lane seeds the SAME measured pool the plain
    // lane does (seed() plants private_usage_pool with a bound usage source
    // and real observations), so both lanes prove the same headroom cells —
    // the orchestrator's failing run was the driver selecting an id this
    // config never carried. The id is the deterministic resource hash, so
    // constructing the resource computes the seeded row's id.
    let measured = native_resource("private_usage_pool");
    let address = address();
    let mut server = Command::new(binary)
        .args(["serve", "--state-dir"])
        .arg(&state)
        .args(["--listen", &address.to_string(), "--console-assets"])
        .arg(built())
        .env_clear()
        .env("PATH", &empty_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if tokio::net::TcpStream::connect(address).await.is_ok() {
                break;
            }
            assert!(
                server.try_wait().unwrap().is_none(),
                "{}",
                await_admission(&mut server, address).await
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native executable startup");
    let link = Command::new(binary)
        .args(["console-access", "--state-dir"])
        .arg(&state)
        .args(["--listen", &address.to_string()])
        .env_clear()
        .env("PATH", &empty_path)
        .output()
        .await
        .unwrap();
    assert!(
        link.status.success(),
        "native access command must work without Node on PATH"
    );
    let url = String::from_utf8(link.stdout).unwrap().trim().to_owned();
    assert!(!url.contains(TOKEN));
    let mut browser = Command::new(node())
        .arg(resource_script())
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    browser.stdin.take().unwrap().write_all(format!("{}\n", json!({"base":format!("http://{address}"),"url":url,"resource":resource.id(),"measured":measured.id(),"executable":true})).as_bytes()).await.unwrap();
    // Same 90s budget as the console walk above: under the shared host's
    // load the Chrome launch alone can eat a third of the old 45s.
    assert!(
        tokio::time::timeout(Duration::from_secs(90), browser.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    server.kill().await.unwrap();
    server.wait().await.unwrap();
}

fn configuration_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../mockup/scripts/native-console-resource-configuration-browser.mjs")
}
#[tokio::test]
async fn native_console_resource_configuration_browser() {
    let address = address();
    let f = Fixture::new(address, Some(&built()));
    let resource = configuration_resource();
    f.domain.put_resource(resource.clone()).await.unwrap();
    hagency_store::private::write_new(
        &f.root.path().join("state/operator.token"),
        TOKEN.as_bytes(),
    )
    .unwrap();
    let server = Server::new(TcpListener::new(address).try_bind().await.unwrap());
    let handle = server.handle();
    let serving = tokio::spawn(server.try_serve(f.app.clone().router()));
    let url = hagency::console::client::access(&f.root.path().join("state"), address)
        .await
        .unwrap();
    let mut child = Command::new(node())
        .arg(configuration_script())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("actual browser tooling must exist");
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(
            format!(
                "{}\n",
                json!({"base":format!("http://{address}"),"url":url,"resource":resource.id()})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let lock = rusqlite::Connection::open(f.root.path().join("state/domain.sqlite3")).unwrap();
    let mut created = None;
    let mut unknown = None;
    let mut steps = Vec::new();
    tokio::time::timeout(Duration::from_secs(90), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            if let Some(id) = line.strip_prefix("CREATED ") {
                let actual = f.domain.resource_configuration(id.into()).await.unwrap();
                assert_eq!(actual.seat_id, resource.seat_id);
                assert_ne!(actual.preset_id, resource.preset_id);
                assert!(actual.published);
                created = Some(id.to_owned());
                continue;
            }
            if let Some(id) = line.strip_prefix("EDITED ") {
                assert_eq!(created.as_deref(), Some(id));
                assert_eq!(
                    u64::from(
                        f.domain
                            .resource_budget(id.into())
                            .await
                            .unwrap()
                            .pool
                            .ceiling
                            .unwrap()
                    ),
                    22222
                );
                continue;
            }
            if let Some(id) = line.strip_prefix("UNKNOWN_CREATED ") {
                let actual = f.domain.resource_configuration(id.into()).await.unwrap();
                assert_eq!(actual.seat_id, resource.seat_id);
                unknown = Some(id.to_owned());
                continue;
            }
            let answer = if let Some(id) = line.strip_prefix("EDIT_RESOURCE ") {
                let mut actual = f.domain.resource_configuration(id.into()).await.unwrap();
                actual.ceiling.as_mut().unwrap().tokens = Some(6000.try_into().unwrap());
                f.domain.edit_resource(actual, None).await.unwrap();
                json!({"ok":true})
            } else {
                match line.as_str() {
                    "HOLD_STORE" => {
                        lock.execute_batch("BEGIN IMMEDIATE").unwrap();
                        json!({"ok":true})
                    }
                    "RELEASE_STORE" => {
                        lock.execute_batch("COMMIT").unwrap();
                        json!({"ok":true})
                    }
                    _ => {
                        println!("{line}");
                        continue;
                    }
                }
            };
            steps.push(line);
            input
                .write_all(format!("{answer}\n").as_bytes())
                .await
                .unwrap();
        }
        assert!(
            child.wait().await.unwrap().success(),
            "real configuration browser assertions failed"
        );
    })
    .await
    .expect("real configuration browser deadline");
    assert!(created.is_some());
    assert!(unknown.is_some());
    assert_eq!(steps.len(), 3);
    assert_eq!(
        f.domain
            .resource_configurations(String::new(), 100)
            .await
            .unwrap()
            .iter()
            .filter(|r| r.config.seat_id == resource.seat_id)
            .count(),
        3
    );
    handle.stop_graceful(Some(Duration::from_secs(2)));
    serving.await.unwrap().unwrap();
    f.close().await;
}

#[tokio::test]
async fn native_console_resource_configuration_executable() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    let binary = env!("CARGO_BIN_EXE_hagency");
    let init = Command::new(binary)
        .args(["init", "--state-dir"])
        .arg(&state)
        .env_clear()
        .env("PATH", &empty_path)
        .output()
        .await
        .unwrap();
    assert!(init.status.success());
    let (mut db, _) = seed(&state);
    let source = configuration_resource();
    db.put_resource(&source).unwrap();
    drop(db);
    let mut selected = source.id();
    for restart in [false, true] {
        let address = address();
        let mut server = Command::new(binary)
            .args(["serve", "--state-dir"])
            .arg(&state)
            .args(["--listen", &address.to_string(), "--console-assets"])
            .arg(built())
            .env_clear()
            .env("PATH", &empty_path)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if tokio::net::TcpStream::connect(address).await.is_ok() {
                    break;
                }
                assert!(
                    server.try_wait().unwrap().is_none(),
                    "{}",
                    await_admission(&mut server, address).await
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let link = Command::new(binary)
            .args(["console-access", "--state-dir"])
            .arg(&state)
            .args(["--listen", &address.to_string()])
            .env_clear()
            .env("PATH", &empty_path)
            .output()
            .await
            .unwrap();
        assert!(
            link.status.success(),
            "{}",
            String::from_utf8_lossy(&link.stderr)
        );
        let url = String::from_utf8(link.stdout).unwrap().trim().to_owned();
        assert!(!url.contains(TOKEN));
        let mut browser = Command::new(node())
            .arg(configuration_script())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        browser.stdin.take().unwrap().write_all(format!("{}\n",json!({"base":format!("http://{address}"),"url":url,"resource":selected,"executable":true,"verifyOnly":restart})).as_bytes()).await.unwrap();
        let mut lines = BufReader::new(browser.stdout.take().unwrap()).lines();
        let mut observed = false;
        // 180s, like the regression lane: parallel Chrome lanes on the
        // shared host starved the old 60s mid-walk.
        tokio::time::timeout(Duration::from_secs(180), async {
            while let Some(line) = lines.next_line().await.unwrap() {
                if let Some(id) = line.strip_prefix("CREATED ") {
                    assert!(!restart);
                    selected = id.into();
                    observed = true;
                }
                if let Some(id) = line.strip_prefix("VERIFIED ") {
                    assert!(restart);
                    assert_eq!(id, selected);
                    observed = true;
                }
                println!("{line}");
            }
            assert!(browser.wait().await.unwrap().success());
        })
        .await
        .expect("native configuration executable browser deadline");
        assert!(observed);
        server.kill().await.unwrap();
        server.wait().await.unwrap();
        let db = hagency_store::DomainRepository::open(&state).unwrap();
        let actual = db.resource_configuration(&selected).unwrap();
        assert_eq!(actual.seat_id, source.seat_id);
        assert_eq!(u64::from(actual.ceiling.unwrap().tokens.unwrap()), 22222);
        drop(db);
    }
}

fn accounts_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../mockup/scripts/native-console-accounts-browser.mjs")
}
#[tokio::test]
async fn native_console_accounts_browser() {
    let address = address();
    let f = Fixture::new(address, Some(&built()));
    hagency_store::private::write_new(
        &f.root.path().join("state/operator.token"),
        TOKEN.as_bytes(),
    )
    .unwrap();
    // Seed one enrolled account through the store's own wrappers, then read
    // its identity values straight from the row the page must not reveal.
    let reserved = f
        .domain
        .reserve_account(hagency_store::ACCOUNT_PROFILE.to_owned())
        .await
        .unwrap();
    let choice = f
        .domain
        .materialize_account(reserved.id.clone())
        .await
        .unwrap();
    let managed = f.domain.managed_account(choice.id.clone()).await.unwrap();
    let access = hagency_store::AccountEnrollmentAccess::new(
        std::time::Instant::now() + std::time::Duration::from_secs(30),
        Default::default(),
    );
    let command = access
        .prepare(
            &managed,
            choice.revision.clone(),
            "gpt-5.6-sol".into(),
            Some("medium".into()),
            None,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
    f.domain.enroll_account_resource(command).await.unwrap();
    let secrets = {
        let sql = rusqlite::Connection::open(f.root.path().join("state/domain.sqlite3")).unwrap();
        sql.query_row(
            "SELECT a.seat_id,a.identity_tuple,json_extract(a.namespace_identity,'$.volume'),r.preset_id FROM managed_accounts a JOIN resource_accounts r ON r.account_id=a.id LIMIT 1",
            [],
            |row| {
                Ok([
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ])
            },
        )
        .unwrap()
    };
    let server = Server::new(TcpListener::new(address).try_bind().await.unwrap());
    let handle = server.handle();
    let serving = tokio::spawn(server.try_serve(f.app.clone().router()));
    let url = hagency::console::client::access(&f.root.path().join("state"), address)
        .await
        .unwrap();
    assert!(
        url.contains("/console/usage/#access="),
        "one link opens the console: {url}"
    );
    let mut child = Command::new(node())
        .arg(accounts_script())
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
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
                json!({"base":format!("http://{address}"),"url":url,"account":choice.id,"secrets":secrets})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(180), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success(),
        "real accounts browser assertions failed"
    );
    handle.stop_graceful(Some(Duration::from_secs(2)));
    serving.await.unwrap().unwrap();
    f.close().await;
}

#[tokio::test]
async fn native_console_agent_roster_browser() {
    let address = address();
    let f = Fixture::new(address, Some(&built()));
    hagency_store::private::write_new(
        &f.root.path().join("state/operator.token"),
        TOKEN.as_bytes(),
    )
    .unwrap();
    let acceptor = TcpListener::new(address).try_bind().await.unwrap();
    let server = Server::new(acceptor);
    let handle = server.handle();
    let serving = tokio::spawn(server.try_serve(f.app.clone().router()));
    // The harness owns the ticket — the driver receives only a URL, and it
    // mints nothing itself; one outstanding ticket at a time.
    let url = hagency::console::client::access(&f.root.path().join("state"), address)
        .await
        .unwrap();
    let mut child = Command::new(node())
        .arg(script())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
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
                json!({"base":format!("http://{address}"),"url":url,"roster":true})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let output = child.wait_with_output().await.unwrap();
    assert!(
        output.status.success(),
        "real roster browser assertions failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("PASS native agent roster browser"),
        "the roster lane reported no pass marker"
    );
    handle.stop_graceful(Some(Duration::from_secs(2)));
    serving.await.unwrap().unwrap();
    f.close().await;
}

#[tokio::test]
async fn native_console_project_side_browser() {
    let address = address();
    let f = Fixture::new(address, Some(&built()));
    hagency_store::private::write_new(
        &f.root.path().join("state/operator.token"),
        TOKEN.as_bytes(),
    )
    .unwrap();
    let acceptor = TcpListener::new(address).try_bind().await.unwrap();
    let server = Server::new(acceptor);
    let handle = server.handle();
    let serving = tokio::spawn(server.try_serve(f.app.clone().router()));
    // The harness owns the ticket — the driver receives only a URL and
    // mints nothing itself; one outstanding ticket at a time.
    let url = hagency::console::client::access(&f.root.path().join("state"), address)
        .await
        .unwrap();
    let mut child = Command::new(node())
        .arg(script())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
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
                json!({"base":format!("http://{address}"),"url":url,"sides":true})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let output = child.wait_with_output().await.unwrap();
    assert!(
        output.status.success(),
        "real project-sides browser assertions failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("PASS native project-sides browser"),
        "the project-sides lane reported no pass marker"
    );
    handle.stop_graceful(Some(Duration::from_secs(2)));
    serving.await.unwrap().unwrap();
    f.close().await;
}

/// CL-S2 (ADR-130) browser scenario: the roster renders NO enabled
/// lifecycle control under a read-only ticket, then the SAME walk under a
/// fresh agent-lifecycle ticket (the harness owns both tickets; the browser
/// mints nothing) renders stop and the implemented private outcome workflow.
/// No external request leaves the page in either phase.
#[tokio::test]
async fn native_console_agent_lifecycle_browser() {
    let address = address();
    let f = Fixture::new(address, Some(&built()));
    super::agents::seed_inspected_failure(&f).await;
    hagency_store::private::write_new(
        &f.root.path().join("state/operator.token"),
        TOKEN.as_bytes(),
    )
    .unwrap();
    let acceptor = TcpListener::new(address).try_bind().await.unwrap();
    let server = Server::new(acceptor);
    let handle = server.handle();
    let serving = tokio::spawn(server.try_serve(f.app.clone().router()));
    let url = hagency::console::client::access(&f.root.path().join("state"), address)
        .await
        .unwrap();
    let mut child = Command::new(node())
        .arg(script())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("actual browser tooling must exist");
    let mut input = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    input
        .write_all(
            format!(
                "{}\n",
                json!({"base":format!("http://{address}"),"url":url,"lifecycle":true,"engagement":f.engagement})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut pass = false;
    tokio::time::timeout(Duration::from_secs(90), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            match line.as_str() {
                "LIFECYCLE_TICKET" => {
                    let lifecycle_url =
                        hagency::console::client::access(&f.root.path().join("state"), address)
                            .await
                            .unwrap();
                    input
                        .write_all(format!("{}\n", json!({"url":lifecycle_url})).as_bytes())
                        .await
                        .unwrap();
                }
                other => {
                    if other.contains("PASS native agent lifecycle browser") {
                        pass = true;
                    }
                }
            }
        }
    })
    .await
    .expect("real lifecycle browser deadline");
    assert!(pass, "the lifecycle lane reported no pass marker");
    assert!(
        child.wait().await.unwrap().success(),
        "lifecycle browser failed"
    );
    let sql = rusqlite::Connection::open(f.root.path().join("state/domain.sqlite3")).unwrap();
    assert_eq!(sql.query_row("SELECT json_extract(config,'$.status') FROM canonical_tasks WHERE id='resolution_task'",[],|r|r.get::<_,String>(0)).unwrap(),"blocked");
    assert_eq!(
        sql.query_row("SELECT COUNT(*) FROM outcome_resolutions", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        sql.query_row(
            "SELECT state FROM runner_dispatches WHERE id='resolution_dispatch'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "outcome_unknown"
    );
    drop(sql);
    handle.stop_graceful(Some(Duration::from_secs(2)));
    serving.await.unwrap().unwrap();
    f.close().await;
}

/// Board #107: the tasks page's WRITE journey over the SERVED binary — create a
/// task, comment on it and see the comment come back, take the move the server
/// offers, then delete it, every step through the page's own controls. The
/// retired regression threw INSIDE the click handler before any request left the
/// browser: the comment control was enabled, the id was valid, and the click
/// sent nothing — so no toast and no render changed. A render-only assertion
/// cannot see that; this walk waits for the SERVED reply, against the shipped
/// bundle the operator's deployment serves.
#[tokio::test]
async fn native_console_tasks_browser() {
    let address = address();
    let f = Fixture::new(address, Some(&built()));
    hagency_store::private::write_new(
        &f.root.path().join("state/operator.token"),
        TOKEN.as_bytes(),
    )
    .unwrap();
    let acceptor = TcpListener::new(address).try_bind().await.unwrap();
    let server = Server::new(acceptor);
    let handle = server.handle();
    let serving = tokio::spawn(server.try_serve(f.app.clone().router()));
    let url = hagency::console::client::access(&f.root.path().join("state"), address)
        .await
        .unwrap();
    let mut child = Command::new(node())
        .arg(script())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("actual browser tooling must exist");
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(
            format!(
                "{}\n",
                json!({"base":format!("http://{address}"),"url":url,"tasks":true})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut pass = false;
    tokio::time::timeout(Duration::from_secs(90), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            if line.contains("PASS native tasks browser") {
                pass = true;
            }
            println!("{line}");
        }
    })
    .await
    .expect("real tasks browser deadline");
    assert!(pass, "the tasks lane reported no pass marker");
    assert!(
        child.wait().await.unwrap().success(),
        "tasks browser failed"
    );
    handle.stop_graceful(Some(Duration::from_secs(2)));
    serving.await.unwrap().unwrap();
    f.close().await;
}

fn regression_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../mockup/scripts/native-console-regression-browser.mjs")
}

/// The #56 regression lane: ONE login walks every rail entry, then the key
/// operator actions end to end on fixture data — approve and refuse a
/// pending engagement, stop an agent (start asserted as the fail-closed
/// absence it is), generate a side registration file, clear-dirty the
/// configuration wizard, resolve the seeded alert — each asserting its
/// visible result text. The store outcomes are asserted here against the
/// database, derived from the same code paths the verdict and lifecycle
/// lanes pin (approve → reserved + provision effect; refuse → rejected;
/// a started dispatch fences to outcome_unknown with its session
/// quarantined; the registration YAML lands on disk).
#[tokio::test]
async fn native_console_regression_browser() {
    let address = address();
    let f = Fixture::new(address, Some(&built()));
    // The approve target: a pending engagement with stored admission
    // evidence, seeded before the server starts (the same shape
    // new_engagement mints for the verdict lane).
    let pool = common::resource("private_usage_pool", "private_usage_seat", 1000);
    let approve_proof = common::proof(&common::request(
        "regression_approve",
        "ApproveWorker",
        &pool,
        100,
    ));
    let approve_id = f.domain.admit(approve_proof, 1000).await.unwrap().id;
    // The clear-dirty target: a published configuration with a 5000-token
    // monthly ceiling the wizard reload restores.
    let resource = configuration_resource();
    f.domain.put_resource(resource.clone()).await.unwrap();
    hagency_store::private::write_new(
        &f.root.path().join("state/operator.token"),
        TOKEN.as_bytes(),
    )
    .unwrap();
    let acceptor = TcpListener::new(address).try_bind().await.unwrap();
    let server = Server::new(acceptor);
    let handle = server.handle();
    let serving = tokio::spawn(server.try_serve(f.app.clone().router()));
    let url = hagency::console::client::access(&f.root.path().join("state"), address)
        .await
        .unwrap();
    let mut child = Command::new(node())
        .arg(regression_script())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("actual browser tooling must exist");
    let mut input = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    input
        .write_all(
            format!(
                "{}\n",
                json!({
                    "base": format!("http://{address}"),
                    "url": url,
                    "engagement": f.engagement,
                    "pendingAgent": "ApproveWorker",
                    "registrationUrl": format!("http://{address}/"),
                    "resource": resource.id(),
                    "agent": "UsageWorker",
                })
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut refuse_id = None;
    let mut tickets = Vec::new();
    let mut pass = false;
    tokio::time::timeout(Duration::from_secs(180), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let answer = match line.as_str() {
                "LIFECYCLE_TICKET" | "CONFIGURATION_TICKET" => {
                    // Ticket issuance is limited to one per second; the
                    // browser walk before each request can outpace it.
                    tokio::time::sleep(Duration::from_millis(1010)).await;
                    // One console link covers every scope, so both tickets are the same call.
                    let scoped =
                        hagency::console::client::access(&f.root.path().join("state"), address)
                            .await
                            .unwrap();
                    tickets.push(line.clone());
                    json!({"url": scoped})
                }
                "REFUSE_ENGAGEMENT" => {
                    let refuse_proof = common::proof(&common::request(
                        "regression_refuse",
                        "RefuseWorker",
                        &pool,
                        100,
                    ));
                    let id = f.domain.admit(refuse_proof, 1000).await.unwrap().id;
                    refuse_id = Some(id);
                    json!({"agent": "RefuseWorker"})
                }
                other => {
                    if other.contains("PASS native console regression browser") {
                        pass = true;
                    }
                    println!("{other}");
                    continue;
                }
            };
            input
                .write_all(format!("{answer}\n").as_bytes())
                .await
                .unwrap();
        }
    })
    .await
    .expect("real regression browser deadline");
    assert!(pass, "the regression lane reported no pass marker");
    assert!(
        child.wait().await.unwrap().success(),
        "regression browser failed"
    );
    assert_eq!(tickets, ["LIFECYCLE_TICKET", "CONFIGURATION_TICKET"]);
    let sql = rusqlite::Connection::open(f.root.path().join("state/domain.sqlite3")).unwrap();
    let approve_state: String = sql
        .query_row(
            "SELECT state FROM engagements WHERE id=?1",
            [&approve_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(approve_state, "reserved");
    // ADR-186 §A: the walk approved with an amount of 80, not the request.
    let approve_allocated: Option<u64> = sql
        .query_row(
            "SELECT allocated_tokens FROM engagements WHERE id=?1",
            [&approve_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(approve_allocated, Some(80));
    // ADR-186 §C: the walk added 25 tokens to the running engagement.
    let topped_up: Option<u64> = sql
        .query_row(
            "SELECT allocated_tokens FROM engagements WHERE id=?1",
            [&f.engagement],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(topped_up, Some(125));
    let provision: String = sql
        .query_row(
            "SELECT state FROM effects WHERE engagement_id=?1 AND kind='provision'",
            [&approve_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(provision, "pending");
    let refuse_state: String = sql
        .query_row(
            "SELECT state FROM engagements WHERE id=?1",
            [refuse_id.expect("the refuse marker ran")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(refuse_state, "rejected");
    let dispatch_state: String = sql
        .query_row(
            "SELECT state FROM runner_dispatches WHERE id='private_dispatch'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dispatch_state, "outcome_unknown");
    let quarantined: u64 = sql
        .query_row(
            "SELECT quarantined FROM runner_sessions WHERE id='private_session'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // The operator stop is a CONFIRMED cleanup (TS parity: the stop route
    // settles only unconfirmed runner terminations behind
    // `stopUnconfirmedDispatches`, backend-v2.js:12636-12648 — the confirmed
    // path never retains the quarantine). The fixture has no runner process,
    // so the host's stop IS the confirmation: the fence ran (the dispatch
    // above ended outcome_unknown), the stop row settles, and the session
    // quarantine clears — the same words the store-level tests use
    // ("the operator stop settles the stop row", console/agents.rs).
    let settled: Option<u64> = sql
        .query_row(
            "SELECT settled_at FROM dispatch_stops WHERE dispatch_id='private_dispatch'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(settled.is_some(), "the operator stop settles the fence row");
    assert_eq!(
        quarantined, 0,
        "the confirmed stop clears the session quarantine"
    );
    let alert_status: String = sql
        .query_row("SELECT status FROM ceiling_alerts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(alert_status, "resolved");
    drop(sql);
    assert!(
        f.root
            .path()
            .join("state/registrations/example.test.yaml")
            .exists(),
        "the registration file landed on disk"
    );
    handle.stop_graceful(Some(Duration::from_secs(2)));
    serving.await.unwrap().unwrap();
    f.close().await;
}
