use std::path::Path;
use std::process::Command;

use serde_json::json;

use orca::config::Config;
use orca::daemon::Daemon;
use orca::daemon::server::IpcClient;
use orca::protocol::{RpcRequest, RpcResponse};

/// Connect a fresh IpcClient, send one request, and return the response.
async fn fresh_call(socket: &Path, method: &str, params: serde_json::Value) -> RpcResponse {
    let mut client = IpcClient::connect(socket).await.unwrap();
    let req = RpcRequest::new(method, params);
    client.call(&req).await.unwrap()
}

#[tokio::test]
async fn test_daemon_plan_lifecycle() {
    // 1. Create a tempdir as project directory
    let tmp = tempfile::tempdir().expect("create tempdir");
    let project_dir = tmp.path().to_path_buf();

    // 2. Init a git repo (needed for worktree support)
    let git_init = Command::new("git")
        .args(["init"])
        .current_dir(&project_dir)
        .output()
        .expect("git init");
    assert!(git_init.status.success(), "git init failed");

    let git_commit = Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(&project_dir)
        .output()
        .expect("git commit");
    assert!(git_commit.status.success(), "git commit failed: {}", String::from_utf8_lossy(&git_commit.stderr));

    // 3. Create .orca/ directory and Config
    let orca_dir = project_dir.join(".orca");
    std::fs::create_dir_all(&orca_dir).expect("create .orca dir");
    let config = Config::default();
    let socket_path = config.socket_path(&project_dir);

    // 4. Start the daemon in a background task
    let daemon = Daemon::new(config, project_dir.clone()).expect("create daemon");
    let daemon_handle = tokio::spawn(async move {
        let _ = daemon.run().await;
    });

    // Wait for the daemon to start accepting connections
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // --- Test ping ---
    let resp = fresh_call(&socket_path, "ping", json!({})).await;
    assert!(resp.error.is_none(), "ping should succeed");
    assert_eq!(resp.result.unwrap()["pong"], true);

    // --- Test submit plan ---
    let plan = json!({
        "id": "e2e-plan",
        "tasks": [
            {
                "id": "t1",
                "title": "Task 1",
                "description": "First independent task",
                "context": {"files": ["a.rs"], "references": [], "constraints": []},
                "isolation": "auto",
                "depends_on": [],
                "priority": 0
            },
            {
                "id": "t2",
                "title": "Task 2",
                "description": "Second independent task",
                "context": {"files": ["b.rs"], "references": [], "constraints": []},
                "isolation": "auto",
                "depends_on": [],
                "priority": 0
            }
        ],
        "dependencies": [],
        "created_at": "2026-01-01T00:00:00Z"
    });

    let resp = fresh_call(&socket_path, "orca_plan", plan).await;
    assert!(resp.error.is_none(), "plan should succeed: {:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["plan_id"], "e2e-plan");
    let tasks = result["tasks"].as_array().expect("tasks should be an array");
    assert_eq!(tasks.len(), 2, "should have 2 tasks");

    // --- Test status ---
    let resp = fresh_call(&socket_path, "orca_status", json!({"state": null})).await;
    assert!(resp.error.is_none(), "status should succeed");
    let result = resp.result.unwrap();
    let tasks = result["tasks"].as_array().expect("tasks array");
    assert_eq!(tasks.len(), 2, "status should show 2 tasks");

    // --- Test task detail (existing task) ---
    let resp = fresh_call(&socket_path, "orca_task_detail", json!({"task_id": "t1"})).await;
    assert!(resp.error.is_none(), "task_detail for t1 should succeed: {:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["spec"]["id"], "t1");
    assert_eq!(result["spec"]["title"], "Task 1");
    assert_eq!(result["state"], "pending");

    // --- Test task detail (nonexistent task) ---
    let resp = fresh_call(&socket_path, "orca_task_detail", json!({"task_id": "nonexistent"})).await;
    assert!(resp.result.is_none(), "nonexistent task should return error");
    assert!(resp.error.is_some(), "nonexistent task should have error");
    assert_eq!(resp.error.unwrap().code, orca::protocol::TASK_NOT_FOUND);

    // --- Cleanup ---
    daemon_handle.abort();
}
