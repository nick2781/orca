use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::config::WorkerConfig;
use crate::types::{TaskOutput, TaskSpec, WorkerStatus};
use crate::worker::{Worker, WorkerMessage};

// Output markers embedded in worker stdout to signal structured events.
pub const MARKER_DONE: &str = "[ORCA:DONE]";
pub const MARKER_ESCALATE: &str = "[ORCA:ESCALATE]";
pub const MARKER_BLOCKED: &str = "[ORCA:BLOCKED]";
pub const MARKER_PROGRESS: &str = "[ORCA:PROGRESS]";

/// Parse a single line of worker output into a structured message.
///
/// Lines prefixed with a known marker are parsed into the corresponding
/// variant. The payload after the marker is treated as JSON where applicable;
/// for `DONE` markers a JSON parse failure falls back to a plain-text
/// `TaskOutput` with the remainder placed in `stdout`.
pub fn parse_worker_line(line: &str) -> WorkerMessage {
    let trimmed = line.trim();

    if let Some(rest) = trimmed.strip_prefix(MARKER_DONE) {
        let payload = rest.trim();
        match serde_json::from_str::<TaskOutput>(payload) {
            Ok(output) => WorkerMessage::Done(output),
            Err(_) => WorkerMessage::Done(TaskOutput {
                files_changed: Vec::new(),
                tests_passed: false,
                diff_summary: String::new(),
                stdout: payload.to_string(),
            }),
        }
    } else if let Some(rest) = trimmed.strip_prefix(MARKER_ESCALATE) {
        let payload = rest.trim();
        let value =
            serde_json::from_str(payload).unwrap_or(serde_json::Value::String(payload.to_string()));
        WorkerMessage::Escalate(value)
    } else if let Some(rest) = trimmed.strip_prefix(MARKER_BLOCKED) {
        let payload = rest.trim();
        let value =
            serde_json::from_str(payload).unwrap_or(serde_json::Value::String(payload.to_string()));
        WorkerMessage::Blocked(value)
    } else if let Some(rest) = trimmed.strip_prefix(MARKER_PROGRESS) {
        WorkerMessage::Progress(rest.trim().to_string())
    } else {
        WorkerMessage::Output(line.to_string())
    }
}

/// Build the prompt string sent to a Codex worker's stdin.
///
/// The prompt includes the task specification, working directory, relevant
/// files, constraints, references, and instructions for using output markers.
pub fn generate_prompt(task: &TaskSpec, work_dir: &str) -> String {
    let mut prompt = String::new();

    prompt.push_str(&format!("# Task: {}\n\n", task.title));
    prompt.push_str(&format!("{}\n\n", task.description));
    prompt.push_str(&format!("Working directory: {}\n\n", work_dir));

    if !task.context.files.is_empty() {
        prompt.push_str("## Relevant files\n");
        for file in &task.context.files {
            prompt.push_str(&format!("- {}\n", file));
        }
        prompt.push('\n');
    }

    if !task.context.constraints.is_empty() {
        prompt.push_str("## Constraints\n");
        for constraint in &task.context.constraints {
            prompt.push_str(&format!("- {}\n", constraint));
        }
        prompt.push('\n');
    }

    if !task.context.references.is_empty() {
        prompt.push_str("## References\n");
        for reference in &task.context.references {
            prompt.push_str(&format!("- {}\n", reference));
        }
        prompt.push('\n');
    }

    prompt.push_str("## Output rules\n\n");
    prompt.push_str("When you finish, output a single line starting with the marker:\n");
    prompt.push_str(&format!(
        "  {} followed by a JSON object: {{\"files_changed\": [...], \"tests_passed\": bool, \"diff_summary\": \"...\", \"stdout\": \"...\"}}\n\n",
        MARKER_DONE
    ));
    prompt.push_str("If you need to escalate a decision, output:\n");
    prompt.push_str(&format!(
        "  {} followed by a JSON object describing the decision needed.\n\n",
        MARKER_ESCALATE
    ));
    prompt.push_str("If you are blocked and cannot proceed, output:\n");
    prompt.push_str(&format!(
        "  {} followed by a JSON object describing the blocker.\n\n",
        MARKER_BLOCKED
    ));
    prompt.push_str("For progress updates, output:\n");
    prompt.push_str(&format!(
        "  {} followed by a short status message.\n",
        MARKER_PROGRESS
    ));

    prompt
}

/// A worker implementation backed by the Codex CLI.
pub struct CodexWorker {
    config: WorkerConfig,
    processes: Arc<Mutex<HashMap<String, Child>>>,
}

impl CodexWorker {
    /// Create a new CodexWorker with the given configuration.
    pub fn new(config: WorkerConfig) -> Self {
        Self {
            config,
            processes: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl Worker for CodexWorker {
    async fn spawn(&self, worker_id: &str, work_dir: &str) -> Result<()> {
        let child = Command::new(&self.config.command)
            .args(&self.config.args)
            .current_dir(work_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!(
                    "failed to spawn worker '{}' with command '{}'",
                    worker_id, self.config.command
                )
            })?;

        let mut procs = self.processes.lock().await;
        procs.insert(worker_id.to_string(), child);
        Ok(())
    }

    async fn dispatch(&self, worker_id: &str, task: &TaskSpec) -> Result<()> {
        let mut procs = self.processes.lock().await;
        let child = procs
            .get_mut(worker_id)
            .ok_or_else(|| anyhow!("worker '{}' not found", worker_id))?;

        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("stdin not available for worker '{}'", worker_id))?;

        let work_dir = "."; // Use child's cwd set during spawn
        let prompt = generate_prompt(task, work_dir);
        stdin
            .write_all(prompt.as_bytes())
            .await
            .with_context(|| format!("failed to write prompt to worker '{}'", worker_id))?;
        stdin
            .flush()
            .await
            .with_context(|| format!("failed to flush stdin for worker '{}'", worker_id))?;

        Ok(())
    }

    async fn health_check(&self, worker_id: &str) -> Result<WorkerStatus> {
        let mut procs = self.processes.lock().await;
        let child = procs
            .get_mut(worker_id)
            .ok_or_else(|| anyhow!("worker '{}' not found", worker_id))?;

        match child.try_wait()? {
            Some(_status) => Ok(WorkerStatus::Dead),
            None => Ok(WorkerStatus::Busy),
        }
    }

    async fn interrupt(&self, worker_id: &str) -> Result<()> {
        let mut procs = self.processes.lock().await;
        let child = procs
            .get_mut(worker_id)
            .ok_or_else(|| anyhow!("worker '{}' not found", worker_id))?;

        child
            .kill()
            .await
            .with_context(|| format!("failed to kill worker '{}'", worker_id))?;

        Ok(())
    }

    async fn cleanup(&self, worker_id: &str) -> Result<()> {
        let mut procs = self.processes.lock().await;
        if let Some(mut child) = procs.remove(worker_id) {
            // Try to kill if still running; ignore errors (may already be dead).
            let _ = child.kill().await;
        }
        Ok(())
    }

    fn worker_type(&self) -> &str {
        "codex"
    }
}
