use hagency_plugin::{
    ApprovalDecision, CommandEnvironment, CommandRequest, EventQueue, HarnessFrame,
    HarnessNotification, HarnessPluginRef, HarnessRequest, HostRequest, MAX_EVENT_QUEUE,
    Manifest, PluginCommand, PluginError, PluginService, PluginSource, PluginToolApproval,
    RedactedEvent, RefusalCode, RestartNotice, RuntimeOutcome, RuntimePluginRef,
    RuntimeQualification, RuntimeSession, Scheduler, ToolResultError, decode_frame,
    decode_harness_frame, encode_harness_request, encode_request, parse_tool_result, run_command,
};
use serde_json::json;
use std::{ffi::OsStr, fs, path::Path, time::Duration};
use tempfile::{TempDir, tempdir};

fn package() -> (TempDir, String) {
    let dir = tempdir().expect("tempdir");
    let manifest = r#"
schema = "hagency.plugin/v1"
id = "example.plugin"
name = "Example"
version = "1.0.0"
min_hagency_version = "0.1.0"
platforms = ["linux-x86_64"]

[[actions]]
id = "ping"
title = "Ping"
command = ["printf", "{\"ok\":true}"]
"#;
    fs::write(dir.path().join("hagency-plugin.toml"), manifest).expect("manifest");
    (dir, manifest.into())
}
fn command_environment(root: &Path) -> CommandEnvironment {
    CommandEnvironment {
        plugin_id: "example.plugin".into(),
        plugin_root: root.into(),
        config_dir: root.join("config"),
        state_dir: root.join("state"),
        entrypoint_id: "action".into(),
        path: None,
        workspace: None,
    }
}

#[test]
fn native_plugin_manifest_accepts_closed_schema() {
    let (_, manifest) = package();
    let parsed = Manifest::parse(manifest.as_bytes()).expect("valid manifest");
    assert_eq!(parsed.id, "example.plugin");
    assert!(Manifest::parse(format!("{manifest}\nunknown = true\n").as_bytes()).is_err());
    assert!(
        Manifest::parse(
            manifest
                .replace("example.plugin", "Example.plugin")
                .as_bytes()
        )
        .is_err()
    );
}

#[test]
fn native_plugin_github_source_requires_full_commit() {
    assert!(PluginSource::github("owner", "repo", None, &"a".repeat(39)).is_err());
    assert!(PluginSource::github("owner", "repo", None, &"A".repeat(40)).is_err());
    let source = PluginSource::github("owner", "repo", Some("examples/plugin"), &"a".repeat(40))
        .expect("full pin");
    assert_eq!(source.commit(), Some("a".repeat(40).as_str()));
}

#[test]
fn native_plugin_registry_preserves_active_on_failed_apply() {
    let (old, _) = package();
    let state = tempdir().expect("state");
    let manager = PluginService::open(state.path()).expect("manager");
    manager.link(old.path()).expect("link");
    manager.enable("example.plugin").expect("enable");
    let replacement = tempdir().expect("replacement");
    fs::write(
        replacement.path().join("hagency-plugin.toml"),
        fs::read(old.path().join("hagency-plugin.toml")).expect("read"),
    )
    .expect("replacement manifest");
    let preview = manager
        .preview_install(PluginSource::linked(replacement.path()).expect("source"))
        .expect("preview");
    assert!(matches!(
        manager.apply_install(&preview.preview_id, "wrong"),
        Err(PluginError::Untrusted)
    ));
    let active = manager.inspect("example.plugin").expect("active");
    assert!(active.enabled);
    assert_eq!(active.install_dir, old.path());
}

#[test]
fn native_plugin_untrusted_preview_can_be_confirmed_later() {
    let (dir, _) = package();
    let state = tempdir().expect("state");
    let manager = PluginService::open(state.path()).expect("manager");
    let preview = manager
        .preview_install(PluginSource::linked(dir.path()).expect("source"))
        .expect("preview");
    assert!(matches!(
        manager.apply_install(&preview.preview_id, "wrong"),
        Err(PluginError::Untrusted)
    ));
    assert!(
        manager
            .apply_install(&preview.preview_id, &preview.source_sha256)
            .is_ok()
    );
}

