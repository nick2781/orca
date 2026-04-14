use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use serde_json::json;
use tracing::{error, info};

use crate::config::{Config, EscalationConfig};
use crate::daemon::scheduler::Scheduler;
use crate::daemon::state::StateStore;
use crate::escalation::{
    EscalationCategory, EscalationContext, EscalationOption, EscalationRequest,
};
use crate::isolation::{IsolationDecision, IsolationManager};
use crate::terminal::Terminal;
use crate::types::{TaskOutput, TaskState, WorkerInfo, WorkerStatus};
use crate::worker::codex::generate_agents_md;
use crate::worker::Worker;

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

    /// Start a single task: decide isolation, write context, launch in terminal pane.
    ///
    /// Instead of spawning a hidden subprocess and piping stdout, this launches
    /// Codex directly in a user-visible terminal pane. Completion is detected
    /// by a background monitor that polls git state for new commits.
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

        info!(task_id, worker_id, work_dir = %work_dir_str, "launching worker in terminal pane");

        // Write AGENTS.md to working directory so Codex has full task context.
        // Then git-add it so it doesn't show up as untracked in completion detection.
        let agents_content = generate_agents_md(&task_spec);
        let agents_path = Path::new(&work_dir_str).join("AGENTS.md");
        std::fs::write(&agents_path, &agents_content)?;
        let _ = std::process::Command::new("git")
            .args(["add", "AGENTS.md"])
            .current_dir(&work_dir_str)
            .output();

        // Record initial HEAD for completion detection.
        let initial_head = get_git_head(&work_dir_str).unwrap_or_default();

        // Build prompt and shell command to run Codex in the terminal pane.
        let short_prompt = format!(
            "Implement: {}. {}. Read AGENTS.md for full task context and rules.",
            task_spec.title, task_spec.description
        );
        // Escape single quotes for shell safety.
        let escaped_prompt = short_prompt.replace('\'', "'\\''");
        // Build shell command from config: codex [flags...] [args...] '<prompt>'
        let worker_config = self.config.codex_worker_config();
        let mut flags = Vec::new();
        if worker_config.full_auto {
            flags.push("--full-auto".to_string());
        }
        // Auto-trust the worktree directory via -c flag (no global config modification).
        flags.push(format!(
            "-c projects.\"{}\".trust_level=\"trusted\"",
            work_dir_str.replace('"', "\\\"")
        ));
        flags.extend(worker_config.args.iter().cloned());
        let flags_str = if flags.is_empty() {
            String::new()
        } else {
            format!(" {}", flags.join(" "))
        };
        let cmd = format!(
            "cd '{}' && {}{} '{}'",
            work_dir_str, worker_config.command, flags_str, escaped_prompt
        );

        // Launch Codex in a user-visible terminal pane.
        let pane_label = format!("{} ({})", task_id, worker_id);
        match self.terminal.create_pane(&cmd, &pane_label).await {
            Ok(pane_id) => info!(worker_id, pane_id, "worker launched in terminal pane"),
            Err(e) => {
                error!(worker_id, "failed to open terminal pane: {e:#}");
                // Print command for manual execution as fallback.
                eprintln!("Run manually: {}", cmd);
            }
        }

        // Spawn background monitor that polls git state for completion.
        let monitor_state = Arc::clone(&self.state);
        let task_id_owned = task_id.to_string();
        let worker_id_owned = worker_id.clone();
        let work_dir_path = PathBuf::from(&work_dir_str);
        let esc_config = self.config.escalation.clone();
        let timeout = self.config.codex_worker_config().timeout_secs;

        tokio::spawn(async move {
            monitor_task_completion(
                monitor_state,
                task_id_owned,
                worker_id_owned,
                work_dir_path,
                initial_head,
                timeout,
                esc_config,
            )
            .await;
        });

        Ok(())
    }
}

// -- Git state helpers for completion detection --

