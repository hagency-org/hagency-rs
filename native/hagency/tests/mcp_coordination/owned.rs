use super::fixture::*;
use hagency_runtime::task_mcp::COORDINATION_TOOLS;
use serde_json::json;

#[tokio::test]
async fn native_owned_profile_serves_coordination_only_with_the_marker() {
    let f = Fixture::new().await;
    let args = json!({"call_id":"owned-delegate","assignee_engagement":f.agents[1],"input_sequences":[f.seq],"definition":{"title":"Implement sum","parent_id":f.parent.task_id}});

    // Without the marker the owned profile is unchanged and refuses delegation.
    let off = f.owned_client(None).await;
    let names = off.tool_names().await;
    assert!(
        COORDINATION_TOOLS
            .iter()
            .skip(1)
            .all(|n| !names.contains(&(*n).to_owned()))
    );
    assert!(!names.contains(&"comment_task".to_owned()));
    let refused = off
        .service
        .call_tool(
            rmcp::model::CallToolRequestParams::new("delegate_task")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(f.count("canonical_tasks"), 1);
    off.close().await;

    // With the marker the eight coordination names are listed and no graph tool.
    let on = f.owned_client(Some("1")).await;
    let names = on.tool_names().await;
    for name in COORDINATION_TOOLS {
        assert!(
            names.contains(&name.to_owned()),
            "{name} missing: {names:?}"
        );
    }
    assert!(names.iter().all(|n| !n.contains("graph")), "{names:?}");
    let refused = on
        .service
        .call_tool(rmcp::model::CallToolRequestParams::new("create_graph"))
        .await;
    assert!(refused.is_err(), "{refused:?}");

    // delegate_task creates the assignee's pending intent and task.
    let result = on.ok("delegate_task", args).await;
    assert_eq!(result["activation"], "pending");
    assert_eq!(f.count("canonical_tasks"), 2);
    on.close().await;

    // Any value other than 1 is invalid, not silently off.
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_hagency"))
        .args(["mcp", "--owned-task-profile"])
        .env_clear()
        .env("HAGENCY_RUNNER_API_ADDR", f.address.to_string())
        .env(
            "HAGENCY_RUNNER_CAPABILITY",
            serde_json::to_string(&f.cap).unwrap(),
        )
        .env("HAGENCY_TASK_ID", &f.parent.task_id)
        .env("HAGENCY_COORDINATION_TOOLS", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    f.close().await;
}

#[tokio::test]
async fn native_plugin_delegate_task_creates_pending_work_for_the_assignee() {
    let f = Fixture::new().await;
    let c = f.owned_client(Some("1")).await;
    let parent = c.ok("get_task", json!({"id":f.parent.task_id})).await;
    let args = json!({"call_id":"plugin-delegate","assignee_engagement":f.agents[1],"input_sequences":[f.seq],"definition":{"title":"Implement sum","parent_id":f.parent.task_id}});
    let result = c.ok("delegate_task", args.clone()).await;
    assert_eq!(result["activation"], "pending");
    assert_eq!(f.count("canonical_tasks"), 2);
    // The assignee has a new task of its own, in a session that is not ours.
    assert_ne!(result["session_id"], f.parent.session_id);
    let created = c.ok("get_task", json!({"id":result["task_id"]})).await;
    assert_eq!(created["task"]["id"], result["task_id"]);
    assert_ne!(created["task"]["id"], f.parent.task_id);
    // The delegator's own task is unchanged and not completed by delegating.
    let after = c.ok("get_task", json!({"id":f.parent.task_id})).await;
    assert_eq!(after["task"], parent["task"]);
    // The same call replays the same task.
    let replay = c.ok("delegate_task", args).await;
    assert_eq!(replay["task_id"], result["task_id"]);
    assert_eq!(replay["replayed"], true);
    assert_eq!(f.count("canonical_tasks"), 2);
    c.close().await;
    f.close().await;
}