#[test]
fn native_plugin_runtime_qualification_requires_enabled_matching_package() {
    let (dir, manifest) = package();
    fs::write(
        dir.path().join("hagency-plugin.toml"),
        format!(
            "{manifest}\n[[runtimes]]\nid = \"adapter\"\nframework = \"custom\"\ncommand = [\"runtime\"]\nprotocol = \"hagency.runtime/v1\"\n"
        ),
    )
    .expect("runtime manifest");
    fs::write(dir.path().join("runtime"), b"runtime").expect("runtime entrypoint");
    let state = tempdir().expect("state");
    let manager = PluginService::open(state.path()).expect("manager");
    let record = manager.link(dir.path()).expect("link");
    let qualification = RuntimeQualification {
        runtime: RuntimePluginRef {
            plugin_id: record.id.clone(),
            adapter_id: "adapter".into(),
            package_sha256: record.package_sha256.clone(),
            settings_sha256: record.settings_sha256.clone(),
        },
        framework: "custom".into(),
        model: "model".into(),
        provider: "provider".into(),
        reasoning: None,
        evidence_digest: "a".repeat(64),
        roles: vec!["review".into()],
    };
    assert!(matches!(
        manager.qualify_runtime(&qualification),
        Err(PluginError::Disabled(_))
    ));
    manager.enable(&record.id).expect("enable");
    manager.qualify_runtime(&qualification).expect("qualify");
    let launch = manager
        .runtime_launch(&qualification.runtime, &qualification.framework)
        .expect("launch");
    assert_eq!(launch.executable, dir.path().join("runtime"));
    assert!(launch.arguments.is_empty());
    let lease = manager.claim_runtime(&qualification).expect("claim");
    assert_eq!(lease.plugin_id(), record.id);
    drop(lease);
    manager.disable(&record.id).expect("disable");
    assert!(matches!(
        manager.claim_runtime(&qualification),
        Err(PluginError::Disabled(_))
    ));
    manager.enable(&record.id).expect("re-enable");
    let mut wrong = qualification.clone();
    wrong.runtime.package_sha256 = "b".repeat(64);
    assert!(matches!(
        manager.qualify_runtime(&wrong),
        Err(PluginError::Untrusted)
    ));
}

