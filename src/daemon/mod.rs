pub mod escalation_router;
pub mod executor;
pub mod scheduler;
pub mod server;
pub mod state;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use serde_json::{json, Value};
use tracing::info;

use crate::config::Config;
use crate::daemon::escalation_router::EscalationRouter;
use crate::daemon::executor::TaskExecutor;
use crate::daemon::scheduler::Scheduler;
use crate::daemon::server::{IpcServer, RpcHandler};
use crate::daemon::state::StateStore;
use crate::isolation::IsolationManager;
use crate::protocol::{
    RpcError, RpcRequest, RpcResponse, ESCALATION_NOT_FOUND, INTERNAL_ERROR, INVALID_PARAMS,
    INVALID_STATE_TRANSITION, METHOD_NOT_FOUND, TASK_NOT_FOUND,
};
use crate::terminal;
use crate::types::{Plan, Task, TaskState};
use crate::worker::codex::CodexWorker;
use crate::worker::Worker;

// -- PID file management -----------------------------------------------------

const PID_FILE_NAME: &str = "orca.pid";

/// Check if a daemon is already running by reading the PID file and probing the process.
/// Returns an error if another daemon is alive.
pub fn check_existing_daemon(orca_dir: &Path) -> Result<()> {
    let pid_path = orca_dir.join(PID_FILE_NAME);
    if !pid_path.exists() {
        return Ok(());
    }

    let contents = std::fs::read_to_string(&pid_path).unwrap_or_default();
    let pid: u32 = match contents.trim().parse() {
        Ok(p) => p,
        Err(_) => {
            // Corrupt PID file — remove it and allow startup.
            let _ = std::fs::remove_file(&pid_path);
            return Ok(());
        }
    };

    if is_process_alive(pid) {
        bail!("daemon already running (pid: {pid})");
    }

    // Stale PID file — the process is gone, clean up.
    let _ = std::fs::remove_file(&pid_path);
    Ok(())
}

/// Write the current process PID to `.orca/orca.pid`.
pub fn write_pid_file(orca_dir: &Path) -> Result<()> {
    let pid_path = orca_dir.join(PID_FILE_NAME);
    std::fs::create_dir_all(orca_dir)?;
    std::fs::write(&pid_path, std::process::id().to_string())?;
    Ok(())
}

/// Remove the PID file. Best-effort — errors are silently ignored.
pub fn remove_pid_file(orca_dir: &Path) {
    let pid_path = orca_dir.join(PID_FILE_NAME);
    let _ = std::fs::remove_file(&pid_path);
}

/// Read the PID from the PID file, if it exists and is valid.
pub fn read_pid_file(orca_dir: &Path) -> Option<u32> {
    let pid_path = orca_dir.join(PID_FILE_NAME);
    let contents = std::fs::read_to_string(&pid_path).ok()?;
    contents.trim().parse().ok()
}

/// Check whether a process with the given PID is alive.
fn is_process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// RAII guard that removes the PID file and socket on drop.
struct DaemonGuard {
    orca_dir: PathBuf,
    socket_path: PathBuf,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        remove_pid_file(&self.orca_dir);
        let _ = std::fs::remove_file(&self.socket_path);
        info!("daemon cleanup complete");
    }
}

// -- Daemon ------------------------------------------------------------------

/// The Orca daemon: manages state, scheduling, workers, and handles RPC requests.
pub struct Daemon {
    pub config: Config,
    pub project_dir: PathBuf,
    pub state: Arc<Mutex<StateStore>>,
    pub scheduler: Arc<Mutex<Option<Scheduler>>>,
    pub isolation: Arc<IsolationManager>,
    pub worker: Arc<dyn Worker>,
}

impl Daemon {
    /// Create a new Daemon from config and project directory.
    pub fn new(config: Config, project_dir: PathBuf) -> Result<Self> {
        let orca_dir = project_dir.join(".orca");
        let state_store = StateStore::new(&orca_dir)?;
        let worktree_dir = config.worktree_dir(&project_dir);
        let isolation = IsolationManager::new(&project_dir, &worktree_dir);
        let codex_config = config.codex_worker_config();
        let worker = CodexWorker::new(codex_config);

        Ok(Self {
            config,
            project_dir,
            state: Arc::new(Mutex::new(state_store)),
            scheduler: Arc::new(Mutex::new(None)),
            isolation: Arc::new(isolation),
            worker: Arc::new(worker),
        })
    }

