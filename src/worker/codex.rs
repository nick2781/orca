use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
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

/// The AGENTS.md template embedded at compile time.
const AGENTS_MD_TEMPLATE: &str = include_str!("agents_md_template.txt");

/// Generate the AGENTS.md content customised with task-specific information.
///
/// The returned string contains the base template plus a section listing the
/// task title, description, and scoped files so the Codex agent knows what it
/// is allowed to touch.
pub fn generate_agents_md(task: &TaskSpec) -> String {
    let mut content = AGENTS_MD_TEMPLATE.to_string();

    content.push_str("\n## Current Task\n\n");
    content.push_str(&format!("**Title:** {}\n\n", task.title));
    content.push_str(&format!("**Description:** {}\n\n", task.description));

    if !task.context.files.is_empty() {
        content.push_str("**Scoped files (only modify these):**\n");
        for file in &task.context.files {
            content.push_str(&format!("- {}\n", file));
        }
        content.push('\n');
    }

    if !task.context.constraints.is_empty() {
        content.push_str(&format!(
            "**Constraints:** {}\n\n",
            task.context.constraints
        ));
    }

    content
}

/// Try to parse a line as Codex CLI JSON output (produced by `codex -q`).
///
/// Codex quiet mode emits newline-delimited JSON objects with a `"type"` field.
/// We map these to `WorkerMessage::Output` carrying the human-readable content.
fn try_parse_codex_json(line: &str) -> Option<WorkerMessage> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return None;
    }

    let obj: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    // Must have a "type" field to be considered Codex JSON output.
    obj.get("type")?;

    let type_str = obj["type"].as_str().unwrap_or("");
    let text = match type_str {
        "message" => obj
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or(trimmed)
            .to_string(),
        "result" => obj
            .get("output")
            .and_then(|v| v.as_str())
            .unwrap_or(trimmed)
            .to_string(),
        _ => trimmed.to_string(),
    };

    Some(WorkerMessage::Output(text))
}

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
    } else if let Some(msg) = try_parse_codex_json(line) {
        msg
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
        prompt.push_str(&format!("## Constraints\n{}\n\n", task.context.constraints));
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

/// Metadata stored for each spawned worker process.
struct WorkerProcess {
    child: Option<Child>,
    work_dir: String,
}

/// A worker implementation backed by the Codex CLI.
pub struct CodexWorker {
    config: WorkerConfig,
    processes: Arc<Mutex<HashMap<String, WorkerProcess>>>,
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
        // spawn only registers the work_dir. The actual process is started
        // in dispatch() because Codex CLI takes the prompt as a positional
        // argument, not via stdin.
        let mut procs = self.processes.lock().await;
        procs.insert(
            worker_id.to_string(),
            WorkerProcess {
                child: None,
                work_dir: work_dir.to_string(),
            },
        );
        Ok(())
    }

    async fn dispatch(&self, worker_id: &str, task: &TaskSpec) -> Result<()> {
        let mut procs = self.processes.lock().await;
        let wp = procs
            .get_mut(worker_id)
            .ok_or_else(|| anyhow!("worker '{}' not found", worker_id))?;

        // Write task-specific AGENTS.md so Codex has full context.
        let agents_path = Path::new(&wp.work_dir).join("AGENTS.md");
        let agents_content = generate_agents_md(task);
        std::fs::write(&agents_path, &agents_content).with_context(|| {
            format!(
                "failed to write AGENTS.md to '{}'",
                agents_path.display()
            )
        })?;

        // Build prompt and spawn Codex with prompt as positional argument.
        // Usage: codex [OPTIONS] [PROMPT]
        let prompt = generate_prompt(task, &wp.work_dir);
        let child = Command::new(&self.config.command)
            .args(&self.config.args)
            .arg(&prompt)
            .current_dir(&wp.work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!(
                    "failed to spawn codex for worker '{}' in '{}'",
                    worker_id, wp.work_dir
                )
            })?;

        wp.child = Some(child);
        Ok(())
    }

    async fn health_check(&self, worker_id: &str) -> Result<WorkerStatus> {
        let mut procs = self.processes.lock().await;
        let wp = procs
            .get_mut(worker_id)
            .ok_or_else(|| anyhow!("worker '{}' not found", worker_id))?;

        match &mut wp.child {
            Some(child) => match child.try_wait()? {
                Some(_status) => Ok(WorkerStatus::Dead),
                None => Ok(WorkerStatus::Busy),
            },
            None => Ok(WorkerStatus::Idle),
        }
    }

    async fn interrupt(&self, worker_id: &str) -> Result<()> {
        let mut procs = self.processes.lock().await;
        let wp = procs
            .get_mut(worker_id)
            .ok_or_else(|| anyhow!("worker '{}' not found", worker_id))?;

        if let Some(child) = &mut wp.child {
            child
                .kill()
                .await
                .with_context(|| format!("failed to kill worker '{}'", worker_id))?;
        }
        Ok(())
    }

    async fn cleanup(&self, worker_id: &str) -> Result<()> {
        let mut procs = self.processes.lock().await;
        if let Some(mut wp) = procs.remove(worker_id) {
            if let Some(mut child) = wp.child.take() {
                let _ = child.kill().await;
            }
        }
        Ok(())
    }

    async fn take_stdout(&self, worker_id: &str) -> Result<Option<tokio::process::ChildStdout>> {
        let mut procs = self.processes.lock().await;
        let wp = procs
            .get_mut(worker_id)
            .ok_or_else(|| anyhow!("worker '{}' not found", worker_id))?;
        Ok(wp.child.as_mut().and_then(|c| c.stdout.take()))
    }

    fn worker_type(&self) -> &str {
        "codex"
    }
}