#[test]
fn native_plugin_settings_pin_launch_revision() {
    let (dir, manifest) = package();
    fs::write(
        dir.path().join("hagency-plugin.toml"),
        format!(
            "{manifest}\n[[runtimes]]\nid = \"adapter\"\nframework = \"custom\"\ncommand = [\"runtime\"]\nprotocol = \"hagency.runtime/v1\"\n[[settings]]\nid = \"endpoint\"\nlabel = \"Endpoint\"\nkind = \"text\"\nrequired = true\n"
        ),
    )
    .expect("settings manifest");
    fs::write(dir.path().join("runtime"), b"runtime").expect("runtime entrypoint");
    let state = tempdir().expect("state");
    let manager = PluginService::open(state.path()).expect("manager");
    let record = manager.link(dir.path()).expect("link");
    let old_settings = manager.settings(&record.id).expect("initial settings");
    assert!(!old_settings.settings_ready);
    manager.enable(&record.id).expect("enable");
    let old_runtime = RuntimePluginRef {
        plugin_id: record.id.clone(),
        adapter_id: "adapter".into(),
        package_sha256: record.package_sha256.clone(),
        settings_sha256: old_settings.document.settings_sha256.clone(),
    };
    assert!(matches!(
        manager.runtime_launch(&old_runtime, "custom"),
        Err(PluginError::ConfigurationRequired(_))
    ));
    let mut values = std::collections::BTreeMap::new();
    values.insert("endpoint".into(), json!("https://provider.example.invalid"));
    let updated = manager
        .update_settings(&record.id, old_settings.document.revision, values)
        .expect("update settings");
    assert!(updated.settings_ready);
    assert_ne!(
        updated.document.settings_sha256,
        old_runtime.settings_sha256
    );
    let launch = manager
        .runtime_launch(
            &RuntimePluginRef {
                settings_sha256: updated.document.settings_sha256.clone(),
                ..old_runtime
            },
            "custom",
        )
        .expect("launch configured revision");
    let config_dir = manager
        .runtime_environment(&launch, dir.path().join("workspace"))
        .expect("environment")
        .build()
        .get(OsStr::new("HAGENCY_PLUGIN_CONFIG_DIR"))
        .cloned()
        .expect("config env");
    assert!(
        config_dir
            .to_string_lossy()
            .contains(&updated.document.settings_sha256)
    );
}
#[test]
fn native_plugin_harness_launch_requires_enabled_matching_package() {
    let (dir, manifest) = package();
    fs::write(
        dir.path().join("hagency-plugin.toml"),
        format!(
            "{manifest}\n[[harnesses]]\nid = \"reference\"\ncommand = [\"harness\", \"--stdio\"]\nprotocol = \"hagency.harness/v1\"\n"
        ),
    )
    .expect("harness manifest");
    fs::write(dir.path().join("harness"), b"harness").expect("harness entrypoint");
    let state = tempdir().expect("state");
    let manager = PluginService::open(state.path()).expect("manager");
    let record = manager.link(dir.path()).expect("link");
    let harness = HarnessPluginRef {
        plugin_id: record.id.clone(),
        harness_id: "reference".into(),
        package_sha256: record.package_sha256.clone(),
        settings_sha256: record.settings_sha256.clone(),
    };
    assert!(matches!(
        manager.harness_launch(&harness),
        Err(PluginError::Disabled(_))
    ));
    manager.enable(&record.id).expect("enable");
    let launch = manager.harness_launch(&harness).expect("launch");
    assert_eq!(launch.executable, dir.path().join("harness"));
    assert_eq!(launch.arguments, vec![std::ffi::OsString::from("--stdio")]);
    let environment = manager
        .harness_environment(&launch, dir.path().join("workspace"))
        .expect("environment")
        .build();
    assert_eq!(
        environment.get(OsStr::new("HAGENCY_PLUGIN_ENTRYPOINT_ID")),
        Some(&OsStr::new("reference").to_owned())
    );
    let lease = manager.claim_harness(&harness).expect("claim");
    assert_eq!(lease.plugin_id(), record.id);
}