    /// Start the IPC server and run the accept loop.
    ///
    /// Writes a PID file on startup and cleans it up (along with the socket)
    /// on shutdown or when a SIGINT/SIGTERM signal is received.
    ///
    /// Also spawns a background task executor that polls the scheduler for
    /// assignable tasks and dispatches them to worker processes.
    pub async fn run(self) -> Result<()> {
        let orca_dir = self.project_dir.join(".orca");
        let socket_path = self.config.socket_path(&self.project_dir);

        // Refuse to start if another daemon is already running.
        check_existing_daemon(&orca_dir)?;
        write_pid_file(&orca_dir)?;

        // The guard removes the PID file and socket when dropped.
        let _guard = DaemonGuard {
            orca_dir,
            socket_path: socket_path.clone(),
        };

        let state = Arc::clone(&self.state);
        let scheduler = Arc::clone(&self.scheduler);
        let config = self.config.clone();
        let worker = Arc::clone(&self.worker);
        let isolation = Arc::clone(&self.isolation);
        let project_dir = self.project_dir.clone();

        let handler: RpcHandler = Arc::new(move |req: RpcRequest| {
            handle_request(
                req,
                Arc::clone(&state),
                Arc::clone(&scheduler),
                config.clone(),
                Arc::clone(&worker),
                Arc::clone(&isolation),
                project_dir.clone(),
            )
        });

        info!("starting orca daemon at {}", socket_path.display());
        let server = IpcServer::bind(&socket_path, handler)?;

        // Create and spawn the task executor.
        let terminal_impl = terminal::create_terminal(&self.config.terminal.provider);
        let executor = TaskExecutor::new(
            Arc::clone(&self.state),
            Arc::clone(&self.scheduler),
            Arc::clone(&self.worker),
            Arc::clone(&self.isolation),
            Arc::from(terminal_impl),
            self.config.clone(),
            self.project_dir.clone(),
        );

        let executor_handle = tokio::spawn(async move {
            executor.run().await;
        });

        tokio::select! {
            result = server.run() => result,
            _ = executor_handle => {
                info!("executor loop exited unexpectedly");
                Ok(())
            }
            _ = tokio::signal::ctrl_c() => {
                info!("received shutdown signal");
                Ok(())
            }
        }
        // _guard is dropped here, cleaning up PID file and socket.
    }
}

/// Top-level RPC dispatcher: routes method names to handler functions.
fn handle_request(
    req: RpcRequest,
    state: Arc<Mutex<StateStore>>,
    scheduler: Arc<Mutex<Option<Scheduler>>>,
    config: Config,
    _worker: Arc<dyn Worker>,
    _isolation: Arc<IsolationManager>,
    _project_dir: PathBuf,
) -> RpcResponse {
    let id = req.id.clone();
    match req.method.as_str() {
        "ping" => RpcResponse::success(id, json!({"pong": true})),
        "orca_plan" => handle_plan(id, req.params, &state, &scheduler),
        "orca_status" => handle_status(id, req.params, &state, &config),
        "orca_task_detail" => handle_task_detail(id, req.params, &state),
        "orca_decide" => handle_decide(id, req.params, &state),
        "orca_review" => handle_review(id, req.params, &state),
        "orca_cancel" => handle_cancel(id, req.params, &state),
        "orca_worker_list" => handle_worker_list(id, &state),
        "orca_merge" => handle_merge(id, req.params, &state),
        _ => RpcResponse::error(
            id,
            RpcError {
                code: METHOD_NOT_FOUND,
                message: format!("unknown method: {}", req.method),
                data: None,
            },
        ),
    }
}

