use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use serde_json::json;
use tracing::{error, info};

use crate::config::Config;
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

    /// Read the origin terminal ID from the .orca directory.
    fn origin_terminal_id(&self) -> Option<String> {
        let path = self.project_dir.join(".orca/origin_terminal_id");
        std::fs::read_to_string(&path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
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

        // Write orca task context to AGENTS.md in the worktree.
        // If the project already has an AGENTS.md (checked out from repo),
        // append orca's content so both project rules and task context are visible.
        let agents_content = generate_agents_md(&task_spec);
        let agents_path = Path::new(&work_dir_str).join("AGENTS.md");
        if agents_path.exists() {
            let existing = std::fs::read_to_string(&agents_path).unwrap_or_default();
            std::fs::write(
                &agents_path,
                format!("{}\n\n---\n\n{}", existing, agents_content),
            )?;
        } else {
            std::fs::write(&agents_path, &agents_content)?;
        }
        let _ = std::process::Command::new("git")
            .args(["add", "AGENTS.md"])
            .current_dir(&work_dir_str)
            .output();

        // Record launch time for session log discovery.
        let launch_time = chrono::Utc::now();

        // Pre-configure trust in codex config so interactive mode won't prompt.
        ensure_codex_trust(&work_dir_str);

        // Build prompt and shell command to run Codex in the terminal pane.
        let short_prompt = format!(
            "Implement: {}. {}. Read AGENTS.md for full task context and rules.",
            task_spec.title, task_spec.description
        );
        let escaped_prompt = short_prompt.replace('\'', "'\\''");
        let worker_config = self.config.codex_worker_config();
        let mut parts = vec![worker_config.command.clone()];
        if worker_config.full_auto {
            parts.push("--full-auto".to_string());
        }
        // Worktrees: allow sandbox to write main repo's .git (needed for git commit).
        if matches!(decision, IsolationDecision::Worktree { .. }) {
            let canonical = std::fs::canonicalize(&self.project_dir)
                .unwrap_or_else(|_| self.project_dir.clone());
            parts.push(format!("--add-dir={}", canonical.display()));
        }
        parts.extend(worker_config.args.iter().cloned());
        let cmd = format!(
            "cd '{}' && {} '{}'",
            work_dir_str,
            parts.join(" "),
            escaped_prompt
        );

        // Launch Codex in a user-visible terminal pane.
        let pane_label = format!("{} ({})", task_id, worker_id);
        let pane_id = match self.terminal.create_pane(&cmd, &pane_label).await {
            Ok(id) => {
                info!(worker_id, pane_id = %id, "worker launched in terminal pane");
                Some(id)
            }
            Err(e) => {
                error!(worker_id, "failed to open terminal pane: {e:#}");
                eprintln!("Run manually: {}", cmd);
                None
            }
        };

        // Spawn background monitor that reads codex session logs.
        let monitor_state = Arc::clone(&self.state);
        let monitor_terminal = Arc::clone(&self.terminal);
        let task_id_owned = task_id.to_string();
        let worker_id_owned = worker_id.clone();
        let work_dir_path = PathBuf::from(&work_dir_str);
        let timeout = self.config.codex_worker_config().timeout_secs;
        let origin_id = self.origin_terminal_id();

        let decision_clone = decision.clone();
        tokio::spawn(async move {
            monitor_task_completion(MonitorParams {
                state: monitor_state,
                terminal: monitor_terminal,
                task_id: task_id_owned,
                worker_id: worker_id_owned,
                work_dir: work_dir_path,
                launch_time,
                timeout_secs: timeout,
                pane_id,
                origin_terminal_id: origin_id,
                decision: decision_clone,
            })
            .await;
        });

        Ok(())
    }
}

/// Write a project-local `.codex/config.toml` in the worktree directory
/// so codex trusts it without modifying global config.
fn ensure_codex_trust(work_dir: &str) {
    let codex_dir = Path::new(work_dir).join(".codex");
    let _ = std::fs::create_dir_all(&codex_dir);
    let config_path = codex_dir.join("config.toml");
    if config_path.exists() {
        return;
    }
    let _ = std::fs::write(&config_path, "trust_level = \"trusted\"\n");
    info!(work_dir, "wrote project-local codex trust config");
}

// -- Codex session log helpers for completion detection --