#[test]
fn native_plugin_harness_templates_resolve_defaults_and_reject_invalid_values() {
    let (dir, manifest) = package();
    fs::write(
        dir.path().join("hagency-plugin.toml"),
        format!(
            "{manifest}\n[[harnesses]]\nid = \"reference\"\ncommand = [\"harness\"]\nprotocol = \"hagency.harness/v1\"\n[[harnesses.configuration]]\nid = \"prompt\"\nlabel = \"Prompt\"\nkind = \"textarea\"\nrequired = true\nmax_length = 32\n[[harnesses.configuration]]\nid = \"mode\"\nlabel = \"Mode\"\nkind = \"text\"\ndefault = \"safe\"\n"
        ),
    )
    .expect("harness manifest");
    fs::write(dir.path().join("harness"), b"harness").expect("harness entrypoint");
    let state = tempdir().expect("state");
    let manager = PluginService::open(state.path()).expect("manager");
    let record = manager.link(dir.path()).expect("link");
    manager.enable(&record.id).expect("enable");
    let harness = HarnessPluginRef {
        plugin_id: record.id.clone(),
        harness_id: "reference".into(),
        package_sha256: record.package_sha256.clone(),
        settings_sha256: record.settings_sha256.clone(),
    };
    let mut supplied = std::collections::BTreeMap::new();
    supplied.insert("prompt".into(), json!("hello\nworld"));
    let document = manager
        .resolve_harness_template(&harness, &supplied)
        .expect("resolve template");
    assert_eq!(document.values.get("prompt"), Some(&json!("hello\nworld")));
    assert_eq!(document.values.get("mode"), Some(&json!("safe")));
    assert_eq!(
        manager
            .validate_harness_template(&document)
            .expect("validate template"),
        harness
    );
    supplied.insert("prompt".into(), json!("changed"));
    let changed = manager
        .resolve_harness_template(&harness, &supplied)
        .expect("resolve changed template");
    assert_ne!(document.template_sha256, changed.template_sha256);

    supplied.insert("unknown".into(), json!(true));
    assert!(matches!(
        manager.resolve_harness_template(&harness, &supplied),
        Err(PluginError::SettingsInvalid(_))
    ));
    supplied.remove("unknown");
    supplied.insert("prompt".into(), json!("x".repeat(33)));
    assert!(matches!(
        manager.resolve_harness_template(&harness, &supplied),
        Err(PluginError::SettingsInvalid(_))
    ));
    let mut tampered = document;
    tampered.values.insert("unknown".into(), json!(true));
    assert!(matches!(
        manager.validate_harness_template(&tampered),
        Err(PluginError::SettingsInvalid(_))
    ));
}

#[test]
fn native_plugin_manifest_textarea_bounds_and_template_digest() {
    let (_, manifest) = package();
    let valid = format!(
        "{manifest}\n[[harnesses]]\nid = \"reference\"\ncommand = [\"harness\"]\nprotocol = \"hagency.harness/v1\"\n[[harnesses.configuration]]\nid = \"prompt\"\nlabel = \"Prompt\"\nkind = \"textarea\"\nmax_length = 4\ndefault = \"a\\nb\"\n"
    );
    assert!(Manifest::parse(valid.as_bytes()).is_ok());
    let invalid = [
        format!(
            "{manifest}\n[[harnesses]]\nid = \"reference\"\ncommand = [\"harness\"]\nprotocol = \"hagency.harness/v1\"\n[[harnesses.configuration]]\nid = \"prompt\"\nlabel = \"Prompt\"\nkind = \"textarea\"\nmax_length = 0\n"
        ),
        format!(
            "{manifest}\n[[harnesses]]\nid = \"reference\"\ncommand = [\"harness\"]\nprotocol = \"hagency.harness/v1\"\n[[harnesses.configuration]]\nid = \"prompt\"\nlabel = \"Prompt\"\nkind = \"textarea\"\nmax_length = 32769\n"
        ),
        format!(
            "{manifest}\n[[harnesses]]\nid = \"reference\"\ncommand = [\"harness\"]\nprotocol = \"hagency.harness/v1\"\n[[harnesses.configuration]]\nid = \"prompt\"\nlabel = \"Prompt\"\nkind = \"textarea\"\nmax_length = 4\ndefault = \"a\\u0001\"\n"
        ),
    ];
    for candidate in invalid {
        assert!(Manifest::parse(candidate.as_bytes()).is_err());
    }
}

#[test]
fn native_plugin_linked_source_never_builds_or_collects() {
    let (dir, _) = package();
    let state = tempdir().expect("state");
    let manager = PluginService::open(state.path()).expect("manager");
    let record = manager.link(dir.path()).expect("link");
    assert!(matches!(record.source, PluginSource::Linked { .. }));
    manager.collect_obsolete("example.plugin").expect("collect");
    assert!(dir.path().join("hagency-plugin.toml").exists());
}

#[test]
fn native_plugin_command_environment_is_allowlisted() {
    let dir = tempdir().expect("root");
    let env = command_environment(dir.path()).build();
    assert!(env.contains_key(OsStr::new("HAGENCY_PLUGIN_ID")));
    assert!(env.contains_key(OsStr::new("HAGENCY_PLUGIN_ROOT")));
    assert!(!env.contains_key(OsStr::new("HAGENCY_OPERATOR_TOKEN")));
    assert!(!env.contains_key(OsStr::new("CODEX_HOME")));
}