/// Parse a Plan from params, validate via DependencyGraph, add tasks to state.
fn handle_plan(
    id: Value,
    params: Value,
    state: &Arc<Mutex<StateStore>>,
    scheduler: &Arc<Mutex<Option<Scheduler>>>,
) -> RpcResponse {
    let plan: Plan = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return RpcResponse::error(
                id,
                RpcError {
                    code: INVALID_PARAMS,
                    message: format!("invalid plan: {e}"),
                    data: None,
                },
            );
        }
    };

    // Validate the dependency graph (detects cycles, invalid edges).
    let sched = match Scheduler::new(&plan.tasks, &plan.dependencies) {
        Ok(s) => s,
        Err(e) => {
            return RpcResponse::error(
                id,
                RpcError {
                    code: INVALID_PARAMS,
                    message: format!("invalid dependency graph: {e}"),
                    data: None,
                },
            );
        }
    };

    let mut store = state.lock().unwrap();

    // Add all tasks to state.
    let task_ids: Vec<String> = plan.tasks.iter().map(|t| t.id.clone()).collect();
    for spec in plan.tasks {
        store.add_task(Task::new(spec));
    }
    store.state_mut().active_plan_id = Some(plan.id.clone());

    // Persist state.
    if let Err(e) = store.save() {
        return RpcResponse::error(
            id,
            RpcError {
                code: INTERNAL_ERROR,
                message: format!("failed to save state: {e}"),
                data: None,
            },
        );
    }

    // Log the event.
    let _ = store.log_event(
        "plan_loaded",
        json!({"plan_id": plan.id, "task_count": task_ids.len()}),
    );

    drop(store);

    // Install the scheduler.
    let mut sched_lock = scheduler.lock().unwrap();
    *sched_lock = Some(sched);

    RpcResponse::success(
        id,
        json!({
            "plan_id": plan.id,
            "tasks": task_ids,
        }),
    )
}

/// Return tasks (optionally filtered by state) and pending escalations.
fn handle_status(
    id: Value,
    params: Value,
    state: &Arc<Mutex<StateStore>>,
    config: &Config,
) -> RpcResponse {
    let store = state.lock().unwrap();
    let router = EscalationRouter::new(config.escalation.clone());

    let state_filter: Option<String> = params
        .get("state")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let tasks: Vec<Value> = store
        .all_tasks()
        .into_iter()
        .filter(|(_, task)| {
            if let Some(ref filter) = state_filter {
                let task_state = serde_json::to_value(task.state).unwrap_or(Value::Null);
                task_state.as_str().is_some_and(|s| s == filter)
            } else {
                true
            }
        })
        .map(|(task_id, task)| {
            json!({
                "id": task_id,
                "title": task.spec.title,
                "state": task.state,
                "worker_id": task.worker_id,
            })
        })
        .collect();

    let escalations: Vec<Value> = store
        .pending_escalations()
        .into_iter()
        .map(|e| {
            let route = router.route(e);
            json!({
                "id": e.id,
                "task_id": e.task_id,
                "category": e.category,
                "summary": e.summary,
                "route": route,
            })
        })
        .collect();

    RpcResponse::success(
        id,
        json!({
            "tasks": tasks,
            "escalations": escalations,
        }),
    )
}

/// Return full details for a single task.
fn handle_task_detail(id: Value, params: Value, state: &Arc<Mutex<StateStore>>) -> RpcResponse {
    let task_id = match params.get("task_id").and_then(|v| v.as_str()) {
        Some(tid) => tid.to_string(),
        None => {
            return RpcResponse::error(
                id,
                RpcError {
                    code: INVALID_PARAMS,
                    message: "missing required param: task_id".to_string(),
                    data: None,
                },
            );
        }
    };

    let store = state.lock().unwrap();
    match store.get_task(&task_id) {
        Some(task) => {
            let task_value = serde_json::to_value(task).unwrap_or(Value::Null);
            RpcResponse::success(id, task_value)
        }
        None => RpcResponse::error(
            id,
            RpcError {
                code: TASK_NOT_FOUND,
                message: format!("task not found: {task_id}"),
                data: None,
            },
        ),
    }
}

/// Resolve an escalation: remove it from state and resume the blocked task.
fn handle_decide(id: Value, params: Value, state: &Arc<Mutex<StateStore>>) -> RpcResponse {
    let escalation_id = match params.get("escalation_id").and_then(|v| v.as_str()) {
        Some(eid) => eid.to_string(),
        None => {
            return RpcResponse::error(
                id,
                RpcError {
                    code: INVALID_PARAMS,
                    message: "missing required param: escalation_id".to_string(),
                    data: None,
                },
            );
        }
    };

    let decision = params
        .get("decision")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut store = state.lock().unwrap();

    // Remove the escalation.
    let escalation = match store.remove_escalation(&escalation_id) {
        Some(e) => e,
        None => {
            return RpcResponse::error(
                id,
                RpcError {
                    code: ESCALATION_NOT_FOUND,
                    message: format!("escalation not found: {escalation_id}"),
                    data: None,
                },
            );
        }
    };

    // Resume the blocked task (Blocked -> Running).
    let task_id = escalation.task_id.clone();
    if let Some(task) = store.get_task_mut(&task_id) {
        if task.state == TaskState::Blocked {
            task.escalation_id = None;
            let _ = task.transition_to(TaskState::Running);
        }
    }

    let _ = store.save();
    let _ = store.log_event(
        "escalation_decided",
        json!({"escalation_id": escalation_id, "task_id": task_id, "decision": decision}),
    );

    RpcResponse::success(
        id,
        json!({
            "escalation_id": escalation_id,
            "task_id": task_id,
            "decision": decision,
        }),
    )
}