/// Find the codex session log file matching a worker's working directory.
///
/// Codex writes session logs to `~/.codex/sessions/<Y>/<M>/<D>/<session>.jsonl`.
/// Each session starts with a `session_meta` event containing the `cwd`.
/// We scan recent files created after `after` to find the matching session.
fn find_session_log(work_dir: &Path, after: chrono::DateTime<chrono::Utc>) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let sessions_dir = home.join(".codex/sessions");
    let date = after.format("%Y/%m/%d").to_string();
    let day_dir = sessions_dir.join(&date);

    if !day_dir.exists() {
        return None;
    }

    let work_dir_str = work_dir.to_string_lossy();
    // Also match the /private prefix macOS adds to /tmp paths.
    let work_dir_private = format!("/private{}", work_dir_str);

    let mut entries: Vec<_> = std::fs::read_dir(&day_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
        .collect();

    // Sort by modification time descending (newest first).
    entries.sort_by(|a, b| {
        b.metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            .cmp(
                &a.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            )
    });

    for entry in entries {
        let path = entry.path();
        // Read just the first line (session_meta) to check cwd and timestamp.
        if let Ok(first_line) = read_first_line(&path) {
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&first_line) {
                if obj.get("type").and_then(|t| t.as_str()) == Some("session_meta") {
                    // Check timestamp is after launch_time.
                    if let Some(ts) = obj.get("timestamp").and_then(|t| t.as_str()) {
                        if let Ok(session_time) = chrono::DateTime::parse_from_rfc3339(ts) {
                            if session_time < after {
                                continue; // Session is older than the worker launch.
                            }
                        }
                    }
                    if let Some(cwd) = obj
                        .get("payload")
                        .and_then(|p| p.get("cwd"))
                        .and_then(|c| c.as_str())
                    {
                        if cwd == work_dir_str || cwd == work_dir_private {
                            return Some(path);
                        }
                    }
                }
            }
        }
    }

    None
}

/// Read the first line of a file.
fn read_first_line(path: &Path) -> Result<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(line)
}

/// Outcome from parsing codex session log events.
enum SessionEvent {
    /// Task completed successfully with output.
    Done(TaskOutput),
    /// Worker is escalating a decision.
    Escalate(String),
    /// Worker is blocked.
    Blocked(String),
    /// Progress update.
    Progress(String),
    /// Codex session ended (task_complete event).
    SessionComplete(String),
    /// Codex is waiting for user approval (sandbox prompt).
    ApprovalNeeded(String),
}

/// Read new lines from a session log file starting at `offset`.
/// Returns parsed events and the new file offset.
fn read_session_events(path: &Path, offset: u64) -> (Vec<SessionEvent>, u64) {
    use std::io::{BufRead, Seek, SeekFrom};

    let mut events = Vec::new();
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return (events, offset),
    };
    let mut reader = std::io::BufReader::new(file);
    if reader.seek(SeekFrom::Start(offset)).is_err() {
        return (events, offset);
    }

    let mut new_offset = offset;
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        new_offset = reader.stream_position().unwrap_or(new_offset);
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(line.trim()) {
            let event_type = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if event_type == "event_msg" {
                if let Some(payload) = obj.get("payload") {
                    let pt = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match pt {
                        "task_complete" => {
                            let msg = payload
                                .get("last_agent_message")
                                .and_then(|m| m.as_str())
                                .unwrap_or("")
                                .to_string();
                            events.push(SessionEvent::SessionComplete(msg));
                        }
                        "agent_message" => {
                            if let Some(msg) = payload.get("message").and_then(|m| m.as_str()) {
                                // Detect approval requests before marker parsing.
                                if msg.contains("requesting approval")
                                    || msg.contains("requesting elevated")
                                    || msg.contains("blocked by the sandbox")
                                {
                                    events.push(SessionEvent::ApprovalNeeded(msg.to_string()));
                                    line.clear();
                                    continue;
                                }
                                let parsed = crate::worker::codex::parse_worker_line(msg);
                                match parsed {
                                    crate::worker::WorkerMessage::Done(output) => {
                                        events.push(SessionEvent::Done(output));
                                    }
                                    crate::worker::WorkerMessage::Escalate(v) => {
                                        events.push(SessionEvent::Escalate(v.to_string()));
                                    }
                                    crate::worker::WorkerMessage::Blocked(v) => {
                                        events.push(SessionEvent::Blocked(v.to_string()));
                                    }
                                    crate::worker::WorkerMessage::Progress(s) => {
                                        events.push(SessionEvent::Progress(s));
                                    }
                                    crate::worker::WorkerMessage::Output(_) => {}
                                }
                            }
                        }
                        "turn_aborted" => {
                            events
                                .push(SessionEvent::Blocked("codex turn was aborted".to_string()));
                        }
                        _ => {}
                    }
                }
            }
        }
        line.clear();
    }

    (events, new_offset)
}

