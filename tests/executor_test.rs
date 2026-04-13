use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use orca::config::Config;
use orca::daemon::executor::TaskExecutor;
use orca::daemon::scheduler::Scheduler;
use orca::daemon::state::StateStore;
use orca::isolation::IsolationManager;
use orca::terminal::Terminal;
use orca::types::*;
use orca::worker::Worker;

// -- Mock implementations ---------------------------------------------------

struct MockWorker {
    spawned: Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
    dispatched: Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
}

impl MockWorker {
    fn new() -> Self {
        Self {
            spawned: Arc::new(tokio::sync::Mutex::new(vec![])),
            dispatched: Arc::new(tokio::sync::Mutex::new(vec![])),
        }
    }
}

#[async_trait]
impl Worker for MockWorker {
    async fn spawn(&self, worker_id: &str, work_dir: &str) -> Result<()> {
        self.spawned
            .lock()
            .await
            .push((worker_id.to_string(), work_dir.to_string()));
        Ok(())
    }

    async fn dispatch(&self, worker_id: &str, task: &TaskSpec) -> Result<()> {
        self.dispatched
            .lock()
            .await
            .push((worker_id.to_string(), task.id.clone()));
        Ok(())
    }

    async fn health_check(&self, _worker_id: &str) -> Result<WorkerStatus> {
        Ok(WorkerStatus::Dead)
    }

    async fn interrupt(&self, _worker_id: &str) -> Result<()> {
        Ok(())
    }

    async fn cleanup(&self, _worker_id: &str) -> Result<()> {
        Ok(())
    }

    async fn take_stdout(&self, _worker_id: &str) -> Result<Option<tokio::process::ChildStdout>> {
        Ok(None)
    }

    fn worker_type(&self) -> &str {
        "mock"
    }
}

struct MockTerminal;

#[async_trait]
impl Terminal for MockTerminal {
    async fn create_pane(&self, _cmd: &str, label: &str) -> Result<String> {
        Ok(label.to_string())
    }
    async fn close_pane(&self, _pane_id: &str) -> Result<()> {
        Ok(())
    }
    async fn focus_pane(&self, _pane_id: &str) -> Result<()> {
        Ok(())
    }
    fn name(&self) -> &str {
        "mock"
    }
}

// -- Helpers ----------------------------------------------------------------

fn make_spec(id: &str) -> TaskSpec {
    TaskSpec {
        id: id.to_string(),
        title: format!("Task {id}"),
        description: String::new(),
        context: TaskContext {
            files: vec!["src/main.rs".to_string()],
            references: vec![],
            constraints: String::new(),
        },
        isolation: IsolationMode::Serial,
        depends_on: vec![],
        priority: 0,
    }
}

fn make_executor(
    state: Arc<Mutex<StateStore>>,
    scheduler: Arc<Mutex<Option<Scheduler>>>,
    worker: Arc<dyn Worker>,
    project_dir: &std::path::Path,
) -> TaskExecutor {
    let worktree = project_dir.join(".agents/worktree");
    let isolation = Arc::new(IsolationManager::new(project_dir, &worktree));
    let terminal: Arc<dyn Terminal> = Arc::new(MockTerminal);
    let mut config = Config::default();
    config.daemon.max_workers = 4;

    TaskExecutor::new(
        state,
        scheduler,
        worker,
        isolation,
        terminal,
        config,
        project_dir.to_path_buf(),
    )
}

// -- Tests ------------------------------------------------------------------

#[tokio::test]
async fn test_no_tick_without_scheduler() {
    let dir = tempfile::tempdir().unwrap();
    let store = StateStore::new(dir.path()).unwrap();
    let state = Arc::new(Mutex::new(store));
    let scheduler: Arc<Mutex<Option<Scheduler>>> = Arc::new(Mutex::new(None));
    let worker: Arc<dyn Worker> = Arc::new(MockWorker::new());

    let executor = make_executor(state.clone(), scheduler, worker, dir.path());

    // tick() should succeed and do nothing when no scheduler is installed.
    executor.tick().await.unwrap();

    let store = state.lock().unwrap();
    assert!(store.state().tasks.is_empty());
    assert!(store.state().workers.is_empty());
}

