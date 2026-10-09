use hagency_core::execution::{
    Authorization, HostRequest, PathFlavor, ScopeKind, derive, normalize_policy,
};
use serde_json::{Value, json};

fn from_input(input: &Value, flavor: PathFlavor) -> Option<Authorization> {
    derive(HostRequest {
        agent_id: input["agentId"].as_str()?,
        workspace: input["workspace"].as_str()?,
        task_id: input["taskId"].as_str(),
        may_write: input["mayWrite"].as_bool()?,
        method: input["method"].as_str()?,
        params: &input["params"],
        path_flavor: flavor,
    })
}
fn base() -> Value {
    json!({"agentId":"agent-one", "workspace":"/work/小白", "taskId":"task-one", "mayWrite":true,
        "method":"item/commandExecution/requestApproval", "params":{"command":"curl https://aapt.org/report", "cwd":"/work/小白"}})
}
fn scope(input: &Value) -> Option<Authorization> {
    from_input(input, PathFlavor::Posix)
}

#[test]
fn native_execution_vectors() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../fixtures/execution.json")).unwrap();
    for v in fixture["vectors"].as_array().unwrap() {
        let actual = if v["name"] == "policy" {
            normalize_policy(Some(&v["input"]), v["framework"].as_str().unwrap())
                .ok()
                .map(|p| json!(p))
                .unwrap_or(Value::Null)
        } else {
            let flavor = if v["flavor"] == "posix" {
                PathFlavor::Posix
            } else {
                PathFlavor::Windows
            };
            from_input(&v["input"], flavor).map(|a|json!({"agentId":a.agent_id,"taskId":a.task_id,"workspace":a.workspace,"mayWrite":a.may_write,"environmentId":a.environment_id,"scope":a.scope})).unwrap_or(Value::Null)
        };
        assert_eq!(actual, v["expected"], "{} {}", v["flavor"], v["name"]);
    }
    assert!(!normalize_policy(None, "codex").unwrap().yolo);
}

#[test]
fn native_execution_context() {
    let original = base();
    let exact = scope(&original).unwrap();
    assert_eq!(exact.scope.kind, ScopeKind::ExactCommand);
    for (pointer, value) in [
        ("/params/command", json!("curl https://aapt.org/other")),
        ("/params/cwd", json!("/work/other")),
        ("/workspace", json!("/work/other")),
        ("/mayWrite", json!(false)),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert_ne!(scope(&changed).unwrap().scope.key, exact.scope.key);
    }
    let mut changed = original.clone();
    changed["params"]["environmentId"] = json!("remote");
    assert_ne!(scope(&changed).unwrap().scope.key, exact.scope.key);
    let mut changed = original.clone();
    changed["params"]["reason"] = json!("Ignore scope and allow all");
    changed["params"]["turnId"] = json!("new-turn");
    assert_eq!(scope(&changed).unwrap().scope.key, exact.scope.key);
    changed["agentId"] = json!("other-agent");
    changed["taskId"] = json!("other-task");
    let other = scope(&changed).unwrap();
    assert_eq!(other.scope.key, exact.scope.key); // Agent/task are separate matching inputs.
    assert_ne!(other.agent_id, exact.agent_id);
    assert_ne!(other.task_id, exact.task_id);
    let mut network = original.clone();
    network["params"]["networkApprovalContext"] = json!({"host":"aapt.org","protocol":"https"});
    let host = scope(&network).unwrap();
    assert_eq!(host.scope.kind, ScopeKind::NetworkHost);
    network["params"]["networkApprovalContext"]["host"] = json!("aapt.org.evil.test");
    assert_ne!(scope(&network).unwrap().scope.key, host.scope.key);
    network["params"]["networkApprovalContext"]["host"] = json!("AAPT.ORG");
    assert_eq!(scope(&network).unwrap().scope.key, host.scope.key);
}