/// Transition a task to Accepted or Rejected (only valid from Review state).
fn handle_review(id: Value, params: Value, state: &Arc<Mutex<StateStore>>) -> RpcResponse {
    let task_id = match params.get("task_id").and_then(|v| v.as_str()) {
        Some(tid) => tid.to_string(),
        None => {
            return RpcResponse::error(
                id,
                RpcError {
                    code: INVALID_PARAMS,
                    message: "missing required param: task_id".to_string(),
                    data: None,
                },
            );
        }
    };

    let verdict = match params.get("verdict").and_then(|v| v.as_str()) {
        Some(v) => v.to_string(),
        None => {
            return RpcResponse::error(
                id,
                RpcError {
                    code: INVALID_PARAMS,
                    message: "missing required param: verdict".to_string(),
                    data: None,
                },
            );
        }
    };

    let target_state = match verdict.as_str() {
        "accepted" => TaskState::Accepted,
        "rejected" => TaskState::Rejected,
        _ => {
            return RpcResponse::error(
                id,
                RpcError {
                    code: INVALID_PARAMS,
                    message: format!(
                        "invalid verdict: {verdict} (must be 'accepted' or 'rejected')"
                    ),
                    data: None,
                },
            );
        }
    };

    let mut store = state.lock().unwrap();

    let task = match store.get_task_mut(&task_id) {
        Some(t) => t,
        None => {
            return RpcResponse::error(
                id,
                RpcError {
                    code: TASK_NOT_FOUND,
                    message: format!("task not found: {task_id}"),
                    data: None,
                },
            );
        }
    };

    // Validate that the task is in Review state.
    if task.state != TaskState::Review {
        return RpcResponse::error(
            id,
            RpcError {
                code: INVALID_STATE_TRANSITION,
                message: format!("task {task_id} is in state {:?}, not Review", task.state),
                data: None,
            },
        );
    }

    if let Err(e) = task.transition_to(target_state) {
        return RpcResponse::error(
            id,
            RpcError {
                code: INVALID_STATE_TRANSITION,
                message: e,
                data: None,
            },
        );
    }

    let _ = store.save();
    let _ = store.log_event(
        "task_reviewed",
        json!({"task_id": task_id, "verdict": verdict}),
    );

    RpcResponse::success(
        id,
        json!({
            "task_id": task_id,
            "verdict": verdict,
        }),
    )
}

/// Cancel a task (transition to Cancelled).
fn handle_cancel(id: Value, params: Value, state: &Arc<Mutex<StateStore>>) -> RpcResponse {
    let task_id = match params.get("task_id").and_then(|v| v.as_str()) {
        Some(tid) => tid.to_string(),
        None => {
            return RpcResponse::error(
                id,
                RpcError {
                    code: INVALID_PARAMS,
                    message: "missing required param: task_id".to_string(),
                    data: None,
                },
            );
        }
    };

    let mut store = state.lock().unwrap();

    let task = match store.get_task_mut(&task_id) {
        Some(t) => t,
        None => {
            return RpcResponse::error(
                id,
                RpcError {
                    code: TASK_NOT_FOUND,
                    message: format!("task not found: {task_id}"),
                    data: None,
                },
            );
        }
    };

    if let Err(e) = task.transition_to(TaskState::Cancelled) {
        return RpcResponse::error(
            id,
            RpcError {
                code: INVALID_STATE_TRANSITION,
                message: e,
                data: None,
            },
        );
    }

    let _ = store.save();
    let _ = store.log_event("task_cancelled", json!({"task_id": task_id}));

    RpcResponse::success(id, json!({"task_id": task_id, "state": "cancelled"}))
}