#[test]
fn native_plugin_scheduler_enforces_global_and_plugin_limits() {
    let scheduler = Scheduler::new();
    let _one = scheduler.try_acquire("example.plugin").expect("first");
    let _two = scheduler.try_acquire("example.plugin").expect("second");
    assert!(scheduler.try_acquire("example.plugin").is_err());
    drop(_one);
    drop(_two);
    let mut slots = Vec::new();
    for index in 0..16 {
        slots.push(
            scheduler
                .try_acquire(&format!("plugin{index}"))
                .expect("global slot"),
        );
    }
    assert!(scheduler.try_acquire("overflow").is_err());
}

#[test]
fn native_plugin_timeout_requires_whole_tree_cleanup() {
    let dir = tempdir().expect("cwd");
    let result = run_command(CommandRequest {
        argv: vec!["sh".into(), "-c".into(), "sleep 1".into()],
        cwd: dir.path().into(),
        stdin: Vec::new(),
        environment: command_environment(dir.path()),
        deadline: Duration::from_millis(5),
    });
    assert!(matches!(
        result,
        Err(hagency_plugin::CommandError::OutcomeUnknown)
    ));
}

#[test]
fn native_plugin_event_queue_redacts_and_reports_loss() {
    let mut queue = EventQueue::default();
    for index in 0..(MAX_EVENT_QUEUE + 1) {
        queue.push(
            RedactedEvent::new(
                "dispatch.started",
                &index.to_string(),
                "working",
                index as u64,
            )
            .expect("event"),
        );
    }
    let (events, lost) = queue.drain();
    assert_eq!(events.len(), MAX_EVENT_QUEUE);
    assert_eq!(lost, 1);
    assert!(RedactedEvent::new("unknown", "id", "state", 0).is_err());
}

#[test]
fn native_plugin_tool_catalog_and_owner_card() {
    let input = json!({"path":"opaque"});
    let card = PluginToolApproval::new("plugin", "example.plugin", "read", "digest", &input)
        .expect("card");
    assert_eq!(card.plugin_id, "example.plugin");
    assert_ne!(card.input_sha256, "");
}

#[test]
fn native_plugin_tool_denial_and_exact_approval_are_service_gated() {
    let (dir, manifest) = package();
    fs::write(
        dir.path().join("hagency-plugin.toml"),
        format!(
            "{manifest}\n[[tools]]\nid = \"read\"\ntitle = \"Read\"\ndescription = \"Read opaque input\"\ninput_schema = {{ type = \"object\" }}\ncommand = [\"printf\", \"{{\\\"content\\\":[{{\\\"type\\\":\\\"text\\\",\\\"text\\\":\\\"ok\\\"}}],\\\"isError\\\":false}}\"]\n"
        ),
    )
    .expect("tool manifest");
    let state = tempdir().expect("state");
    let manager = PluginService::open(state.path()).expect("manager");
    let record = manager.link(dir.path()).expect("link");
    manager.enable(&record.id).expect("enable");
    let catalog = manager.tool_catalog(&record.id).expect("catalog");
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].name, "plugin.example.plugin.read");

    let input = json!({"path":"opaque"});
    let approval = manager
        .prepare_tool_approval("owner", &record.id, "read", &input)
        .expect("approval");
    let denied = manager
        .invoke_approved_tool(&approval, &input, ApprovalDecision::Deny)
        .expect("denial");
    assert!(denied.is_error);
    let approved = manager.invoke_approved_tool(
        &approval,
        &json!({"path":"different"}),
        ApprovalDecision::ApproveOnce,
    );
    assert!(matches!(approved, Err(PluginError::Untrusted)));
}

