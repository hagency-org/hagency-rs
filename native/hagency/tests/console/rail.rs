//! Board #88: every page the rail links must be SHIPPED and SERVED.
//!
//! The rail is the only navigable surface an operator has. A row that renders
//! an `<a href>` is a page they can click, so if the build script does not
//! emit it — or `assets.rs` does not admit it — the click 404s, while every
//! unit test of the page itself still passes. That is exactly the live defect
//! #88 records: pages sat in the Next app, were never packaged, and the front
//! door did not exist at all.
//!
//! This test closes both halves against real artifacts, never a hand list:
//!   1. the hrefs are read OUT OF `Rail.jsx`, so a new rail row is covered
//!      the day it is added and cannot be forgotten here;
//!   2. each page is asserted PRESENT IN THE BUILT BUNDLE's manifest (the
//!      build script's half), and
//!   3. the REAL binary is spawned on that bundle and each page plus the
//!      front door is GET, expecting 200 (the service's half).
//!
//! Feature-gated with the browser lane: the bundle is a build artifact named
//! by `HAGENCY_NATIVE_CONSOLE_ASSETS`, which only `build:native` produces.

use serde_json::Value;
use std::{
    net::{SocketAddr, TcpListener as StdListener},
    path::PathBuf,
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

/// The built bundle, resolved to its actual host path. The bundle may be
/// spelled through a symlinked ancestor (this host's `.../home/hl` ->
/// `hl.noindex`); the in-process fixtures canonicalize for the same reason.
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

/// The native rail's page paths, DERIVED from `Rail.jsx`.
///
/// `NativeRail` renders an `<a>` for the keys in its inline array and a
/// disabled `<span aria-disabled>` for every other row, so that array IS the
/// set of pages the rail can navigate to. Reading it here is what makes the
/// test fail the day a page joins the rail without being shipped.
fn rail_pages() -> Vec<String> {
    let source = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../mockup/components/Rail.jsx"),
    )
    .expect("mockup/components/Rail.jsx must exist to derive the rail's pages");
    let line = source
        .lines()
        .find(|line| line.contains("].includes(row.key)"))
        .expect("Rail.jsx must keep the native `[...].includes(row.key)` link guard");
    let marker = "].includes(row.key)";
    let end = line.find(marker).expect("link guard marker");
    let start = line[..end].rfind('[').expect("link guard array start");
    let keys: Vec<String> = line[start + 1..end]
        .split(',')
        .map(|token| token.trim().trim_matches(['\'', '"']).to_owned())
        .filter(|key| !key.is_empty())
        .collect();
    assert_eq!(
        keys.len(),
        9,
        "the native rail's link set changed size; re-derive and confirm the pages ship: {keys:?}"
    );
    keys.into_iter()
        .map(|key| match key.as_str() {
            // Mirrors NATIVE_PATHS (Rail.jsx): the workforce row is served by
            // the agents roster, the camel-cased keys by their hyphenated
            // pages, every other row by its own path.
            "workforce" => "/console/agents/".to_owned(),
            "serverEngagements" => "/console/server-engagements/".to_owned(),
            other => format!("/console/{other}/"),
        })
        .collect()
}

/// The bundle path that backs a served page URL.
fn document(path: &str) -> String {
    if path == "/console/" {
        "index.html".to_owned()
    } else {
        format!("{}index.html", path.trim_start_matches("/console/"))
    }
}

/// One GET over a raw socket. The asset routes need no session (they serve
/// under the browser boundary, which checks the Host header and the same-origin
/// fetch site), so a plain request is what a browser document navigation sends.
async fn status(address: SocketAddr, path: &str) -> u16 {
    let mut socket = tokio::net::TcpStream::connect(address)
        .await
        .unwrap_or_else(|error| panic!("connect for {path}: {error}"));
    let request = format!(
        "GET {path} HTTP/1.1\r\nhost: {address}\r\nsec-fetch-site: same-origin\r\nconnection: close\r\n\r\n"
    );
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut wire = Vec::new();
    socket.read_to_end(&mut wire).await.unwrap();
    let head = String::from_utf8_lossy(&wire);
    let line = head.lines().next().unwrap_or_default();
    line.split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("no status line for {path}: {line}"))
        .parse()
        .unwrap_or_else(|_| panic!("unparseable status for {path}: {line}"))
}

/// The reason a spawned executable refused to start — its exit status and its
/// stderr. A generic "process exited" line is the failure mode #83 needed a
/// re-run for, so the refusal names itself.
async fn refusal(server: &mut tokio::process::Child) -> String {
    let mut stderr = String::new();
    if let Some(mut pipe) = server.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr).await;
    }
    format!(
        "exited before admission: {:?}; stderr:\n{stderr}",
        server.try_wait()
    )
}

#[tokio::test]
async fn native_console_rail_pages_are_shipped_and_served() {
    let bundle = built();

    // --- the build script's half: every rail page is IN the bundle ---
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(bundle.join("manifest.json")).expect("the bundle must carry manifest.json"),
    )
    .expect("manifest.json must be JSON");
    let packaged: Vec<&str> = manifest["assets"]
        .as_array()
        .expect("manifest assets")
        .iter()
        .filter_map(|asset| asset["path"].as_str())
        .collect();
    assert!(
        packaged.contains(&"index.html"),
        "the front door is not packaged; packed documents: {packaged:?}"
    );
    for page in rail_pages() {
        let document = document(&page);
        assert!(
            packaged.contains(&document.as_str()),
            "the rail links {page} but the build did not package {document}; packed documents: {packaged:?}"
        );
    }

    // --- the service's half: the real binary serves each of them ---
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
    let address = address();
    let mut server = Command::new(binary)
        .args(["serve", "--state-dir"])
        .arg(&state)
        .args(["--listen", &address.to_string(), "--console-assets"])
        .arg(&bundle)
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
                refusal(&mut server).await
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native executable startup");

    let mut failures = Vec::new();
    for path in std::iter::once("/console/".to_owned()).chain(rail_pages()) {
        let code = status(address, &path).await;
        if code != 200 {
            failures.push(format!("{path} -> {code}"));
        }
    }
    server.kill().await.ok();
    server.wait().await.ok();

    assert!(
        failures.is_empty(),
        "every rail page and the front door must serve: {failures:?}"
    );
}
