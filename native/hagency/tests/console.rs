// The browser and live-action harnesses each mount the shared Matrix fixture.
#![allow(clippy::duplicate_mod)]
#[path = "console/accounts.rs"]
mod accounts;
#[path = "console/agents.rs"]
mod agents;
#[path = "console/alerts.rs"]
mod alerts;
#[path = "console/approval_bindings.rs"]
mod approval_bindings;
#[path = "console/approvals.rs"]
mod approvals;
#[path = "console/browser.rs"]
#[cfg(feature = "native-console-browser")]
mod browser;
#[path = "console/configuration.rs"]
mod configuration;
#[path = "console/engagements.rs"]
mod engagements;
#[path = "console/engagements_allocation.rs"]
mod engagements_allocation;
#[path = "console/engagements_retire.rs"]
mod engagements_retire;
#[path = "console/engagements_verdict.rs"]
mod engagements_verdict;
#[path = "console/exec_policy.rs"]
mod exec_policy;
#[path = "console/fixture.rs"]
mod fixture;
#[path = "console/fleet_views.rs"]
mod fleet_views;
#[path = "console/graphs.rs"]
mod graphs;
#[path = "console/invites.rs"]
mod invites;
#[path = "console/live_actions.rs"]
#[cfg(feature = "native-console-browser")]
mod live_actions;
#[path = "console/matrix_diag.rs"]
mod matrix_diag;
#[path = "console/offer_book.rs"]
mod offer_book;
#[path = "console/origin.rs"]
mod origin;
#[path = "console/palpo_import.rs"]
mod palpo_import;
#[path = "console/project_sides.rs"]
mod project_sides;
#[path = "console/rail.rs"]
#[cfg(feature = "native-console-browser")]
mod rail;
#[path = "console/real_agent.rs"]
mod real_agent;
#[path = "console/registration.rs"]
mod registration;
#[path = "console/resources.rs"]
mod resources;
#[path = "console/server_engagements.rs"]
mod server_engagements;
#[path = "console/setup_page.rs"]
mod setup_page;
#[path = "console/side_budget.rs"]
mod side_budget;
#[path = "console/side_lifecycle.rs"]
mod side_lifecycle;
#[path = "console/side_registration.rs"]
mod side_registration;
#[path = "console/status_strip.rs"]
#[cfg(feature = "native-console-browser")]
mod status_strip;
#[path = "console/stream.rs"]
mod stream;
#[path = "console/tasks.rs"]
mod tasks;
#[path = "console/ts_oracle_approvals.rs"]
mod ts_oracle_approvals;
use fixture::*;
use hagency_core::tasks::{DispatchInput, ResourceLease, SessionBinding};
use salvo::{
    prelude::*,
    test::{ResponseExt, TestClient},
};
use serde_json::{Value, json};

async fn issue(service: &Service) -> String {
    let mut response = TestClient::post(format!("{BASE}/api/native/v1/console/access"))
        .add_header("host", "127.0.0.1:13300", true)
        .bearer_auth(TOKEN)
        .send(service)
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    response.take_json::<Value>().await.unwrap()["ticket"]
        .as_str()
        .unwrap()
        .to_owned()
}
async fn exchange(service: &Service, ticket: &str) -> Response {
    TestClient::post(format!("{BASE}/console/session"))
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("origin", BASE, true)
        .add_header("sec-fetch-site", "same-origin", true)
        .json(&json!({"ticket":ticket}))
        .send(service)
        .await
}
async fn session(service: &Service) -> String {
    let ticket = issue(service).await;
    let response = exchange(service, &ticket).await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap();
    for flag in ["HttpOnly", "SameSite=Strict", "Path=/console"] {
        assert!(cookie.contains(flag));
    }
    // TS parity: no Max-Age — the login cookie survives reloads; only
    // logout (or process end) ends it.
    assert!(!cookie.contains("Max-Age"));
    cookie.split(';').next().unwrap().to_owned()
}
fn get(path: &str, cookie: &str) -> salvo::test::RequestBuilder {
    TestClient::get(format!("{BASE}{path}"))
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("sec-fetch-site", "same-origin", true)
        .add_header("cookie", cookie, true)
}
/// Board #92: a HEAD exactly as a browser issues it — the same authority
/// headers a GET carries. Next's `<Link>` prefetch uses HEAD on the documents
/// the console links to, so this is a real client, not a hypothetical.
fn head(path: &str, cookie: &str) -> salvo::test::RequestBuilder {
    TestClient::head(format!("{BASE}{path}"))
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("sec-fetch-site", "same-origin", true)
        .add_header("cookie", cookie, true)
}
fn post(path: &str, cookie: &str) -> salvo::test::RequestBuilder {
    TestClient::post(format!("{BASE}{path}"))
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("origin", BASE, true)
        .add_header("sec-fetch-site", "same-origin", true)
        .add_header("cookie", cookie, true)
}
fn delete(path: &str, cookie: &str) -> salvo::test::RequestBuilder {
    TestClient::delete(format!("{BASE}{path}"))
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("origin", BASE, true)
        .add_header("sec-fetch-site", "same-origin", true)
        .add_header("cookie", cookie, true)
}
fn patch(path: &str, cookie: &str) -> salvo::test::RequestBuilder {
    TestClient::patch(format!("{BASE}{path}"))
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("origin", BASE, true)
        .add_header("sec-fetch-site", "same-origin", true)
        .add_header("cookie", cookie, true)
}
fn put(path: &str, cookie: &str) -> salvo::test::RequestBuilder {
    TestClient::put(format!("{BASE}{path}"))
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("origin", BASE, true)
        .add_header("sec-fetch-site", "same-origin", true)
        .add_header("cookie", cookie, true)
}
/// TS parity: one login is the whole console — the scoped issue routes are
/// gone, so every former "scoped session" is the same `session()`.
async fn lifecycle_session(service: &Service) -> String {
    session(service).await
}

