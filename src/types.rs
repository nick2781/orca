use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Specification for a task to be executed by a worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    pub id: String,
    pub title: String,
    pub description: String,
    pub context: TaskContext,
    pub isolation: IsolationMode,
    pub depends_on: Vec<String>,
    pub priority: u32,
}

/// Contextual information provided to a worker for task execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskContext {
    pub files: Vec<String>,
    pub references: Vec<String>,
    #[serde(default)]
    pub constraints: String,
}

/// How a task should be isolated during execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    #[default]
    Auto,
    Worktree,
    Serial,
}

/// The lifecycle state of a task.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Assigned,
    Running,
    Done,
    Blocked,
    Review,
    Accepted,
    Rejected,
    Completed,
    Cancelled,
}

/// A task with its current execution state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub spec: TaskSpec,
    pub state: TaskState,
    pub worker_id: Option<String>,
    pub escalation_id: Option<String>,
    pub output: Option<TaskOutput>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub worktree_path: Option<String>,
    pub branch_name: Option<String>,
}

/// Output produced by a completed task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskOutput {
    pub files_changed: Vec<String>,
    pub tests_passed: bool,
    pub diff_summary: String,
    pub stdout: String,
}

/// A plan consisting of tasks and their dependency graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub tasks: Vec<TaskSpec>,
    pub dependencies: Vec<Edge>,
    pub created_at: DateTime<Utc>,
}

/// A directed edge in the dependency graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
}

/// Information about a registered worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerInfo {
    pub id: String,
    pub worker_type: String,
    pub status: WorkerStatus,
    pub current_task_id: Option<String>,
    pub started_at: DateTime<Utc>,
}

/// Current status of a worker.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Idle,
    Busy,
    Dead,
}

impl Task {
    /// Create a new task from a spec, starting in Pending state.
    pub fn new(spec: TaskSpec) -> Self {
        let now = Utc::now();
        Self {
            spec,
            state: TaskState::Pending,
            worker_id: None,
            escalation_id: None,
            output: None,
            created_at: now,
            updated_at: now,
            worktree_path: None,
            branch_name: None,
        }
    }

    /// Check whether transitioning to the target state is valid.
    pub fn can_transition_to(&self, target: TaskState) -> bool {
        matches!(
            (self.state, target),
            (TaskState::Pending, TaskState::Assigned)
                | (TaskState::Assigned, TaskState::Running)
                | (TaskState::Assigned, TaskState::Cancelled)
                | (TaskState::Running, TaskState::Done)
                | (TaskState::Running, TaskState::Blocked)
                | (TaskState::Running, TaskState::Cancelled)
                | (TaskState::Blocked, TaskState::Running)
                | (TaskState::Done, TaskState::Review)
                | (TaskState::Review, TaskState::Accepted)
                | (TaskState::Review, TaskState::Rejected)
                | (TaskState::Accepted, TaskState::Completed)
                | (TaskState::Rejected, TaskState::Pending)
        )
    }

    /// Validate and apply a state transition, returning an error if invalid.
    pub fn transition_to(&mut self, target: TaskState) -> Result<(), String> {
        if self.can_transition_to(target) {
            self.state = target;
            self.updated_at = Utc::now();
            Ok(())
        } else {
            Err(format!(
                "invalid state transition: {:?} -> {:?}",
                self.state, target
            ))
        }
    }
}

impl TaskSpec {
    /// Create a new TaskSpec with sensible defaults.
    pub fn new(title: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            title: title.into(),
            description: description.into(),
            context: TaskContext::default(),
            isolation: IsolationMode::default(),
            depends_on: Vec::new(),
            priority: 0,
        }
    }
}