#[test]
fn native_plugin_tool_malformed_or_lost_result_is_unknown() {
    assert!(matches!(
        parse_tool_result(b"{}"),
        Err(ToolResultError::Malformed)
    ));
    assert!(matches!(
        parse_tool_result(br#"{"content":[{"type":"text","text":"ok"}],"isError":false}"#)
            .expect("result")
            .is_error,
        false
    ));
}

#[test]
fn native_plugin_runtime_frames_validate_protocol_and_ids() {
    let request = encode_request(
        "1",
        &HostRequest::Initialize {
            protocol: "hagency.runtime/v1".into(),
            capabilities: vec!["tool.v1".into()],
        },
    )
    .expect("request");
    assert!(request.ends_with(b"\n"));
    assert!(decode_frame(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#).is_err());
    assert!(decode_frame(br#"{"jsonrpc":"2.0","method":"future","params":{}}"#).is_err());
}

#[test]
fn native_plugin_runtime_resource_identity_is_immutable() {
    let first = RuntimePluginRef {
        plugin_id: "example.plugin".into(),
        adapter_id: "adapter".into(),
        package_sha256: "a".repeat(64),
        settings_sha256: "c".repeat(64),
    };
    let changed = RuntimePluginRef {
        package_sha256: "b".repeat(64),
        ..first.clone()
    };
    assert_ne!(first, changed);
}

#[test]
fn native_plugin_runtime_qualification_gates_resource_claims() {
    let runtime = RuntimePluginRef {
        plugin_id: "example.plugin".into(),
        adapter_id: "adapter".into(),
        package_sha256: "a".repeat(64),
        settings_sha256: "c".repeat(64),
    };
    let qualification = RuntimeQualification {
        runtime: runtime.clone(),
        framework: "custom".into(),
        model: "model".into(),
        provider: "provider".into(),
        reasoning: None,
        evidence_digest: "e".repeat(64),
        roles: vec!["review".into()],
    };
    assert!(qualification.matches(&runtime, "model", "provider", None));
    assert!(!qualification.matches(&runtime, "other", "provider", None));
}

#[test]
fn native_plugin_runtime_dispatch_settles_through_existing_path() {
    let mut session = RuntimeSession::new("dispatch", "turn").expect("session");
    let frame = decode_frame(br#"{"jsonrpc":"2.0","method":"turn/result","params":{"dispatchId":"dispatch","turnId":"turn","outcome":"completed","finalText":"PLUGIN_OK","usage":null}}"#).expect("frame");
    let notification = session
        .accept(frame)
        .expect("accept")
        .expect("notification");
    assert!(matches!(
        notification,
        hagency_plugin::PluginNotification::TurnResult {
            outcome: RuntimeOutcome::Completed,
            ..
        }
    ));
}

#[test]
fn native_plugin_runtime_cleanup_unknown_refuses_settlement() {
    let mut session = RuntimeSession::new("dispatch", "turn").expect("session");
    let frame = decode_frame(br#"{"jsonrpc":"2.0","method":"turn/result","params":{"dispatchId":"other","turnId":"turn","outcome":"completed"}}"#).expect("frame");
    assert!(session.accept(frame).is_err());
}


#[test]
fn native_plugin_cli_uses_operator_routes_and_exact_subcommands() {
    assert!(matches!(
        PluginCommand::parse(&["list"]).expect("list"),
        PluginCommand::List
    ));
    assert!(matches!(
        PluginCommand::parse(&["config-dir", "example.plugin"]).expect("config"),
        PluginCommand::ConfigDir(_)
    ));
    assert!(PluginCommand::parse(&["unknown"]).is_err());
}

#[test]
fn native_plugin_install_requires_exact_trust_confirmation() {
    let (dir, _) = package();
    let state = tempdir().expect("state");
    let manager = PluginService::open(state.path()).expect("manager");
    let preview = manager
        .preview_install(PluginSource::linked(dir.path()).expect("source"))
        .expect("preview");
    assert!(matches!(
        manager.apply_install(&preview.preview_id, "different"),
        Err(PluginError::Untrusted)
    ));
}

#[test]
fn native_console_plugins_renders_bounded_state_without_plugin_markup() {
    let (dir, _) = package();
    let state = tempdir().expect("state");
    let manager = PluginService::open(state.path()).expect("manager");
    let record = manager.link(dir.path()).expect("link");
    let json = serde_json::to_string(&record).expect("record json");
    assert!(!json.contains("<script"));
    assert!(json.contains("packageSha256"));
}

#[test]
fn native_plugin_runtime_changes_return_restart_needed() {
    let notice = RestartNotice::runtime_changed();
    assert!(notice.restart_needed);
    assert_eq!(
        serde_json::to_value(RefusalCode::PluginRestartRequired).expect("code"),
        json!("plugin_restart_required")
    );
}

#[test]
fn native_harness_frames_round_trip_view_messages_and_requests() {
    let bytes = encode_harness_request(
        "h1",
        &HarnessRequest::Snapshot {
            sequence: 1,
            state: json!({"state": "working"}),
        },
    )
    .expect("request");
    assert!(bytes.ends_with(b"\n"));
    assert!(matches!(
        decode_harness_frame(br#"{"jsonrpc":"2.0","id":"h1","method":"stop_attempt","params":{"attemptId":"a1"}}"#),
        Ok(HarnessFrame::Request { ref id, request: HarnessRequest::StopAttempt { ref attempt_id } })
            if id == "h1" && attempt_id == "a1"
    ));
    assert!(matches!(
        decode_harness_frame(br#"{"jsonrpc":"2.0","method":"event","params":{"sequence":2,"event":{"kind":"done"}}}"#),
        Ok(HarnessFrame::Notification(HarnessNotification::Event { sequence: 2, .. }))
    ));
}

#[test]
fn native_harness_frames_reject_invalid_ids_and_notifications() {
    assert!(matches!(
        decode_harness_frame(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
        Err(hagency_plugin::HarnessProtocolError::InvalidId)
    ));
    assert!(decode_harness_frame(br#"{"jsonrpc":"2.0","method":"future","params":{}}"#).is_err());
    assert!(encode_harness_request("", &HarnessRequest::Shutdown).is_err());
}

#[test]
fn native_plugin_registry_migrates_legacy_and_clears_transient_leases() {
    let (dir, _) = package();
    let state = tempdir().expect("state");
    let plugins = state.path().join("plugins");
    fs::create_dir_all(&plugins).expect("plugins");
    fs::write(
        plugins.join("registry.json"),
        serde_json::to_vec(&json!({
            "profile": "hagency_plugin_registry_v1",
            "records": {
                "example.plugin": {
                    "id": "example.plugin",
                    "source": {"kind": "linked", "path": dir.path()},
                    "manifestVersion": "1.0.0",
                    "sourceSha256": "a".repeat(64),
                    "packageSha256": "b".repeat(64),
                    "installDir": dir.path(),
                    "enabled": false,
                    "capabilities": [],
                    "updatedAtMs": 1,
                    "activeLeases": 7,
                    "readiness": "disabled"
                }
            }
        }))
        .expect("legacy registry"),
    )
    .expect("write registry");
    let manager = PluginService::open(state.path()).expect("open migrated registry");
    let record = manager.inspect("example.plugin").expect("record");
    assert_eq!(record.revisions.len(), 1);
    assert_eq!(record.active_leases, 0);
    assert_eq!(
        record
            .revisions
            .values()
            .next()
            .expect("revision")
            .active_leases,
        0
    );
    let migrated = fs::read_to_string(plugins.join("registry.json")).expect("migrated file");
    assert!(migrated.contains("hagency_plugin_registry_v2"));
}

#[test]
fn native_plugin_revision_leases_block_uninstall_until_release() {
    let (old, manifest) = package();
    let state = tempdir().expect("state");
    let manager = PluginService::open(state.path()).expect("manager");
    let record = manager.link(old.path()).expect("old link");
    manager.enable(&record.id).expect("enable");
    let lease = manager.acquire_lease(&record.id).expect("old lease");
    let replacement = tempdir().expect("replacement");
    fs::write(
        replacement.path().join("hagency-plugin.toml"),
        manifest.replace("name = \"Example\"", "name = \"Replacement\""),
    )
    .expect("replacement manifest");
    manager.link(replacement.path()).expect("replacement link");
    let replacement_record = manager.inspect(&record.id).expect("replacement record");
    assert_ne!(replacement_record.package_sha256, record.package_sha256);
    assert_eq!(replacement_record.revisions.len(), 2);
    assert!(replacement_record.restart_required);
    manager.disable(&record.id).expect("disable");
    assert!(matches!(
        manager.uninstall(&record.id),
        Err(PluginError::Busy)
    ));
    drop(lease);
    manager
        .uninstall(&record.id)
        .expect("uninstall after release");
}

#[test]
fn native_plugin_invocation_replays_after_service_reopen() {
    let (dir, _) = package();
    let state = tempdir().expect("state");
    let manager = PluginService::open(state.path()).expect("manager");
    let record = manager.link(dir.path()).expect("link");
    manager.enable(&record.id).expect("enable");
    let submitted = manager
        .submit_action(&record.id, "ping", json!({"ok": true}), "replay-key")
        .expect("submit");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let terminal = loop {
        let current = manager.invocation(&submitted.invocation_id).expect("poll");
        if matches!(
            current.state.as_str(),
            "succeeded" | "failed" | "outcome_unknown"
        ) {
            break current;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "invocation did not settle"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(manager);
    let reopened = PluginService::open(state.path()).expect("reopen");
    let replay = reopened
        .submit_action(&record.id, "ping", json!({"ok": true}), "replay-key")
        .expect("replay");
    assert_eq!(replay.invocation_id, submitted.invocation_id);
    assert_eq!(
        reopened
            .invocation(&submitted.invocation_id)
            .expect("stored")
            .state,
        terminal.state
    );
}

#[test]
fn native_plugin_tool_service_requires_exact_approval() {
    let (dir, manifest) = package();
    fs::write(
        dir.path().join("hagency-plugin.toml"),
        format!(
            "{manifest}\n[[tools]]\nid = \"read\"\ntitle = \"Read\"\ndescription = \"bounded read\"\ninput_schema = {{type = \"object\"}}\ncommand = [\"printf\", '{{\\\"content\\\":[{{\\\"type\\\":\\\"text\\\",\\\"text\\\":\\\"ok\\\"}}],\\\"isError\\\":false}}']\n"
        ),
    )
    .expect("tool manifest");
    let state = tempdir().expect("state");
    let manager = PluginService::open(state.path()).expect("manager");
    let record = manager.link(dir.path()).expect("link");
    manager.enable(&record.id).expect("enable");
    let catalog = manager.tool_catalog(&record.id).expect("catalog");
    assert_eq!(catalog[0].name, "plugin.example.plugin.read");
    let input = json!({"path": "opaque"});
    let approval = manager
        .prepare_tool_approval("custom", &record.id, "read", &input)
        .expect("approval");
    let denied = manager
        .invoke_approved_tool(&approval, &input, ApprovalDecision::Deny)
        .expect("denial");
    assert!(denied.is_error);
    assert_eq!(
        manager
            .scheduler_snapshot(&record.id)
            .expect("scheduler")
            .global_active,
        0
    );
}

#[test]
fn reference_examples_use_closed_manifest_schema() {
    for path in [
        "../../examples/plugins/reference-runtime/hagency-plugin.toml",
        "../../examples/plugins/reference-harness/hagency-plugin.toml",
    ] {
        let bytes = fs::read(path).expect("example manifest");
        Manifest::parse(&bytes).expect("valid example manifest");
    }
}