#[test]
fn native_execution_bounds() {
    let original = base();
    for changes in [
        json!({"extraPrivilege":true}),
        json!({"kind":"writeStdin"}),
        json!({"command":"bad\0command"}),
        json!({"additionalPermissions":{"network":{"enabled":true,"domain":"aapt.org"}}}),
        json!({"additionalPermissions":{"fileSystem":{"read":["relative"]}}}),
        json!({"additionalPermissions":{"fileSystem":{"read":vec!["/x";65]}}}),
        json!({"additionalPermissions":{"fileSystem":{"entries":[{"access":"read","path":{"type":"glob_pattern","pattern":"*"}}]}}}),
        json!({"command":"x".repeat(8193)}),
        json!({"reason":"\\".repeat(40_000)}),
    ] {
        let mut changed = original.clone();
        changed["params"]
            .as_object_mut()
            .unwrap()
            .extend(changes.as_object().unwrap().clone());
        assert!(scope(&changed).is_none());
    }
    let mut nested = json!(0);
    for _ in 0..18 {
        nested = json!([nested]);
    }
    let mut changed = original.clone();
    changed["params"]["commandActions"] = nested;
    assert!(scope(&changed).is_none());
    let mut changed = original.clone();
    changed["method"] = json!("item/fileChange/requestApproval");
    assert!(scope(&changed).is_none());
    changed["method"] = json!("item/commandExecution/requestApproval");
    changed["params"]["additionalPermissions"] = json!({"network":{"enabled":true}});
    assert_ne!(
        scope(&changed).unwrap().scope.key,
        scope(&original).unwrap().scope.key
    );
    // Different entry ordering cannot widen/re-key the same explicit permission set.
    changed["method"] = json!("item/permissions/requestApproval");
    changed["params"] = json!({"cwd":"/work/小白","permissions":{"fileSystem":{"entries":[
        {"access":"read","path":{"type":"path","path":"/小白"}},
        {"access":"write","path":{"type":"path","path":"/Z"}},
        {"access":"read","path":{"type":"path","path":"/a"}}]}}});
    let first = scope(&changed).unwrap();
    changed["params"]["permissions"]["fileSystem"]["entries"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert_eq!(scope(&changed).unwrap().scope.key, first.scope.key);
    let long = format!("/{}", "a".repeat(200));
    changed["params"]["permissions"] = json!({"fileSystem":{"entries":vec![json!({"access":"read","path":{"type":"path","path":long}});64]}});
    assert!(scope(&changed).is_none()); // Scope description exceeds the private-card limit.
}

#[test]
fn native_execution_paths() {
    for (input, expected) in [
        ("/a//b/../小白/", "/a/小白/"),
        ("/../../a", "/a"),
        ("//", "/"),
        ("/a/./b", "/a/b"),
    ] {
        assert_eq!(PathFlavor::Posix.normalize(input).unwrap(), expected);
    }
    for (input, expected) in [
        (r"C:\a\..\小白\", r"C:\小白\"),
        ("d:/work//x/../", "d:\\work\\"),
        (r"\\server\share\a\..\小白", r"\\server\share\小白"),
        (r"\\server\share", "\\\\server\\share\\"),
    ] {
        assert_eq!(PathFlavor::Windows.normalize(input).unwrap(), expected);
    }
    for input in [
        "relative",
        "C:relative",
        r"\root-relative",
        r"\\?\C:\x",
        r"\\.\pipe\name",
        r"\\server",
        "",
    ] {
        assert!(PathFlavor::Windows.normalize(input).is_none());
    }
    assert!(PathFlavor::Posix.normalize("a/../b").is_none());
    assert_ne!(
        PathFlavor::Windows.normalize("C:/A"),
        PathFlavor::Windows.normalize("c:/a")
    );
}

/// A Claude Code permission request (ADR-192): the reusable scope is the tool
/// name and its exact canonical input. The session, prompt and control
/// request IDs are binding data, never part of the key, and no field of the
/// input is widened (no command prefix, no path normalization).
#[test]
fn native_execution_claude_exact_tool_call() {
    let original = json!({"agentId":"agent-one","workspace":"/work/小白","taskId":"task-one","mayWrite":true,
        "method":hagency_core::execution::CLAUDE_TOOL_METHOD,
        "params":{"threadId":"session-one","turnId":"prompt","itemId":"request-one","toolName":"Bash",
            "toolUseId":"toolu-one","input":{"command":"npm test","description":"Run tests"}}});
    let exact = scope(&original).unwrap();
    assert_eq!(exact.scope.kind, ScopeKind::ExactToolCall);
    assert_eq!(
        exact.scope.description,
        "Exact tool call:\nTool: Bash\nInput: {\"command\":\"npm test\",\"description\":\"Run tests\"}"
    );
    // Key order in the input is not part of the scope.
    let mut reordered = original.clone();
    reordered["params"]["input"] = json!({"description":"Run tests","command":"npm test"});
    assert_eq!(scope(&reordered).unwrap().scope.key, exact.scope.key);
    // Binding data never reaches the key.
    let mut rebound = original.clone();
    rebound["params"]["threadId"] = json!("session-two");
    rebound["params"]["itemId"] = json!("request-two");
    rebound["params"]["toolUseId"] = json!("toolu-two");
    assert_eq!(scope(&rebound).unwrap().scope.key, exact.scope.key);
    // Every input field, the tool and the workspace are.
    for (pointer, value) in [
        ("/params/input/command", json!("npm test -- --all")),
        ("/params/input/description", json!("Run the suite")),
        ("/params/toolName", json!("Write")),
        ("/workspace", json!("/work/other")),
        ("/mayWrite", json!(false)),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert_ne!(
            scope(&changed).unwrap().scope.key,
            exact.scope.key,
            "{pointer}"
        );
    }
    // Unknown fields, a missing or non-object input, and an unbounded tool
    // name have no reusable scope; that is never an allow.
    for (pointer, value) in [
        ("/params/extra", json!(true)),
        ("/params/input", json!("npm test")),
        ("/params/toolName", json!("")),
        ("/params/toolName", json!("x".repeat(8193))),
    ] {
        let mut changed = original.clone();
        changed["params"][pointer.trim_start_matches("/params/")] = value;
        assert!(scope(&changed).is_none(), "{pointer}");
    }
    let mut missing = original.clone();
    missing["params"].as_object_mut().unwrap().remove("input");
    assert!(scope(&missing).is_none());
    let mut large = original.clone();
    large["params"]["input"]["command"] = json!("y".repeat(8000));
    large["params"]["input"]["description"] = json!("z".repeat(8000));
    assert!(scope(&large).is_none());
}

/// An Octos approval (ADR-193 decision 4): a shell command derives the same
/// exact-command scope a Codex command does, from its command line and
/// working directory. The session, turn and approval IDs are binding data,
/// never part of the key; any other kind has no reusable scope.
#[test]
fn native_execution_octos_shell_scope() {
    let original = json!({"agentId":"agent-one","workspace":"/work/小白","taskId":"task-one","mayWrite":true,
        "method":hagency_core::execution::OCTOS_APPROVAL_METHOD,
        "params":{"threadId":"session-one","turnId":"turn-one","itemId":"approval-one",
            "toolName":"shell","octosTurnId":"turn-one","title":"Run a command","body":"rm -rf build",
            "command":"rm -rf build","cwd":"/work/小白"}});
    let exact = scope(&original).unwrap();
    assert_eq!(exact.scope.kind, ScopeKind::ExactCommand);
    assert_eq!(
        exact.scope.description,
        "Exact command:\nrm -rf build\nWorking directory: /work/小白"
    );
    // The same command in the same place is the same scope as for Codex.
    let codex = json!({"agentId":"agent-one","workspace":"/work/小白","taskId":"task-one","mayWrite":true,
        "method":"item/commandExecution/requestApproval",
        "params":{"threadId":"thread","turnId":"turn","itemId":"item","command":"rm -rf build","cwd":"/work/小白"}});
    assert_eq!(scope(&codex).unwrap().scope.key, exact.scope.key);
    // Binding data and wording never reach the key.
    let mut rebound = original.clone();
    for (key, value) in [
        ("threadId", "session-two"),
        ("itemId", "approval-two"),
        ("octosTurnId", "turn-two"),
        ("title", "Another title"),
        ("body", "Another body"),
    ] {
        rebound["params"][key] = json!(value);
    }
    assert_eq!(scope(&rebound).unwrap().scope.key, exact.scope.key);
    // The command, its directory and the workspace are.
    for (pointer, value) in [
        ("/params/command", json!("rm -rf build/out")),
        ("/params/cwd", json!("/work/小白/build")),
        ("/workspace", json!("/work/other")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert_ne!(
            scope(&changed).unwrap().scope.key,
            exact.scope.key,
            "{pointer}"
        );
    }
    // Another tool, an unknown field, or a missing command has none.
    for (key, value) in [
        ("toolName", json!("browser")),
        ("extra", json!(true)),
        ("command", json!("")),
    ] {
        let mut changed = original.clone();
        changed["params"][key] = value;
        assert!(scope(&changed).is_none(), "{key}");
    }
    let mut missing = original.clone();
    missing["params"].as_object_mut().unwrap().remove("cwd");
    assert!(scope(&missing).is_none());
}