/// Check whether a codex session is still active by checking if the
/// session log file was modified recently (within the last 30 seconds).
/// Falls back to pgrep if no session log is available.
fn is_codex_active(session_log: &Option<PathBuf>, work_dir: &Path) -> bool {
    // Primary: check if session log was recently modified.
    if let Some(ref log_path) = session_log {
        if let Ok(meta) = std::fs::metadata(log_path) {
            if let Ok(modified) = meta.modified() {
                let age = modified.elapsed().unwrap_or(Duration::from_secs(999));
                return age < Duration::from_secs(30);
            }
        }
    }
    // Fallback: pgrep for codex processes with the work dir.
    let pattern = format!("codex.*{}", work_dir.display());
    std::process::Command::new("pgrep")
        .args(["-f", &pattern])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Gracefully exit codex in a terminal pane and close the pane.
async fn graceful_exit_codex(terminal: &Arc<dyn Terminal>, pane_id: &str) {
    let _ = terminal.send_text(pane_id, "/exit\n").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let _ = terminal.close_pane(pane_id).await;
    info!(pane_id, "sent /exit to codex and closed pane");
}

/// Notify the main agent terminal about an escalation.
async fn notify_escalation(
    terminal: &Arc<dyn Terminal>,
    origin_id: &Option<String>,
    task_id: &str,
    summary: &str,
) {
    // Focus the origin terminal (CC's pane).
    if let Some(ref id) = origin_id {
        let _ = terminal.focus_pane(id).await;
    }
    // Send macOS notification.
    let _ = tokio::process::Command::new("osascript")
        .args([
            "-e",
            &format!(
                "display notification \"{}\" with title \"Orca Escalation\" subtitle \"{}\"",
                summary.replace('"', "'"),
                task_id
            ),
        ])
        .output()
        .await;
}

/// Monitor task completion by reading codex session logs.
///
/// Finds the session log file matching the worker's working directory,
/// then tails it for completion/escalation events. Falls back to
/// process exit detection if no session log is found.
struct MonitorParams {
    state: Arc<Mutex<StateStore>>,
    terminal: Arc<dyn Terminal>,
    task_id: String,
    worker_id: String,
    work_dir: PathBuf,
    launch_time: chrono::DateTime<chrono::Utc>,
    timeout_secs: u64,
    pane_id: Option<String>,
    origin_terminal_id: Option<String>,
    decision: IsolationDecision,
}

async fn monitor_task_completion(p: MonitorParams) {
    let MonitorParams {
        state,
        terminal,
        task_id,
        worker_id,
        work_dir,
        launch_time,
        timeout_secs,
        pane_id,
        origin_terminal_id,
        decision,
    } = p;
    let start = std::time::Instant::now();

    // Wait for codex to initialize and create its session log.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let mut session_log: Option<PathBuf> = None;
    let mut log_offset: u64 = 0;

    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;

        // Try to find the session log if we haven't yet.
        if session_log.is_none() {
            session_log = find_session_log(&work_dir, launch_time);
            if let Some(ref path) = session_log {
                info!(task_id, log = %path.display(), "found codex session log");
            } else if start.elapsed().as_secs() < 30 {
                // Session log not yet available — codex may still be starting.
                continue;
            }
        }

        // Read new events from the session log.
        if let Some(ref log_path) = session_log {
            let (events, new_offset) = read_session_events(log_path, log_offset);
            log_offset = new_offset;

            for event in &events {
                match event {
                    SessionEvent::Done(output) => {
                        info!(task_id, worker_id, "codex reported done via log");
                        {
                            let mut store = state.lock().unwrap();
                            if let Some(task) = store.get_task_mut(&task_id) {
                                task.output = Some(output.clone());
                                let _ = task.transition_to(TaskState::Done);
                                let _ = task.transition_to(TaskState::Review);
                            }
                            mark_worker_dead(&mut store, &worker_id);
                            let _ = store.save();
                            let _ = store.log_event(
                                "task_completed",
                                json!({
                                    "task_id": task_id,
                                    "worker_id": worker_id,
                                    "source": "session_log",
                                }),
                            );
                        }
                        notify_escalation(
                            &terminal,
                            &origin_terminal_id,
                            &task_id,
                            "Task completed, ready for review",
                        )
                        .await;
                        if let Some(ref pid) = pane_id {
                            graceful_exit_codex(&terminal, pid).await;
                        }
                        return;
                    }
                    SessionEvent::SessionComplete(msg) => {
                        info!(task_id, worker_id, "codex session complete");
                        {
                            let mut store = state.lock().unwrap();
                            if let Some(task) = store.get_task_mut(&task_id) {
                                // Parse the last message for output if possible.
                                let output = parse_done_from_message(msg);
                                task.output = Some(output);
                                let _ = task.transition_to(TaskState::Done);
                                let _ = task.transition_to(TaskState::Review);
                            }
                            mark_worker_dead(&mut store, &worker_id);
                            let _ = store.save();
                            let _ = store.log_event(
                                "task_completed",
                                json!({
                                    "task_id": task_id,
                                    "worker_id": worker_id,
                                    "source": "session_complete",
                                }),
                            );
                        }
                        notify_escalation(
                            &terminal,
                            &origin_terminal_id,
                            &task_id,
                            "Task completed, ready for review",
                        )
                        .await;
                        if let Some(ref pid) = pane_id {
                            graceful_exit_codex(&terminal, pid).await;
                        }
                        return;
                    }
                    SessionEvent::Escalate(summary) => {
                        info!(task_id, worker_id, "codex escalation via log");
                        let escalation_id = format!("esc-worker-{}", task_id);
                        {
                            let mut store = state.lock().unwrap();
                            if let Some(task) = store.get_task_mut(&task_id) {
                                let _ = task.transition_to(TaskState::Blocked);
                                task.escalation_id = Some(escalation_id.clone());
                            }
                            store.add_escalation(EscalationRequest {
                                id: escalation_id,
                                task_id: task_id.clone(),
                                worker_id: worker_id.clone(),
                                category: EscalationCategory::ImplementationChoice,
                                summary: summary.clone(),
                                options: vec![],
                                context: EscalationContext::default(),
                            });
                            let _ = store.save();
                        }
                        notify_escalation(&terminal, &origin_terminal_id, &task_id, summary).await;
                        return;
                    }
                    SessionEvent::Blocked(reason) => {
                        info!(task_id, worker_id, "codex blocked via log");
                        let escalation_id = format!("esc-blocked-{}", task_id);
                        {
                            let mut store = state.lock().unwrap();
                            if let Some(task) = store.get_task_mut(&task_id) {
                                let _ = task.transition_to(TaskState::Blocked);
                                task.escalation_id = Some(escalation_id.clone());
                            }
                            store.add_escalation(EscalationRequest {
                                id: escalation_id,
                                task_id: task_id.clone(),
                                worker_id: worker_id.clone(),
                                category: EscalationCategory::Conflict,
                                summary: reason.clone(),
                                options: vec![],
                                context: EscalationContext::default(),
                            });
                            let _ = store.save();
                        }
                        notify_escalation(&terminal, &origin_terminal_id, &task_id, reason).await;
                        return;
                    }
                    SessionEvent::Progress(msg) => {
                        info!(task_id, worker_id, progress = %msg, "codex progress");
                    }
                    SessionEvent::ApprovalNeeded(msg) => {
                        // Route through escalation system:
                        // - Worktree tasks: auto-approve (worktree isolation = safe)
                        // - Same-dir tasks: escalate to CC
                        let is_worktree =
                            matches!(decision, IsolationDecision::Worktree { .. });
                        if is_worktree {
                            info!(task_id, worker_id, "auto-approving (worktree isolated)");
                            if let Some(ref pid) = pane_id {
                                let _ = terminal.send_text(pid, "y").await;
                            }
                            let _ = state.lock().unwrap().log_event(
                                "approval_auto",
                                json!({
                                    "task_id": task_id,
                                    "worker_id": worker_id,
                                    "reason": "worktree_isolated",
                                }),
                            );
                        } else {
                            info!(task_id, worker_id, "sandbox approval needed, escalating");
                            let esc_id = format!("esc-approval-{}", task_id);
                            {
                                let mut store = state.lock().unwrap();
                                store.add_escalation(EscalationRequest {
                                    id: esc_id.clone(),
                                    task_id: task_id.clone(),
                                    worker_id: worker_id.clone(),
                                    category: EscalationCategory::ImplementationChoice,
                                    summary: format!(
                                        "Codex needs sandbox approval: {}",
                                        msg.chars().take(200).collect::<String>()
                                    ),
                                    options: vec![
                                        EscalationOption {
                                            id: "approve".into(),
                                            desc: "Approve the operation".into(),
                                        },
                                        EscalationOption {
                                            id: "reject".into(),
                                            desc: "Reject and skip".into(),
                                        },
                                    ],
                                    context: EscalationContext::default(),
                                });
                                let _ = store.save();
                            }
                            notify_escalation(
                                &terminal,
                                &origin_terminal_id,
                                &task_id,
                                "Codex needs sandbox approval",
                            )
                            .await;
                            // Don't return — keep monitoring. CC will decide
                            // via orca_decide, and the next monitor tick will
                            // check if the escalation was resolved.
                        }
                    }
                }
            }
        }

        // Fallback: codex exited without session log events.
        if !is_codex_active(&session_log, &work_dir) {
            // Grace period: wait for session log to flush before giving up.
            tokio::time::sleep(Duration::from_secs(2)).await;

            // Try to find the session log one more time if we haven't yet.
            if session_log.is_none() {
                session_log = find_session_log(&work_dir, launch_time);
            }

            // Final read of session log.
            if let Some(ref log_path) = session_log {
                let (events, _) = read_session_events(log_path, log_offset);
                for event in &events {
                    if matches!(
                        event,
                        SessionEvent::Done(_) | SessionEvent::SessionComplete(_)
                    ) {
                        let output = match event {
                            SessionEvent::Done(o) => o.clone(),
                            SessionEvent::SessionComplete(msg) => parse_done_from_message(msg),
                            _ => unreachable!(),
                        };
                        info!(task_id, worker_id, "codex completed (final log read)");
                        {
                            let mut store = state.lock().unwrap();
                            if let Some(task) = store.get_task_mut(&task_id) {
                                task.output = Some(output);
                                let _ = task.transition_to(TaskState::Done);
                                let _ = task.transition_to(TaskState::Review);
                            }
                            mark_worker_dead(&mut store, &worker_id);
                            let _ = store.save();
                        }
                        notify_escalation(
                            &terminal,
                            &origin_terminal_id,
                            &task_id,
                            "Task completed, ready for review",
                        )
                        .await;
                        return;
                    }
                }
            }

            info!(task_id, worker_id, "codex exited without completion signal");
            let escalation_id = format!("esc-exit-{}", task_id);
            {
                let mut store = state.lock().unwrap();
                if let Some(task) = store.get_task_mut(&task_id) {
                    let _ = task.transition_to(TaskState::Blocked);
                    task.escalation_id = Some(escalation_id.clone());
                }
                mark_worker_dead(&mut store, &worker_id);
                store.add_escalation(EscalationRequest {
                    id: escalation_id,
                    task_id: task_id.clone(),
                    worker_id: worker_id.clone(),
                    category: EscalationCategory::Timeout,
                    summary: "Codex exited without completion signal".to_string(),
                    options: vec![],
                    context: EscalationContext::default(),
                });
                let _ = store.save();
                let _ = store.log_event(
                    "task_blocked",
                    json!({
                        "task_id": task_id,
                        "worker_id": worker_id,
                        "reason": "exit_no_signal",
                    }),
                );
            }
            notify_escalation(
                &terminal,
                &origin_terminal_id,
                &task_id,
                "Codex exited without completion",
            )
            .await;
            return;
        }

        // Timeout check.
        if start.elapsed().as_secs() > timeout_secs {
            info!(task_id, worker_id, "task timed out after {}s", timeout_secs);
            let escalation_id = format!("esc-timeout-{}", task_id);
            {
                let mut store = state.lock().unwrap();
                if let Some(task) = store.get_task_mut(&task_id) {
                    let _ = task.transition_to(TaskState::Blocked);
                    task.escalation_id = Some(escalation_id.clone());
                }
                mark_worker_dead(&mut store, &worker_id);
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
            }
            notify_escalation(
                &terminal,
                &origin_terminal_id,
                &task_id,
                &format!("Task timed out after {}s", timeout_secs),
            )
            .await;
            if let Some(ref pid) = pane_id {
                graceful_exit_codex(&terminal, pid).await;
            }
            return;
        }
    }
}

/// Mark a worker as dead and unassign its task.
fn mark_worker_dead(store: &mut StateStore, worker_id: &str) {
    if let Some(w) = store.get_worker_mut(worker_id) {
        w.status = WorkerStatus::Dead;
        w.current_task_id = None;
    }
}

/// Try to parse a `[ORCA:DONE]` marker from a message string.
/// Falls back to an empty TaskOutput if no marker is found.
fn parse_done_from_message(msg: &str) -> TaskOutput {
    let parsed = crate::worker::codex::parse_worker_line(msg);
    match parsed {
        crate::worker::WorkerMessage::Done(output) => output,
        _ => TaskOutput {
            files_changed: vec![],
            tests_passed: false,
            diff_summary: String::new(),
            stdout: msg.to_string(),
        },
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
        async fn send_text(&self, _pane_id: &str, _text: &str) -> Result<()> {
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
