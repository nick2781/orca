use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::daemon::escalation_router::EscalationRouter;
use crate::daemon::scheduler::Scheduler;
use crate::daemon::state::StateStore;
use crate::escalation::{
    EscalationCategory, EscalationContext, EscalationOption, EscalationRequest, EscalationRoute,
};
use crate::isolation::{IsolationDecision, IsolationManager};
use crate::terminal::Terminal;
use crate::types::{TaskState, WorkerInfo, WorkerStatus};
use crate::worker::codex::parse_worker_line;
use crate::worker::{Worker, WorkerMessage};

/// Atomic counter for generating unique worker IDs.
static WORKER_COUNTER: AtomicU32 = AtomicU32::new(1);

/// Generate the next unique worker ID.
fn next_worker_id() -> String {
    format!("codex-{}", WORKER_COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Background task execution engine.
///
/// Polls the scheduler for assignable tasks, decides isolation strategy,
/// spawns worker processes, opens terminal panes, and monitors output
/// for structured markers.
pub struct TaskExecutor {
    state: Arc<Mutex<StateStore>>,
    scheduler: Arc<Mutex<Option<Scheduler>>>,
    worker: Arc<dyn Worker>,
    isolation: Arc<IsolationManager>,
    terminal: Arc<dyn Terminal>,
    config: Config,
    project_dir: PathBuf,
}

impl TaskExecutor {
    pub fn new(
        state: Arc<Mutex<StateStore>>,
        scheduler: Arc<Mutex<Option<Scheduler>>>,
        worker: Arc<dyn Worker>,
        isolation: Arc<IsolationManager>,
        terminal: Arc<dyn Terminal>,
        config: Config,
        project_dir: PathBuf,
    ) -> Self {
        Self {
            state,
            scheduler,
            worker,
            isolation,
            terminal,
            config,
            project_dir,
        }
    }

    /// Start the background execution loop.
    /// Runs indefinitely, polling every 2 seconds.
    pub async fn run(&self) {
        loop {
            if let Err(e) = self.tick().await {
                error!("executor tick error: {e:#}");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// One iteration of the execution loop.
    ///
    /// Checks for assignable tasks, decides isolation, spawns workers,
    /// opens terminal panes, and starts output monitors.
    pub async fn tick(&self) -> Result<()> {
        // Gather assignable task IDs while holding the lock briefly.
        let assignable = {
            let store = self.state.lock().unwrap();
            let sched_lock = self.scheduler.lock().unwrap();
            let sched = match sched_lock.as_ref() {
                Some(s) => s,
                None => return Ok(()), // No plan submitted yet.
            };

            let active_count = store
                .state()
                .workers
                .values()
                .filter(|w| w.status == WorkerStatus::Busy)
                .count();

            sched.assignable_tasks(
                &store.state().tasks,
                self.config.daemon.max_workers,
                active_count,
            )
        };

        if assignable.is_empty() {
            return Ok(());
        }

        for task_id in assignable {
            if let Err(e) = self.start_task(&task_id).await {
                error!("failed to start task {task_id}: {e:#}");
                // Revert the task back to pending if we partially transitioned.
                let mut store = self.state.lock().unwrap();
                if let Some(task) = store.get_task_mut(&task_id) {
                    if task.state == TaskState::Assigned {
                        // Force back to pending since we failed before running.
                        task.state = TaskState::Pending;
                        task.worker_id = None;
                    }
                }
            }
        }

        Ok(())
    }

    /// Start a single task: decide isolation, spawn worker, open pane, dispatch.
    async fn start_task(&self, task_id: &str) -> Result<()> {
        // Decide isolation strategy.
        let (decision, task_spec) = {
            let store = self.state.lock().unwrap();
            let task = store
                .get_task(task_id)
                .ok_or_else(|| anyhow::anyhow!("task not found: {task_id}"))?;

            // Collect specs of currently running tasks for overlap checks.
            let running_specs: Vec<_> = store
                .state()
                .tasks
                .values()
                .filter(|t| t.state == TaskState::Running)
                .map(|t| &t.spec)
                .collect();

            let decision = self.isolation.decide(&task.spec, &running_specs);
            (decision, task.spec.clone())
        };

        // Determine the working directory based on isolation decision.
        let work_dir = match &decision {
            IsolationDecision::Worktree { path, branch } => {
                info!(
                    task_id,
                    branch,
                    "creating worktree for task at {}",
                    path.display()
                );
                self.isolation.create_worktree(path, branch)?;
                path.clone()
            }
            IsolationDecision::SameDir => self.project_dir.clone(),
            IsolationDecision::Serial { wait_for } => {
                info!(
                    task_id,
                    wait_for, "task needs serial execution, skipping for now"
                );
                return Ok(()); // Skip -- will be picked up on a future tick.
            }
            IsolationDecision::AskCc => {
                info!(
                    task_id,
                    "isolation decision needs CC input, creating escalation"
                );
                let escalation_id = format!("esc-isolation-{task_id}");
                let mut store = self.state.lock().unwrap();
                store.add_escalation(EscalationRequest {
                    id: escalation_id.clone(),
                    task_id: task_id.to_string(),
                    worker_id: String::new(),
                    category: EscalationCategory::ImplementationChoice,
                    summary: format!(
                        "Cannot determine isolation for task '{}': no file info. \
                         Use worktree or same-dir?",
                        task_spec.title
                    ),
                    options: vec![
                        EscalationOption {
                            id: "worktree".to_string(),
                            desc: "Run in a dedicated worktree".to_string(),
                        },
                        EscalationOption {
                            id: "same_dir".to_string(),
                            desc: "Run in the project directory".to_string(),
                        },
                    ],
                    context: EscalationContext {
                        relevant_files: vec![],
                        worker_recommendation: None,
                    },
                });
                if let Some(task) = store.get_task_mut(task_id) {
                    let _ = task.transition_to(TaskState::Assigned);
                    let _ = task.transition_to(TaskState::Running);
                    let _ = task.transition_to(TaskState::Blocked);
                    task.escalation_id = Some(escalation_id);
                }
                let _ = store.save();
                return Ok(());
            }
        };

        let worker_id = next_worker_id();
        let work_dir_str = work_dir.to_string_lossy().to_string();

        // Transition: Pending -> Assigned -> Running.
        {
            let mut store = self.state.lock().unwrap();
            let task = store
                .get_task_mut(task_id)
                .ok_or_else(|| anyhow::anyhow!("task not found: {task_id}"))?;

            task.transition_to(TaskState::Assigned)
                .map_err(|e| anyhow::anyhow!(e))?;
            task.worker_id = Some(worker_id.clone());

            // Record worktree/branch info if applicable.
            if let IsolationDecision::Worktree { path, branch } = &decision {
                task.worktree_path = Some(path.to_string_lossy().to_string());
                task.branch_name = Some(branch.clone());
            }

            task.transition_to(TaskState::Running)
                .map_err(|e| anyhow::anyhow!(e))?;

            // Register the worker.
            store.register_worker(WorkerInfo {
                id: worker_id.clone(),
                worker_type: self.worker.worker_type().to_string(),
                status: WorkerStatus::Busy,
                current_task_id: Some(task_id.to_string()),
                started_at: chrono::Utc::now(),
            });

            let _ = store.save();
            let _ = store.log_event(
                "task_started",
                json!({
                    "task_id": task_id,
                    "worker_id": worker_id,
                    "work_dir": work_dir_str,
                }),
            );
        }

        info!(task_id, worker_id, work_dir = %work_dir_str, "spawning worker");

        // Spawn the worker process.
        self.worker.spawn(&worker_id, &work_dir_str).await?;

        // Open a terminal pane for visibility.
        let pane_label = format!("{} ({})", task_id, worker_id);
        match self
            .terminal
            .create_pane(&format!("tail -f /dev/null # {worker_id}"), &pane_label)
            .await
        {
            Ok(pane_id) => info!(worker_id, pane_id, "terminal pane opened"),
            Err(e) => warn!(worker_id, "failed to open terminal pane: {e:#}"),
        }

        // Dispatch the task to the worker.
        self.worker.dispatch(&worker_id, &task_spec).await?;

        // Take stdout and spawn a background monitor task.
        let stdout = self.worker.take_stdout(&worker_id).await?;
        let monitor_state = Arc::clone(&self.state);
        let monitor_worker = Arc::clone(&self.worker);
        let task_id_owned = task_id.to_string();
        let worker_id_owned = worker_id.clone();
        let escalation_config = self.config.escalation.clone();

        tokio::spawn(async move {
            monitor_worker_output(
                monitor_state,
                monitor_worker,
                worker_id_owned,
                task_id_owned,
                stdout,
                escalation_config,
            )
            .await;
        });

        Ok(())
    }
}

/// Monitor a worker's stdout in a background tokio task.
///
/// Reads lines from stdout, parses them with `parse_worker_line()`,
/// and updates task state based on structured markers. Escalations
/// are routed through the `EscalationRouter` so auto-approve
/// categories are resolved immediately without blocking the task.
async fn monitor_worker_output(
    state: Arc<Mutex<StateStore>>,
    worker: Arc<dyn Worker>,
    worker_id: String,
    task_id: String,
    stdout: Option<tokio::process::ChildStdout>,
    escalation_config: crate::config::EscalationConfig,
) {
    let router = EscalationRouter::new(escalation_config);
    if let Some(stdout) = stdout {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let msg = parse_worker_line(&line);
            match msg {
                WorkerMessage::Done(output) => {
                    info!(worker_id, task_id, "worker reported DONE");
                    let mut store = state.lock().unwrap();
                    if let Some(task) = store.get_task_mut(&task_id) {
                        task.output = Some(output);
                        let _ = task.transition_to(TaskState::Done);
                        let _ = task.transition_to(TaskState::Review);
                    }
                    if let Some(w) = store.get_worker_mut(&worker_id) {
                        w.status = WorkerStatus::Dead;
                        w.current_task_id = None;
                    }
                    let _ = store.save();
                    let _ = store.log_event(
                        "task_done",
                        json!({"task_id": task_id, "worker_id": worker_id}),
                    );
                    break;
                }
                WorkerMessage::Escalate(data) => {
                    info!(worker_id, task_id, "worker escalating");
                    let escalation_id = format!("esc-{}-{}", task_id, uuid::Uuid::new_v4());
                    let summary = data
                        .get("summary")
                        .or_else(|| data.get("reason"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("Worker escalation")
                        .to_string();

                    let escalation = EscalationRequest {
                        id: escalation_id,
                        task_id: task_id.clone(),
                        worker_id: worker_id.clone(),
                        category: EscalationCategory::ImplementationChoice,
                        summary,
                        options: vec![],
                        context: EscalationContext {
                            relevant_files: vec![],
                            worker_recommendation: data
                                .get("recommendation")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                        },
                    };

                    let route = router.route(&escalation);
                    match route {
                        EscalationRoute::AutoApprove => {
                            if let Some(decision) = router.auto_resolve(&escalation) {
                                info!(
                                    worker_id,
                                    task_id,
                                    decision = %decision.decision,
                                    "escalation auto-approved"
                                );
                                let store = state.lock().unwrap();
                                let _ = store.log_event(
                                    "escalation_auto_resolved",
                                    json!({
                                        "escalation_id": escalation.id,
                                        "task_id": task_id,
                                        "worker_id": worker_id,
                                        "decision": decision.decision,
                                    }),
                                );
                                // Task stays Running -- no block needed.
                                continue;
                            }
                            // No auto-resolution possible; fall through to store it.
                            let mut store = state.lock().unwrap();
                            if let Some(task) = store.get_task_mut(&task_id) {
                                let _ = task.transition_to(TaskState::Blocked);
                                task.escalation_id = Some(escalation.id.clone());
                            }
                            store.add_escalation(escalation);
                            if let Some(w) = store.get_worker_mut(&worker_id) {
                                w.status = WorkerStatus::Idle;
                            }
                            let _ = store.save();
                            let _ = store.log_event(
                                "task_escalated",
                                json!({"task_id": task_id, "worker_id": worker_id, "route": "auto_approve"}),
                            );
                            break;
                        }
                        EscalationRoute::CcFirst | EscalationRoute::AlwaysUser => {
                            let route_str = match route {
                                EscalationRoute::CcFirst => "cc_first",
                                EscalationRoute::AlwaysUser => "always_user",
                                _ => unreachable!(),
                            };
                            let mut store = state.lock().unwrap();
                            if let Some(task) = store.get_task_mut(&task_id) {
                                let _ = task.transition_to(TaskState::Blocked);
                                task.escalation_id = Some(escalation.id.clone());
                            }
                            store.add_escalation(escalation);
                            if let Some(w) = store.get_worker_mut(&worker_id) {
                                w.status = WorkerStatus::Idle;
                            }
                            let _ = store.save();
                            let _ = store.log_event(
                                "task_escalated",
                                json!({"task_id": task_id, "worker_id": worker_id, "route": route_str}),
                            );
                            break;
                        }
                    }
                }
                WorkerMessage::Blocked(data) => {
                    info!(worker_id, task_id, "worker blocked");
                    let reason = data
                        .get("blocker")
                        .or_else(|| data.get("reason"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("Worker blocked")
                        .to_string();
                    let escalation_id = format!("esc-{}-{}", task_id, uuid::Uuid::new_v4());

                    let mut store = state.lock().unwrap();
                    if let Some(task) = store.get_task_mut(&task_id) {
                        let _ = task.transition_to(TaskState::Blocked);
                        task.escalation_id = Some(escalation_id.clone());
                    }
                    store.add_escalation(EscalationRequest {
                        id: escalation_id,
                        task_id: task_id.clone(),
                        worker_id: worker_id.clone(),
                        category: EscalationCategory::Timeout,
                        summary: reason,
                        options: vec![],
                        context: EscalationContext {
                            relevant_files: vec![],
                            worker_recommendation: None,
                        },
                    });
                    if let Some(w) = store.get_worker_mut(&worker_id) {
                        w.status = WorkerStatus::Idle;
                    }
                    let _ = store.save();
                    let _ = store.log_event(
                        "task_blocked",
                        json!({"task_id": task_id, "worker_id": worker_id}),
                    );
                    break;
                }
                WorkerMessage::Progress(text) => {
                    info!(worker_id, task_id, progress = %text, "worker progress");
                    let store = state.lock().unwrap();
                    let _ = store.log_event(
                        "task_progress",
                        json!({"task_id": task_id, "worker_id": worker_id, "message": text}),
                    );
                }
                WorkerMessage::Output(text) => {
                    // Pass-through: log at trace level to avoid noise.
                    tracing::trace!(worker_id, task_id, output = %text, "worker output");
                }
            }
        }
    }

    // After stdout closes, check if the worker exited without reporting DONE.
    // Poll health to confirm death, then mark the task as blocked if still running.
    tokio::time::sleep(Duration::from_millis(500)).await;

    match worker.health_check(&worker_id).await {
        Ok(WorkerStatus::Dead) | Err(_) => {
            let mut store = state.lock().unwrap();
            if let Some(task) = store.get_task_mut(&task_id) {
                if task.state == TaskState::Running {
                    warn!(
                        worker_id,
                        task_id, "worker exited without DONE marker, marking task blocked"
                    );
                    let escalation_id = format!("esc-exit-{}", task_id);
                    let _ = task.transition_to(TaskState::Blocked);
                    task.escalation_id = Some(escalation_id.clone());

                    store.add_escalation(EscalationRequest {
                        id: escalation_id,
                        task_id: task_id.clone(),
                        worker_id: worker_id.clone(),
                        category: EscalationCategory::Timeout,
                        summary: "Worker exited without completing the task".to_string(),
                        options: vec![],
                        context: EscalationContext {
                            relevant_files: vec![],
                            worker_recommendation: None,
                        },
                    });
                }
            }
            if let Some(w) = store.get_worker_mut(&worker_id) {
                w.status = WorkerStatus::Dead;
                w.current_task_id = None;
            }
            let _ = store.save();
        }
        Ok(_) => {
            // Worker still alive -- unusual after stdout closes, but not fatal.
            warn!(worker_id, "worker stdout closed but process still alive");
        }
    }

    // Clean up the worker process.
    let _ = worker.cleanup(&worker_id).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{IsolationMode, Task, TaskContext, TaskSpec};

    /// Build a minimal TaskSpec.
    fn make_spec(id: &str) -> TaskSpec {
        TaskSpec {
            id: id.to_string(),
            title: format!("Task {id}"),
            description: String::new(),
            context: TaskContext {
                files: vec!["src/main.rs".to_string()],
                references: vec![],
                constraints: vec![],
            },
            isolation: IsolationMode::Worktree,
            depends_on: vec![],
            priority: 0,
        }
    }

    /// A mock worker that tracks spawn/dispatch calls without real processes.
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

    #[async_trait::async_trait]
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

        async fn take_stdout(
            &self,
            _worker_id: &str,
        ) -> Result<Option<tokio::process::ChildStdout>> {
            // No real stdout in mock -- return None so monitor exits immediately.
            Ok(None)
        }

        fn worker_type(&self) -> &str {
            "mock"
        }
    }

    /// A mock terminal that records pane creation.
    struct MockTerminal;

    #[async_trait::async_trait]
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

    /// A mock IsolationManager that always returns SameDir.
    fn make_isolation(dir: &std::path::Path) -> IsolationManager {
        let worktree = dir.join(".agents/worktree");
        IsolationManager::new(dir, &worktree)
    }

    fn make_executor(
        state: Arc<Mutex<StateStore>>,
        scheduler: Arc<Mutex<Option<Scheduler>>>,
        worker: Arc<dyn Worker>,
        project_dir: &std::path::Path,
    ) -> TaskExecutor {
        let isolation = Arc::new(make_isolation(project_dir));
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

    #[tokio::test]
    async fn test_tick_no_scheduler() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path()).unwrap();
        let state = Arc::new(Mutex::new(store));
        let scheduler = Arc::new(Mutex::new(None));
        let worker: Arc<dyn Worker> = Arc::new(MockWorker::new());

        let executor = make_executor(state.clone(), scheduler, worker, dir.path());

        // tick() should return Ok and do nothing when there's no scheduler.
        executor.tick().await.unwrap();

        let store = state.lock().unwrap();
        assert!(store.state().tasks.is_empty());
    }

    #[tokio::test]
    async fn test_tick_assigns_pending_tasks() {
        let dir = tempfile::tempdir().unwrap();

        // Initialize a git repo so worktree operations could work (though we
        // use SameDir isolation via IsolationMode::Serial with no overlap).
        let _git_init = std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output();
        let _git_commit = std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(dir.path())
            .output();

        let mut store = StateStore::new(dir.path()).unwrap();

        // Use Serial isolation mode with no file overlap so we get SameDir.
        let mut spec = make_spec("t1");
        spec.isolation = IsolationMode::Serial;
        store.add_task(Task::new(spec.clone()));

        let state = Arc::new(Mutex::new(store));

        let sched = Scheduler::new(&[spec], &[]).unwrap();
        let scheduler = Arc::new(Mutex::new(Some(sched)));

        let mock_worker = Arc::new(MockWorker::new());
        let worker: Arc<dyn Worker> = mock_worker.clone();

        let executor = make_executor(state.clone(), scheduler, worker, dir.path());

        executor.tick().await.unwrap();

        // Give the background monitor a moment to run.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // The task should have been dispatched.
        let dispatched = mock_worker.dispatched.lock().await;
        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0].1, "t1");

        // The task should no longer be Pending.
        let store = state.lock().unwrap();
        let task = store.get_task("t1").unwrap();
        // After monitor sees Dead worker without DONE, task goes to Blocked.
        // But during start_task it transitions through Assigned -> Running.
        assert!(
            task.state != TaskState::Pending,
            "task should not still be pending, got {:?}",
            task.state
        );
    }

    #[tokio::test]
    async fn test_tick_respects_max_workers() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = StateStore::new(dir.path()).unwrap();

        // Create two tasks.
        let spec1 = {
            let mut s = make_spec("t1");
            s.isolation = IsolationMode::Serial;
            s
        };
        let spec2 = {
            let mut s = make_spec("t2");
            s.isolation = IsolationMode::Serial;
            s.context.files = vec!["src/other.rs".to_string()]; // No overlap.
            s
        };

        store.add_task(Task::new(spec1.clone()));
        store.add_task(Task::new(spec2.clone()));

        // Pre-register a busy worker to consume capacity.
        store.register_worker(WorkerInfo {
            id: "existing-1".to_string(),
            worker_type: "mock".to_string(),
            status: WorkerStatus::Busy,
            current_task_id: Some("other".to_string()),
            started_at: chrono::Utc::now(),
        });

        let state = Arc::new(Mutex::new(store));

        let sched = Scheduler::new(&[spec1, spec2], &[]).unwrap();
        let scheduler = Arc::new(Mutex::new(Some(sched)));

        let mock_worker = Arc::new(MockWorker::new());
        let worker: Arc<dyn Worker> = mock_worker.clone();

        // Set max_workers to 2 (1 already busy, so only 1 slot available).
        let isolation = Arc::new(make_isolation(dir.path()));
        let terminal: Arc<dyn Terminal> = Arc::new(MockTerminal);
        let mut config = Config::default();
        config.daemon.max_workers = 2;

        let executor = TaskExecutor::new(
            state.clone(),
            scheduler,
            worker,
            isolation,
            terminal,
            config,
            dir.path().to_path_buf(),
        );

        executor.tick().await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Only one task should have been dispatched.
        let dispatched = mock_worker.dispatched.lock().await;
        assert_eq!(
            dispatched.len(),
            1,
            "should only dispatch 1 task with 1 available slot"
        );
    }

    #[tokio::test]
    async fn test_ask_cc_creates_escalation() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = StateStore::new(dir.path()).unwrap();

        // Auto isolation + empty files -> AskCc.
        let spec = {
            let mut s = make_spec("t1");
            s.isolation = IsolationMode::Auto;
            s.context.files = vec![]; // Empty -> triggers AskCc.
            s
        };

        store.add_task(Task::new(spec.clone()));
        let state = Arc::new(Mutex::new(store));

        let sched = Scheduler::new(&[spec], &[]).unwrap();
        let scheduler = Arc::new(Mutex::new(Some(sched)));

        let mock_worker = Arc::new(MockWorker::new());
        let worker: Arc<dyn Worker> = mock_worker.clone();

        let executor = make_executor(state.clone(), scheduler, worker, dir.path());

        executor.tick().await.unwrap();

        // Task should be blocked with an escalation.
        let store = state.lock().unwrap();
        let task = store.get_task("t1").unwrap();
        assert_eq!(task.state, TaskState::Blocked);
        assert!(task.escalation_id.is_some());

        // Escalation should exist.
        let escalations = store.pending_escalations();
        assert_eq!(escalations.len(), 1);
        assert!(escalations[0]
            .summary
            .contains("Cannot determine isolation"));
    }
}
