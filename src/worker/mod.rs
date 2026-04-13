pub mod codex;

use crate::types::{TaskOutput, TaskSpec, WorkerStatus};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Trait representing a worker that can execute tasks.
///
/// Implementations manage the lifecycle of external processes (e.g. Codex CLI)
/// that perform coding work on behalf of the orchestrator.
#[async_trait]
pub trait Worker: Send + Sync {
    /// Spawn a new worker process in the given working directory.
    async fn spawn(&self, worker_id: &str, work_dir: &str) -> Result<()>;

    /// Dispatch a task to a running worker by writing a prompt to its stdin.
    async fn dispatch(&self, worker_id: &str, task: &TaskSpec) -> Result<()>;

    /// Check the health of a worker process.
    async fn health_check(&self, worker_id: &str) -> Result<WorkerStatus>;

    /// Send an interrupt signal to a worker process.
    async fn interrupt(&self, worker_id: &str) -> Result<()>;

    /// Clean up a worker process, killing it if still running.
    async fn cleanup(&self, worker_id: &str) -> Result<()>;

    /// Take stdout from the child process for reading output.
    /// Returns None if stdout was already taken or the worker doesn't exist.
    async fn take_stdout(&self, worker_id: &str) -> Result<Option<tokio::process::ChildStdout>>;

    /// Return the type identifier for this worker implementation.
    fn worker_type(&self) -> &str;
}

/// Messages parsed from worker stdout output lines.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum WorkerMessage {
    /// Task completed with output.
    Done(TaskOutput),
    /// Worker is escalating a decision.
    Escalate(serde_json::Value),
    /// Worker is blocked and needs help.
    Blocked(serde_json::Value),
    /// Progress update from the worker.
    Progress(String),
    /// Raw output line (no marker detected).
    Output(String),
}