/// Get the current HEAD commit hash for a git working directory.
fn get_git_head(work_dir: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(work_dir)
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Get `git diff --stat` output for a working directory.
fn get_git_diff_stat(work_dir: &Path) -> String {
    std::process::Command::new("git")
        .args(["diff", "--stat"])
        .current_dir(work_dir)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}



/// Check whether a codex process is still running in the given directory.
///
/// Uses `pgrep -f` to search for a codex process whose command line
/// references the working directory.
fn is_codex_running(work_dir: &Path) -> bool {
    let pattern = format!("codex.*{}", work_dir.display());
    std::process::Command::new("pgrep")
        .args(["-f", &pattern])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Monitor task completion by polling git state instead of reading stdout.
///
/// Checks periodically whether the codex process has exited and whether
/// it produced git changes (new commits or uncommitted work). Transitions
/// the task to Done/Review on success, or Blocked on failure/timeout.
async fn monitor_task_completion(
    state: Arc<Mutex<StateStore>>,
    task_id: String,
    worker_id: String,
    work_dir: PathBuf,
    initial_head: String,
    timeout_secs: u64,
    _esc_config: EscalationConfig,
) {
    let start = std::time::Instant::now();

    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;

        let work_dir_str = work_dir.to_str().unwrap_or(".");
        let current_head = get_git_head(work_dir_str).unwrap_or_default();
        let has_new_commits = !current_head.is_empty() && current_head != initial_head;
        let codex_running = is_codex_running(&work_dir);

        // Case 1: New commits detected — task completed (regardless of process state).
        // Codex in --full-auto may not exit after finishing, so we don't wait for exit.
        if has_new_commits {
            let diff = get_git_diff_stat(&work_dir);
            info!(task_id, worker_id, "codex produced commits — task complete");

            let mut store = state.lock().unwrap();
            if let Some(task) = store.get_task_mut(&task_id) {
                task.output = Some(TaskOutput {
                    files_changed: vec![],
                    tests_passed: false,
                    diff_summary: diff,
                    stdout: String::new(),
                });
                let _ = task.transition_to(TaskState::Done);
                let _ = task.transition_to(TaskState::Review);
            }
            if let Some(w) = store.get_worker_mut(&worker_id) {
                w.status = WorkerStatus::Dead;
                w.current_task_id = None;
            }
            let _ = store.save();
            let _ = store.log_event(
                "task_completed",
                json!({ "task_id": task_id, "worker_id": worker_id }),
            );
            break;
        }

        // Case 2: Codex exited without producing commits.
        if !codex_running {
            info!(task_id, worker_id, "codex exited without commits");

            let escalation_id = format!("esc-exit-{}", task_id);
            let mut store = state.lock().unwrap();
            if let Some(task) = store.get_task_mut(&task_id) {
                let _ = task.transition_to(TaskState::Blocked);
                task.escalation_id = Some(escalation_id.clone());
            }
            if let Some(w) = store.get_worker_mut(&worker_id) {
                w.status = WorkerStatus::Dead;
                w.current_task_id = None;
            }
            store.add_escalation(EscalationRequest {
                id: escalation_id,
                task_id: task_id.clone(),
                worker_id: worker_id.clone(),
                category: EscalationCategory::Timeout,
                summary: "Codex exited without producing any changes".to_string(),
                options: vec![],
                context: EscalationContext::default(),
            });
            let _ = store.save();
            let _ = store.log_event(
                "task_blocked",
                json!({ "task_id": task_id, "worker_id": worker_id, "reason": "no_changes" }),
            );
            break;
        }

        // Case 3: Timeout.
        if start.elapsed().as_secs() > timeout_secs {
            info!(task_id, worker_id, "task timed out after {}s", timeout_secs);

            let escalation_id = format!("esc-timeout-{}", task_id);
            let mut store = state.lock().unwrap();
            if let Some(task) = store.get_task_mut(&task_id) {
                let _ = task.transition_to(TaskState::Blocked);
                task.escalation_id = Some(escalation_id.clone());
            }
            if let Some(w) = store.get_worker_mut(&worker_id) {
                w.status = WorkerStatus::Dead;
                w.current_task_id = None;
            }
            store.add_escalation(EscalationRequest {
                id: escalation_id,
                task_id: task_id.clone(),
                worker_id: worker_id.clone(),
                category: EscalationCategory::Timeout,
                summary: format!("Task exceeded timeout of {}s", timeout_secs),
                options: vec![],
                context: EscalationContext::default(),
            });
            let _ = store.save();
            let _ = store.log_event(
                "task_timeout",
                json!({ "task_id": task_id, "worker_id": worker_id }),
            );
            break;
        }
    }
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
                constraints: String::new(),
            },
            isolation: IsolationMode::Worktree,
            depends_on: vec![],
            priority: 0,
        }
    }

    /// A mock worker used by scheduling tests.
    ///
    /// The executor no longer calls spawn/dispatch/take_stdout on the worker
    /// (those methods belong to the old piped-subprocess model). This mock
    /// exists to satisfy the `Worker` trait bound required by `TaskExecutor`.
    struct MockWorker;

    impl MockWorker {
        fn new() -> Self {
            Self
        }
    }

    #[async_trait::async_trait]
    impl Worker for MockWorker {
        async fn spawn(&self, _worker_id: &str, _work_dir: &str) -> Result<()> {
            Ok(())
        }

        async fn dispatch(&self, _worker_id: &str, _task: &TaskSpec) -> Result<()> {
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

        let worker: Arc<dyn Worker> = Arc::new(MockWorker::new());

        let executor = make_executor(state.clone(), scheduler, worker, dir.path());

        executor.tick().await.unwrap();

        // Give the background monitor a moment to run.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // The task should have transitioned out of Pending.
        // With the terminal-pane model, start_task writes AGENTS.md, opens a
        // pane, and spawns a git-polling monitor. The task transitions through
        // Assigned -> Running during start_task.
        let store = state.lock().unwrap();
        let task = store.get_task("t1").unwrap();
        assert!(
            task.state != TaskState::Pending,
            "task should not still be pending, got {:?}",
            task.state
        );
        // Verify a worker was registered.
        assert!(
            !store.state().workers.is_empty(),
            "a worker should have been registered"
        );
    }

    #[tokio::test]
    async fn test_tick_respects_max_workers() {
        let dir = tempfile::tempdir().unwrap();

        // Initialize a git repo.
        let _git_init = std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output();
        let _git_commit = std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(dir.path())
            .output();

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

        let worker: Arc<dyn Worker> = Arc::new(MockWorker::new());

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

        // Only one task should have been started (1 available slot).
        // Count tasks that moved past Pending.
        let store = state.lock().unwrap();
        let started_count = store
            .state()
            .tasks
            .values()
            .filter(|t| t.state != TaskState::Pending)
            .count();
        assert_eq!(
            started_count, 1,
            "should only start 1 task with 1 available slot"
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

        let worker: Arc<dyn Worker> = Arc::new(MockWorker::new());

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
