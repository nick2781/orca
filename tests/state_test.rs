use orca::daemon::state::StateStore;
use orca::types::*;
use tempfile::TempDir;

#[test]
fn test_state_store_create_and_persist() {
    let tmp = TempDir::new().unwrap();
    let orca_dir = tmp.path().join(".orca");

    // Create store, add a task, save to disk.
    {
        let mut store = StateStore::new(&orca_dir).unwrap();
        let spec = TaskSpec::new("Persist me", "This task survives a reload");
        let task = Task::new(spec.clone());
        store.add_task(task);
        store.save().unwrap();

        assert!(store.get_task(&spec.id).is_some());
    }

    // Reload from disk and verify the task is still there.
    {
        let store = StateStore::new(&orca_dir).unwrap();
        assert_eq!(store.state().tasks.len(), 1);
        let task = store.all_tasks().into_iter().next().unwrap().1;
        assert_eq!(task.spec.title, "Persist me");
        assert_eq!(task.state, TaskState::Pending);
    }
}

#[test]
fn test_ledger_append() {
    let tmp = TempDir::new().unwrap();
    let orca_dir = tmp.path().join(".orca");
    let store = StateStore::new(&orca_dir).unwrap();

    store
        .log_event("task_created", serde_json::json!({"id": "t1"}))
        .unwrap();
    store
        .log_event("task_assigned", serde_json::json!({"id": "t1", "worker": "w1"}))
        .unwrap();

    let ledger = std::fs::read_to_string(orca_dir.join("ledger.jsonl")).unwrap();
    let lines: Vec<&str> = ledger.lines().collect();
    assert_eq!(lines.len(), 2);

    // Each line should be valid JSON with the expected event field.
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["event"], "task_created");
    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["event"], "task_assigned");
}

#[test]
fn test_multiple_tasks() {
    let tmp = TempDir::new().unwrap();
    let orca_dir = tmp.path().join(".orca");
    let mut store = StateStore::new(&orca_dir).unwrap();

    for i in 0..3 {
        let spec = TaskSpec::new(format!("Task {i}"), format!("Description {i}"));
        store.add_task(Task::new(spec));
    }

    assert_eq!(store.all_tasks().len(), 3);
    assert_eq!(store.state().tasks.len(), 3);
}

#[test]
fn test_worker_registration() {
    let tmp = TempDir::new().unwrap();
    let orca_dir = tmp.path().join(".orca");
    let mut store = StateStore::new(&orca_dir).unwrap();

    let worker = WorkerInfo {
        id: "worker-1".to_string(),
        worker_type: "codex".to_string(),
        status: WorkerStatus::Idle,
        current_task_id: None,
        started_at: chrono::Utc::now(),
    };

    store.register_worker(worker);

    let retrieved = store.get_worker("worker-1").unwrap();
    assert_eq!(retrieved.id, "worker-1");
    assert_eq!(retrieved.worker_type, "codex");
    assert_eq!(retrieved.status, WorkerStatus::Idle);
    assert!(retrieved.current_task_id.is_none());

    // Verify missing worker returns None.
    assert!(store.get_worker("nonexistent").is_none());
}