#[tokio::test]
async fn native_console_authority() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let service = f.service();
    let anonymous = TestClient::get(format!("{BASE}/console/api/engagements"))
        .add_header("host", "127.0.0.1:13300", true)
        .send(&service)
        .await;
    assert_eq!(anonymous.status_code, Some(StatusCode::UNAUTHORIZED));
    let denied = TestClient::post(format!("{BASE}/api/native/v1/console/access"))
        .add_header("host", "127.0.0.1:13300", true)
        .bearer_auth(TOKEN)
        .add_header("origin", BASE, true)
        .send(&service)
        .await;
    assert_eq!(denied.status_code, Some(StatusCode::FORBIDDEN));
    let ticket = issue(&service).await;
    // TS parity: issuing access is never rate-limited (the retained
    // middleware never throttled re-authentication), and re-issuing simply
    // replaces the unexchanged link — no console_busy 429.
    let again = issue(&service).await;
    assert_ne!(again, ticket, "a fresh link replaces the old one");
    assert_eq!(
        exchange(&service, &ticket).await.status_code,
        Some(StatusCode::UNAUTHORIZED),
        "the replaced link is retired"
    );
    let response = exchange(&service, &again).await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    // The ticket is a reusable credential, not one-time: exchanging it
    // again yields another working session (a reload of the access URL
    // never meets a burn).
    assert_eq!(
        exchange(&service, &again).await.status_code,
        Some(StatusCode::OK)
    );
    // The scoped issue routes are gone — one login is the whole console.
    for path in [
        "resource-publication-access",
        "resource-configuration-access",
        "account-access",
        "agent-lifecycle-access",
    ] {
        let response = TestClient::post(format!("{BASE}/api/native/v1/console/{path}"))
            .add_header("host", "127.0.0.1:13300", true)
            .bearer_auth(TOKEN)
            .send(&service)
            .await;
        assert_eq!(
            response.status_code,
            Some(StatusCode::NOT_FOUND),
            "scoped route {path} is gone"
        );
    }
    for (name, value) in [
        ("host", "evil.test"),
        ("origin", "https://evil.test"),
        ("sec-fetch-site", "cross-site"),
        ("x-forwarded-for", "127.0.0.1"),
        ("cookie", "hagency_console=bad"),
        ("cookie", &format!("{cookie}; {cookie}")),
    ] {
        let response = get("/console/api/engagements", &cookie)
            .add_header(name, value, true)
            .send(&service)
            .await;
        assert!(matches!(
            response.status_code,
            Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
        ));
    }
    let raw = get("/api/native/v1/engagements", &cookie)
        .bearer_auth(TOKEN)
        .send(&service)
        .await;
    assert_eq!(raw.status_code, Some(StatusCode::FORBIDDEN));
    for fetch in ["none", "cross-site"] {
        let response = TestClient::get(format!("{BASE}/console/usage/#access=unused"))
            .add_header("host", "127.0.0.1:13300", true)
            .add_header("sec-fetch-site", fetch, true)
            .send(&service)
            .await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
    }
    let logout = TestClient::delete(format!("{BASE}/console/session"))
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("origin", BASE, true)
        .add_header("sec-fetch-site", "same-origin", true)
        .add_header("cookie", &cookie, true)
        .send(&service)
        .await;
    assert_eq!(logout.status_code, Some(StatusCode::OK));
    assert_eq!(
        get("/console/api/engagements", &cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    f.close().await;
}

#[tokio::test]
async fn native_console_assets() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let path = f.root.path().join("assets").canonicalize().unwrap();
    std::fs::write(path.join("usage/index.html"), b"changed after admission").unwrap();
    assert!(hagency::console::Console::load(&path).is_err());
    let mut response = TestClient::get(format!("{BASE}/console/usage/"))
        .add_header("host", "127.0.0.1:13300", true)
        .send(&f.service())
        .await;
    assert!(
        response
            .take_string()
            .await
            .unwrap()
            .contains("retained asset fixture")
    );
    // Board #47: the task-graphs document must be SERVED, which needs the page
    // admitted by `assets.rs` (an unlisted `task-graphs/index.html` makes
    // `Console::load` fail outright, so a bundle carrying the page would be
    // refused) at the URL the rail links to.
    let mut graphs = TestClient::get(format!("{BASE}/console/task-graphs/"))
        .add_header("host", "127.0.0.1:13300", true)
        .send(&f.service())
        .await;
    assert_eq!(graphs.status_code, Some(StatusCode::OK));
    assert!(
        graphs
            .take_string()
            .await
            .unwrap()
            .contains("task-graphs document fixture")
    );
    for path in [
        "/console/operator.token",
        "/console/agents/FixtureName",
        "/console/api/hagency/agents",
        "/console/_next/static/unknown.js",
        "/console/usage/?unknown=1",
    ] {
        let response = TestClient::get(format!("{BASE}{path}"))
            .add_header("host", "127.0.0.1:13300", true)
            .send(&f.service())
            .await;
        assert!(response.status_code.unwrap().is_client_error());
    }
    #[cfg(unix)]
    {
        let alias = f.root.path().canonicalize().unwrap().join("alias");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert!(hagency::console::Console::load(&alias).is_err());
        // Board #84: an ANCESTOR spelled through a relative symlink is a
        // legitimate path, not a bait — this host's own worktree alias is
        // `hl -> hl.noindex`. The old component-by-component nofollow walk
        // refused it and killed the executable suite (`Error: Assets`).
        // Only the asset directory ITSELF must be a real directory.
        use std::os::unix::fs::PermissionsExt;
        let anchor = path.parent().unwrap();
        let real = anchor.join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        assets(&real.join("bundle"));
        std::os::unix::fs::symlink("real", anchor.join("aliasdir")).unwrap();
        let through_alias = anchor.join("aliasdir").join("bundle");
        assert!(
            hagency::console::Console::load(&through_alias).is_ok(),
            "a bundle behind an ancestor symlink must load"
        );
        std::fs::remove_file(path.join("usage/index.html")).unwrap();
        std::os::unix::fs::symlink(path.join("manifest.json"), path.join("usage/index.html"))
            .unwrap();
        assert!(hagency::console::Console::load(&path).is_err());
    }
    for change in [
        "size",
        "hash",
        "count",
        "duplicate",
        "unknown",
        "manifest_bytes",
    ] {
        let dir = f.root.path().join(format!("assets_{change}"));
        assets(&dir);
        let dir = dir.canonicalize().unwrap();
        let manifest = dir.join("manifest.json");
        let mut value: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        match change {
            "size" => value["assets"][0]["size"] = json!(4 * 1024 * 1024 + 1),
            "hash" => value["assets"][0]["sha256"] = json!("0".repeat(64)),
            "count" => value["assets"] = json!(vec![value["assets"][0].clone(); 513]),
            "duplicate" => value["assets"] = json!(vec![value["assets"][0].clone(); 2]),
            "unknown" => value["secret"] = json!("not_an_asset_field"),
            "manifest_bytes" => {}
            _ => unreachable!(),
        }
        let bytes = if change == "manifest_bytes" {
            vec![b' '; 128 * 1024 + 1]
        } else {
            serde_json::to_vec(&value).unwrap()
        };
        std::fs::write(manifest, bytes).unwrap();
        assert!(
            hagency::console::Console::load(&dir).is_err(),
            "invalid {change} set was admitted"
        );
    }
    f.close().await;
}