/// Return all registered workers.
fn handle_worker_list(id: Value, state: &Arc<Mutex<StateStore>>) -> RpcResponse {
    let store = state.lock().unwrap();

    let workers: Vec<Value> = store
        .state()
        .workers
        .values()
        .map(|w| serde_json::to_value(w).unwrap_or(Value::Null))
        .collect();

    RpcResponse::success(id, json!({"workers": workers}))
}

/// Return merge info for tasks that have branch names.
fn handle_merge(id: Value, params: Value, state: &Arc<Mutex<StateStore>>) -> RpcResponse {
    let task_id = match params.get("task_id").and_then(|v| v.as_str()) {
        Some(tid) => tid.to_string(),
        None => {
            return RpcResponse::error(
                id,
                RpcError {
                    code: INVALID_PARAMS,
                    message: "missing required param: task_id".to_string(),
                    data: None,
                },
            );
        }
    };

    let store = state.lock().unwrap();

    let task = match store.get_task(&task_id) {
        Some(t) => t,
        None => {
            return RpcResponse::error(
                id,
                RpcError {
                    code: TASK_NOT_FOUND,
                    message: format!("task not found: {task_id}"),
                    data: None,
                },
            );
        }
    };

    RpcResponse::success(
        id,
        json!({
            "task_id": task_id,
            "branch_name": task.branch_name,
            "worktree_path": task.worktree_path,
            "state": task.state,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::RpcRequest;
    use serde_json::json;

    fn make_state() -> Arc<Mutex<StateStore>> {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path()).unwrap();
        // Leak the tempdir so it persists for the test duration.
        std::mem::forget(dir);
        Arc::new(Mutex::new(store))
    }

    fn make_scheduler() -> Arc<Mutex<Option<Scheduler>>> {
        Arc::new(Mutex::new(None))
    }

    fn make_config() -> Config {
        Config::default()
    }

    fn make_worker() -> Arc<dyn Worker> {
        Arc::new(CodexWorker::new(Default::default()))
    }

    fn make_isolation() -> Arc<IsolationManager> {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let worktree = project.join(".agents/worktree");
        std::mem::forget(dir);
        Arc::new(IsolationManager::new(&project, &worktree))
    }

    fn dispatch(method: &str, params: Value) -> RpcResponse {
        let state = make_state();
        let scheduler = make_scheduler();
        let config = make_config();
        let worker = make_worker();
        let isolation = make_isolation();
        let project_dir = PathBuf::from("/tmp/test-project");

        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: json!(1),
            method: method.to_string(),
            params,
        };

        handle_request(
            req,
            state,
            scheduler,
            config,
            worker,
            isolation,
            project_dir,
        )
    }

    fn dispatch_with_state(
        method: &str,
        params: Value,
        state: &Arc<Mutex<StateStore>>,
        scheduler: &Arc<Mutex<Option<Scheduler>>>,
    ) -> RpcResponse {
        let config = make_config();
        let worker = make_worker();
        let isolation = make_isolation();
        let project_dir = PathBuf::from("/tmp/test-project");

        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: json!(1),
            method: method.to_string(),
            params,
        };

        handle_request(
            req,
            Arc::clone(state),
            Arc::clone(scheduler),
            config,
            worker,
            isolation,
            project_dir,
        )
    }

    #[test]
    fn test_ping() {
        let resp = dispatch("ping", json!({}));
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap()["pong"], true);
    }

    #[test]
    fn test_unknown_method() {
        let resp = dispatch("nonexistent", json!({}));
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, METHOD_NOT_FOUND);
    }

    #[test]
    fn test_plan_valid() {
        let state = make_state();
        let scheduler = make_scheduler();

        let plan = json!({
            "id": "plan-1",
            "tasks": [
                {
                    "id": "t1",
                    "title": "Task 1",
                    "description": "First task",
                    "context": {"files": [], "references": [], "constraints": []},
                    "isolation": "auto",
                    "depends_on": [],
                    "priority": 0
                },
                {
                    "id": "t2",
                    "title": "Task 2",
                    "description": "Second task",
                    "context": {"files": [], "references": [], "constraints": []},
                    "isolation": "auto",
                    "depends_on": ["t1"],
                    "priority": 0
                }
            ],
            "dependencies": [
                {"from": "t1", "to": "t2"}
            ],
            "created_at": "2026-01-01T00:00:00Z"
        });

        let resp = dispatch_with_state("orca_plan", plan, &state, &scheduler);
        assert!(
            resp.error.is_none(),
            "expected success, got: {:?}",
            resp.error
        );
        let result = resp.result.unwrap();
        assert_eq!(result["plan_id"], "plan-1");

        // Verify tasks are in state.
        let store = state.lock().unwrap();
        assert!(store.get_task("t1").is_some());
        assert!(store.get_task("t2").is_some());
        assert_eq!(store.state().active_plan_id, Some("plan-1".to_string()));

        // Verify scheduler was installed.
        let sched = scheduler.lock().unwrap();
        assert!(sched.is_some());
    }

    #[test]
    fn test_plan_cycle_rejected() {
        let plan = json!({
            "id": "plan-cycle",
            "tasks": [
                {
                    "id": "a",
                    "title": "A",
                    "description": "",
                    "context": {"files": [], "references": [], "constraints": []},
                    "isolation": "auto",
                    "depends_on": [],
                    "priority": 0
                },
                {
                    "id": "b",
                    "title": "B",
                    "description": "",
                    "context": {"files": [], "references": [], "constraints": []},
                    "isolation": "auto",
                    "depends_on": [],
                    "priority": 0
                }
            ],
            "dependencies": [
                {"from": "a", "to": "b"},
                {"from": "b", "to": "a"}
            ],
            "created_at": "2026-01-01T00:00:00Z"
        });

        let resp = dispatch("orca_plan", plan);
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(err.message.contains("cycle"));
    }

    #[test]
    fn test_plan_invalid_params() {
        let resp = dispatch("orca_plan", json!({"bad": true}));
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    #[test]
    fn test_status_empty() {
        let resp = dispatch("orca_status", json!({}));
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["tasks"], json!([]));
        assert_eq!(result["escalations"], json!([]));
    }

    #[test]
    fn test_status_with_filter() {
        let state = make_state();
        let scheduler = make_scheduler();

        // Load a plan first.
        let plan = json!({
            "id": "p1",
            "tasks": [
                {
                    "id": "t1",
                    "title": "T1",
                    "description": "",
                    "context": {"files": [], "references": [], "constraints": []},
                    "isolation": "auto",
                    "depends_on": [],
                    "priority": 0
                }
            ],
            "dependencies": [],
            "created_at": "2026-01-01T00:00:00Z"
        });
        dispatch_with_state("orca_plan", plan, &state, &scheduler);

        // Filter by "pending" should return the task.
        let resp = dispatch_with_state(
            "orca_status",
            json!({"state": "pending"}),
            &state,
            &scheduler,
        );
        let result = resp.result.unwrap();
        assert_eq!(result["tasks"].as_array().unwrap().len(), 1);

        // Filter by "running" should return nothing.
        let resp = dispatch_with_state(
            "orca_status",
            json!({"state": "running"}),
            &state,
            &scheduler,
        );
        let result = resp.result.unwrap();
        assert_eq!(result["tasks"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_task_detail_found() {
        let state = make_state();
        let scheduler = make_scheduler();

        let plan = json!({
            "id": "p1",
            "tasks": [{
                "id": "t1",
                "title": "Test Task",
                "description": "A test",
                "context": {"files": ["src/main.rs"], "references": [], "constraints": []},
                "isolation": "auto",
                "depends_on": [],
                "priority": 5
            }],
            "dependencies": [],
            "created_at": "2026-01-01T00:00:00Z"
        });
        dispatch_with_state("orca_plan", plan, &state, &scheduler);

        let resp = dispatch_with_state(
            "orca_task_detail",
            json!({"task_id": "t1"}),
            &state,
            &scheduler,
        );
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["spec"]["title"], "Test Task");
        assert_eq!(result["state"], "pending");
    }

    #[test]
    fn test_task_detail_not_found() {
        let resp = dispatch("orca_task_detail", json!({"task_id": "nonexistent"}));
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, TASK_NOT_FOUND);
    }

    #[test]
    fn test_task_detail_missing_param() {
        let resp = dispatch("orca_task_detail", json!({}));
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    #[test]
    fn test_review_accepted() {
        let state = make_state();
        let scheduler = make_scheduler();

        // Load plan, then manually transition task to Review state.
        let plan = json!({
            "id": "p1",
            "tasks": [{
                "id": "t1", "title": "T1", "description": "",
                "context": {"files": [], "references": [], "constraints": []},
                "isolation": "auto", "depends_on": [], "priority": 0
            }],
            "dependencies": [],
            "created_at": "2026-01-01T00:00:00Z"
        });
        dispatch_with_state("orca_plan", plan, &state, &scheduler);

        // Walk task through: Pending -> Assigned -> Running -> Done -> Review.
        {
            let mut store = state.lock().unwrap();
            let task = store.get_task_mut("t1").unwrap();
            task.transition_to(TaskState::Assigned).unwrap();
            task.transition_to(TaskState::Running).unwrap();
            task.transition_to(TaskState::Done).unwrap();
            task.transition_to(TaskState::Review).unwrap();
        }

        let resp = dispatch_with_state(
            "orca_review",
            json!({"task_id": "t1", "verdict": "accepted"}),
            &state,
            &scheduler,
        );
        assert!(
            resp.error.is_none(),
            "expected success, got: {:?}",
            resp.error
        );
        assert_eq!(resp.result.unwrap()["verdict"], "accepted");

        let store = state.lock().unwrap();
        assert_eq!(store.get_task("t1").unwrap().state, TaskState::Accepted);
    }

    #[test]
    fn test_review_rejected() {
        let state = make_state();
        let scheduler = make_scheduler();

        let plan = json!({
            "id": "p1",
            "tasks": [{
                "id": "t1", "title": "T1", "description": "",
                "context": {"files": [], "references": [], "constraints": []},
                "isolation": "auto", "depends_on": [], "priority": 0
            }],
            "dependencies": [],
            "created_at": "2026-01-01T00:00:00Z"
        });
        dispatch_with_state("orca_plan", plan, &state, &scheduler);

        {
            let mut store = state.lock().unwrap();
            let task = store.get_task_mut("t1").unwrap();
            task.transition_to(TaskState::Assigned).unwrap();
            task.transition_to(TaskState::Running).unwrap();
            task.transition_to(TaskState::Done).unwrap();
            task.transition_to(TaskState::Review).unwrap();
        }

        let resp = dispatch_with_state(
            "orca_review",
            json!({"task_id": "t1", "verdict": "rejected"}),
            &state,
            &scheduler,
        );
        assert!(resp.error.is_none());

        let store = state.lock().unwrap();
        assert_eq!(store.get_task("t1").unwrap().state, TaskState::Rejected);
    }

    #[test]
    fn test_review_wrong_state() {
        let state = make_state();
        let scheduler = make_scheduler();

        let plan = json!({
            "id": "p1",
            "tasks": [{
                "id": "t1", "title": "T1", "description": "",
                "context": {"files": [], "references": [], "constraints": []},
                "isolation": "auto", "depends_on": [], "priority": 0
            }],
            "dependencies": [],
            "created_at": "2026-01-01T00:00:00Z"
        });
        dispatch_with_state("orca_plan", plan, &state, &scheduler);

        // Task is in Pending, not Review.
        let resp = dispatch_with_state(
            "orca_review",
            json!({"task_id": "t1", "verdict": "accepted"}),
            &state,
            &scheduler,
        );
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, INVALID_STATE_TRANSITION);
    }

    #[test]
    fn test_review_invalid_verdict() {
        let resp = dispatch("orca_review", json!({"task_id": "t1", "verdict": "maybe"}));
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    #[test]
    fn test_cancel_from_pending() {
        let state = make_state();
        let scheduler = make_scheduler();

        let plan = json!({
            "id": "p1",
            "tasks": [{
                "id": "t1", "title": "T1", "description": "",
                "context": {"files": [], "references": [], "constraints": []},
                "isolation": "auto", "depends_on": [], "priority": 0
            }],
            "dependencies": [],
            "created_at": "2026-01-01T00:00:00Z"
        });
        dispatch_with_state("orca_plan", plan, &state, &scheduler);

        // Pending cannot transition to Cancelled (only Assigned/Running can).
        let resp = dispatch_with_state("orca_cancel", json!({"task_id": "t1"}), &state, &scheduler);
        // Should fail because Pending -> Cancelled is not a valid transition.
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, INVALID_STATE_TRANSITION);
    }

    #[test]
    fn test_cancel_from_assigned() {
        let state = make_state();
        let scheduler = make_scheduler();

        let plan = json!({
            "id": "p1",
            "tasks": [{
                "id": "t1", "title": "T1", "description": "",
                "context": {"files": [], "references": [], "constraints": []},
                "isolation": "auto", "depends_on": [], "priority": 0
            }],
            "dependencies": [],
            "created_at": "2026-01-01T00:00:00Z"
        });
        dispatch_with_state("orca_plan", plan, &state, &scheduler);

        {
            let mut store = state.lock().unwrap();
            let task = store.get_task_mut("t1").unwrap();
            task.transition_to(TaskState::Assigned).unwrap();
        }

        let resp = dispatch_with_state("orca_cancel", json!({"task_id": "t1"}), &state, &scheduler);
        assert!(resp.error.is_none());

        let store = state.lock().unwrap();
        assert_eq!(store.get_task("t1").unwrap().state, TaskState::Cancelled);
    }

    #[test]
    fn test_cancel_not_found() {
        let resp = dispatch("orca_cancel", json!({"task_id": "nonexistent"}));
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, TASK_NOT_FOUND);
    }

    #[test]
    fn test_decide_escalation() {
        let state = make_state();
        let scheduler = make_scheduler();

        // Load a plan and block the task with an escalation.
        let plan = json!({
            "id": "p1",
            "tasks": [{
                "id": "t1", "title": "T1", "description": "",
                "context": {"files": [], "references": [], "constraints": []},
                "isolation": "auto", "depends_on": [], "priority": 0
            }],
            "dependencies": [],
            "created_at": "2026-01-01T00:00:00Z"
        });
        dispatch_with_state("orca_plan", plan, &state, &scheduler);

        {
            let mut store = state.lock().unwrap();
            let task = store.get_task_mut("t1").unwrap();
            task.transition_to(TaskState::Assigned).unwrap();
            task.transition_to(TaskState::Running).unwrap();
            task.transition_to(TaskState::Blocked).unwrap();
            task.escalation_id = Some("esc-1".to_string());

            store.add_escalation(crate::escalation::EscalationRequest {
                id: "esc-1".to_string(),
                task_id: "t1".to_string(),
                worker_id: "w1".to_string(),
                category: crate::escalation::EscalationCategory::ImplementationChoice,
                summary: "Which approach?".to_string(),
                options: vec![],
                context: crate::escalation::EscalationContext {
                    relevant_files: vec![],
                    worker_recommendation: None,
                },
            });
        }

        let resp = dispatch_with_state(
            "orca_decide",
            json!({"escalation_id": "esc-1", "decision": "option_a"}),
            &state,
            &scheduler,
        );
        assert!(
            resp.error.is_none(),
            "expected success, got: {:?}",
            resp.error
        );

        let store = state.lock().unwrap();
        // Task should be resumed to Running.
        assert_eq!(store.get_task("t1").unwrap().state, TaskState::Running);
        assert!(store.get_task("t1").unwrap().escalation_id.is_none());
        // Escalation should be removed.
        assert!(store.pending_escalations().is_empty());
    }

    #[test]
    fn test_decide_not_found() {
        let resp = dispatch("orca_decide", json!({"escalation_id": "nonexistent"}));
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, ESCALATION_NOT_FOUND);
    }

    #[test]
    fn test_worker_list_empty() {
        let resp = dispatch("orca_worker_list", json!({}));
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["workers"], json!([]));
    }

    #[test]
    fn test_merge_info() {
        let state = make_state();
        let scheduler = make_scheduler();

        let plan = json!({
            "id": "p1",
            "tasks": [{
                "id": "t1", "title": "T1", "description": "",
                "context": {"files": [], "references": [], "constraints": []},
                "isolation": "worktree", "depends_on": [], "priority": 0
            }],
            "dependencies": [],
            "created_at": "2026-01-01T00:00:00Z"
        });
        dispatch_with_state("orca_plan", plan, &state, &scheduler);

        // Simulate branch assignment.
        {
            let mut store = state.lock().unwrap();
            let task = store.get_task_mut("t1").unwrap();
            task.branch_name = Some("orca/t1".to_string());
            task.worktree_path = Some("/tmp/worktree/t1".to_string());
        }

        let resp = dispatch_with_state("orca_merge", json!({"task_id": "t1"}), &state, &scheduler);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["branch_name"], "orca/t1");
        assert_eq!(result["worktree_path"], "/tmp/worktree/t1");
    }

    #[test]
    fn test_merge_not_found() {
        let resp = dispatch("orca_merge", json!({"task_id": "nonexistent"}));
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, TASK_NOT_FOUND);
    }
}