#[tokio::test]
async fn test_tick_assigns_pending_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::new(dir.path()).unwrap();

    let spec = make_spec("t1");
    store.add_task(Task::new(spec.clone()));

    let state = Arc::new(Mutex::new(store));
    let sched = Scheduler::new(&[spec], &[]).unwrap();
    let scheduler = Arc::new(Mutex::new(Some(sched)));

    let mock_worker = Arc::new(MockWorker::new());
    let worker: Arc<dyn Worker> = mock_worker.clone();

    let executor = make_executor(state.clone(), scheduler, worker, dir.path());

    executor.tick().await.unwrap();

    // Allow background monitor to complete.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Worker should have been spawned and dispatched.
    let spawned = mock_worker.spawned.lock().await;
    assert_eq!(spawned.len(), 1, "should have spawned 1 worker");

    let dispatched = mock_worker.dispatched.lock().await;
    assert_eq!(dispatched.len(), 1, "should have dispatched 1 task");
    assert_eq!(dispatched[0].1, "t1");

    // The task should no longer be Pending.
    let store = state.lock().unwrap();
    let task = store.get_task("t1").unwrap();
    assert_ne!(
        task.state,
        TaskState::Pending,
        "task should have transitioned from Pending"
    );
    assert!(task.worker_id.is_some(), "task should have a worker_id");
}

#[tokio::test]
async fn test_tick_skips_when_no_pending() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::new(dir.path()).unwrap();

    let spec = make_spec("t1");
    let mut task = Task::new(spec.clone());
    // Manually advance past pending so it won't be picked up.
    task.transition_to(TaskState::Assigned).unwrap();
    task.transition_to(TaskState::Running).unwrap();
    store.add_task(task);

    let state = Arc::new(Mutex::new(store));
    let sched = Scheduler::new(&[spec], &[]).unwrap();
    let scheduler = Arc::new(Mutex::new(Some(sched)));

    let mock_worker = Arc::new(MockWorker::new());
    let worker: Arc<dyn Worker> = mock_worker.clone();

    let executor = make_executor(state.clone(), scheduler, worker, dir.path());

    executor.tick().await.unwrap();

    // No new workers should have been spawned.
    let spawned = mock_worker.spawned.lock().await;
    assert_eq!(
        spawned.len(),
        0,
        "should not spawn for already-running task"
    );
}

#[tokio::test]
async fn test_tick_respects_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::new(dir.path()).unwrap();

    let spec1 = make_spec("t1");
    let mut spec2 = make_spec("t2");
    spec2.depends_on = vec!["t1".to_string()];
    spec2.context.files = vec!["src/other.rs".to_string()];

    store.add_task(Task::new(spec1.clone()));
    store.add_task(Task::new(spec2.clone()));

    let edges = vec![Edge {
        from: "t1".to_string(),
        to: "t2".to_string(),
    }];

    let state = Arc::new(Mutex::new(store));
    let sched = Scheduler::new(&[spec1, spec2], &edges).unwrap();
    let scheduler = Arc::new(Mutex::new(Some(sched)));

    let mock_worker = Arc::new(MockWorker::new());
    let worker: Arc<dyn Worker> = mock_worker.clone();

    let executor = make_executor(state.clone(), scheduler, worker, dir.path());

    executor.tick().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Only t1 should be dispatched (t2 depends on t1).
    let dispatched = mock_worker.dispatched.lock().await;
    assert_eq!(dispatched.len(), 1);
    assert_eq!(dispatched[0].1, "t1");

    // t2 should still be pending.
    let store = state.lock().unwrap();
    assert_eq!(store.get_task("t2").unwrap().state, TaskState::Pending);
}

#[tokio::test]
async fn test_ask_cc_creates_escalation() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::new(dir.path()).unwrap();

    // Auto mode + empty files -> AskCc.
    let mut spec = make_spec("t1");
    spec.isolation = IsolationMode::Auto;
    spec.context.files = vec![];

    store.add_task(Task::new(spec.clone()));
    let state = Arc::new(Mutex::new(store));

    let sched = Scheduler::new(&[spec], &[]).unwrap();
    let scheduler = Arc::new(Mutex::new(Some(sched)));

    let mock_worker = Arc::new(MockWorker::new());
    let worker: Arc<dyn Worker> = mock_worker.clone();

    let executor = make_executor(state.clone(), scheduler, worker, dir.path());

    executor.tick().await.unwrap();

    // Worker should NOT have been spawned (we escalated instead).
    let spawned = mock_worker.spawned.lock().await;
    assert_eq!(spawned.len(), 0, "should not spawn for AskCc task");

    // Task should be blocked with an escalation.
    let store = state.lock().unwrap();
    let task = store.get_task("t1").unwrap();
    assert_eq!(task.state, TaskState::Blocked);
    assert!(task.escalation_id.is_some());

    let escalations = store.pending_escalations();
    assert_eq!(escalations.len(), 1);
    assert!(escalations[0]
        .summary
        .contains("Cannot determine isolation"));
}