#[tokio::test]
async fn native_console_usage() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let service = f.service();
    let cookie = session(&service).await;
    let mut listing = get("/console/api/engagements?limit=1", &cookie)
        .send(&service)
        .await;
    let value = listing.take_json::<Value>().await.unwrap();
    assert_eq!(value["engagements"][0]["id"], f.engagement);
    assert_private(&value);
    let path = format!("/console/api/engagements/{}/usage", f.engagement);
    let mut response = get(&path, &cookie).send(&service).await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let value = response.take_json::<Value>().await.unwrap();
    assert_private(&value);
    assert_eq!(value["summary"]["latest_counts"]["input"], 4);
    assert_eq!(value["summary"]["known_high_water_lower_bound"]["input"], 7);
    assert_eq!(value["summary"]["regression_observations"], 1);
    assert_eq!(value["daily"]["incomplete"], true);
    let new_id = f.new_engagement().await;
    let mut response = get(&format!("/console/api/engagements/{new_id}/usage"), &cookie)
        .send(&service)
        .await;
    let value = response.take_json::<Value>().await.unwrap();
    assert_eq!(value["summary"]["sources"], 0);
    assert!(value["summary"]["latest_counts"].is_null() && value["daily"].is_null());
    for suffix in [
        "?at_ms=1&at_ms=2",
        "?at_ms=bad",
        "?x=1",
        "?at_ms=18446744073709551616",
    ] {
        assert_eq!(
            get(&format!("{path}{suffix}"), &cookie)
                .send(&service)
                .await
                .status_code,
            Some(StatusCode::BAD_REQUEST)
        );
    }
    for query in ["limit=17", "limit=0", "limit=1&limit=2", "unknown=1"] {
        assert_eq!(
            get(&format!("/console/api/engagements?{query}"), &cookie)
                .send(&service)
                .await
                .status_code,
            Some(StatusCode::BAD_REQUEST)
        );
    }
    // Hold the actual SQLite writer, then poll the authenticated HTTP read into
    // its queue wait. Retirement occurs after admission and before its response.
    let lock = rusqlite::Connection::open(f.root.path().join("state/domain.sqlite3")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut write = Box::pin(f.domain.register(common::registration()));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(write.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    let mut waiting = Box::pin(get(&path, &cookie).send(&service));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    f.console.retire();
    lock.execute_batch("COMMIT").unwrap();
    write.await.unwrap();
    assert_eq!(
        waiting.await.status_code,
        Some(StatusCode::SERVICE_UNAVAILABLE)
    );
    assert_eq!(
        get(&path, &cookie).send(&service).await.status_code,
        Some(StatusCode::SERVICE_UNAVAILABLE)
    );
    f.close().await;
}

/// Board #108: the served page must show REAL counts when a Codex usage event
/// was recorded, even when the same engagement also carries a source that was
/// bound (its dispatch started) but never observed. The live fleet has exactly
/// that shape — several dispatches per engagement, one of them without a usage
/// notification — and the page rendered "Latest observed counts: Unknown" for
/// every kind beside real ceiling figures, because `usage_summary` folded the
/// all-unknown sibling into the sum. This walks the console mount, the path a
/// live console actually uses.
#[tokio::test]
async fn native_console_usage_counts_survive_an_unobserved_sibling_source() {
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap(), None);
    let service = f.service();
    let cookie = session(&service).await;
    let path = format!("/console/api/engagements/{}/usage", f.engagement);
    // The seed's single source is fully observed.
    let mut response = get(&path, &cookie).send(&service).await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let before: Value = response.take_json().await.unwrap();
    assert_eq!(before["summary"]["sources"], 1);
    let measured = before["summary"]["latest_counts"]["input"].clone();
    assert!(
        measured.is_u64(),
        "the seeded Codex usage event rendered a real count"
    );
    // Bind a SECOND source on the same engagement and never observe it: its
    // own session, task, workspace and dispatch, claimed and started, then
    // abandoned without a usage notification.
    f.domain
        .register_session(SessionBinding {
            id: "unobserved_session".into(),
            engagement_id: f.engagement.clone(),
            room_id: "!project:example.test".into(),
            thread_root: Some("$unobserved_thread".into()),
        })
        .await
        .unwrap();
    f.domain
        .create_canonical_task(
            "unobserved_task".into(),
            "unobserved_session".into(),
            "Unobserved turn".into(),
            2000,
        )
        .await
        .unwrap();
    f.domain
        .register_workspace("unobserved_workspace".into())
        .await
        .unwrap();
    f.domain
        .enqueue_dispatch(DispatchInput {
            id: "unobserved_dispatch".into(),
            session_id: "unobserved_session".into(),
            task_id: Some("unobserved_task".into()),
            resources: vec![ResourceLease {
                id: "unobserved_workspace".into(),
                exclusive: true,
            }],
            payload: json!({}),
        })
        .await
        .unwrap();
    // The ASYNC store's `owned_dispatch_scope`/`start_owned_dispatch` stamp the
    // REAL writer clock, so the claim must be made on that same clock or the
    // lease has already expired by the time the scope is taken.
    let cap = f
        .domain
        .claim_dispatch("unobserved_runner".into(), now(), 60_000, 120_000, 128)
        .await
        .unwrap()
        .unwrap();
    let scope = f.domain.owned_dispatch_scope(cap.clone()).await.unwrap();
    let started = f
        .domain
        .start_owned_dispatch(cap.clone(), scope.fingerprint().to_owned())
        .await
        .unwrap();
    let _unobserved = f.domain.bind_usage_source(cap, started).await.unwrap();
    // The unobserved sibling must not erase the recorded event's figures.
    let mut response = get(&path, &cookie).send(&service).await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let after: Value = response.take_json().await.unwrap();
    assert_eq!(
        after["summary"]["sources"], 2,
        "the bound-but-unobserved sibling is counted as a source"
    );
    assert_eq!(
        after["summary"]["latest_counts"]["input"], measured,
        "the recorded Codex event still shows its real count beside an unobserved source"
    );
    assert!(
        !after["summary"]["latest_counts"]["input"].is_null(),
        "never the Unknown word when usage was measured"
    );
    f.close().await;
}

/// Board #28: a refusal to start must SAY what is wrong and how to fix it —
/// a bare `Error: Assets` (exit 1, no field, no fix) is a bug on its own.
/// `Assets::load` deliberately walks every path component with
/// `open_dir_nofollow` (the console never resolves a filesystem path at
/// request time), so a bundle spelled through a symlinked ancestor — this
/// host's `.../home/hl` -> `hl.noindex`, macOS's `/var` -> `/private/var` —
/// is refused; the refusal must name `--console-assets` and the fix, and the
/// same directory reached by its actual host path must still admit.
#[tokio::test]
async fn native_console_assets_refusal_names_field_and_fix() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let binary = env!("CARGO_BIN_EXE_hagency");
    let init = std::process::Command::new(binary)
        .args(["init", "--state-dir"])
        .arg(&state)
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let bundle = root.path().join("assets");
    assets(&bundle);

    // (a) The refusal: an alias path exits non-zero and names field AND fix.
    // The child is polled to a deadline — `output()` would block forever if
    // the refusal ever regressed into an admission.
    #[cfg(unix)]
    {
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&bundle, &alias).unwrap();
        let mut child = std::process::Command::new(binary)
            .args(["serve", "--state-dir"])
            .arg(&state)
            .args(["--listen", "127.0.0.1:0", "--console-assets"])
            .arg(&alias)
            .env("PATH", "")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "an aliased bundle was admitted instead of refused"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert!(!status.success(), "an aliased bundle must not be admitted");
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            use std::io::Read;
            pipe.read_to_string(&mut stderr).unwrap();
        }
        assert!(
            stderr.contains("Error: Config"),
            "the assets refusal must ride the named config class, got: {stderr}"
        );
        assert!(
            stderr.contains("--console-assets"),
            "the refusal must name the field, got: {stderr}"
        );
        assert!(
            stderr.contains("symlink"),
            "the refusal must name the fix (the alias), got: {stderr}"
        );
    }

    // (b) The same bytes by their actual host path are admitted — the
    // refusal is the alias, not the bundle.
    let actual = bundle.canonicalize().unwrap();
    assert!(
        hagency::console::Console::load(&actual).is_ok(),
        "the real host path must admit"
    );
}
