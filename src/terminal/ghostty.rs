use std::sync::Mutex;

use anyhow::{Context, Result};
use async_trait::async_trait;

use super::Terminal;
use crate::config::TerminalConfig;

/// Tracks the most recently created worker pane.
/// First split goes right from origin (creating the worker column).
/// Subsequent splits go down from the last worker pane (stacking in column).
static LAST_WORKER_PANE: Mutex<Option<String>> = Mutex::new(None);

/// Terminal adapter for Ghostty 1.3+ on macOS.
///
/// Uses Ghostty's native AppleScript scripting API to create split panes
/// and send commands — no keystroke simulation, no clipboard hacks.
///
/// Inspired by [gx-ghostty](https://github.com/ashsidhu/gx-ghostty).
///
/// Layout:
/// ```text
/// ┌──────────┬──────────┐
/// │          │ Worker 1 │
/// │   CC     ├──────────┤
/// │ (main)   │ Worker 2 │
/// └──────────┴──────────┘
/// ```
pub struct GhosttyTerminal {
    _config: TerminalConfig,
    /// The terminal UUID where CC (main agent) is running.
    /// Captured at construction time so splits always happen in the
    /// correct window, even when the daemon runs in the background.
    origin_terminal_id: String,
}

impl GhosttyTerminal {
    /// Create a new GhosttyTerminal.
    ///
    /// Reads the origin terminal UUID from `.orca/origin_terminal_id`
    /// (written by `orca daemon start` before entering the async runtime).
    /// This ensures splits always happen in the window where the user started the daemon.
    pub fn new_with_project_dir(config: &TerminalConfig, project_dir: &std::path::Path) -> Self {
        let origin_file = project_dir.join(".orca/origin_terminal_id");
        let origin_id = std::fs::read_to_string(&origin_file)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        if origin_id.is_empty() {
            tracing::warn!("no origin terminal UUID found — run `orca daemon start` from Ghostty");
        } else {
            tracing::info!(terminal_id = %origin_id, "loaded origin Ghostty terminal from file");
        }

        Self {
            _config: config.clone(),
            origin_terminal_id: origin_id,
        }
    }

    pub fn new(config: &TerminalConfig) -> Self {
        Self {
            _config: config.clone(),
            origin_terminal_id: String::new(),
        }
    }
}

#[async_trait]
impl Terminal for GhosttyTerminal {
    async fn create_pane(&self, cmd: &str, label: &str) -> Result<String> {
        // Decide split target and direction:
        // - First worker: split RIGHT from origin (creates worker column)
        // - Subsequent workers: split DOWN from last worker pane (stack in column)
        let last_pane = LAST_WORKER_PANE.lock().unwrap().clone();
        let (parent_id, direction) = match last_pane {
            Some(last) => (last, "down"),
            None => {
                let origin = if self.origin_terminal_id.is_empty() {
                    get_focused_terminal().await?
                } else {
                    self.origin_terminal_id.clone()
                };
                (origin, "right")
            }
        };

        let new_id = split_terminal(&parent_id, direction)
            .await
            .with_context(|| format!("failed to split ghostty terminal {direction}"))?;

        // Track this pane so the next worker splits down from it.
        {
            let mut last = LAST_WORKER_PANE.lock().unwrap();
            *last = Some(new_id.clone());
        }

        tracing::info!(
            pane_id = %new_id,
            label = %label,
            direction,
            "created ghostty split pane"
        );

        // Send command + newline to the new pane (by UUID, no focus change).
        let cmd_with_newline = format!("{}\n", cmd);
        input_text(&new_id, &cmd_with_newline).await?;

        Ok(new_id)
    }

    async fn close_pane(&self, pane_id: &str) -> Result<()> {
        close_terminal(pane_id).await
    }

    async fn focus_pane(&self, pane_id: &str) -> Result<()> {
        focus_terminal(pane_id).await
    }

    async fn send_text(&self, pane_id: &str, text: &str) -> Result<()> {
        input_text(pane_id, text).await
    }

    fn name(&self) -> &str {
        "ghostty"
    }
}

// ---------------------------------------------------------------------------
// Ghostty AppleScript helpers (1.3+ scripting dictionary)
// Reference: https://github.com/ashsidhu/gx-ghostty
// ---------------------------------------------------------------------------

async fn get_focused_terminal() -> Result<String> {
    let script = r#"tell application "Ghostty" to get id of focused terminal of selected tab of front window"#;
    run_osascript(script).await
}

async fn split_terminal(terminal_id: &str, direction: &str) -> Result<String> {
    let script = format!(
        r#"tell application "Ghostty"
    set newTerm to split terminal id "{}" direction {}
    get id of newTerm
end tell"#,
        terminal_id, direction
    );
    run_osascript(&script).await
}

async fn input_text(terminal_id: &str, text: &str) -> Result<()> {
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"tell application "Ghostty" to input text "{}" to terminal id "{}""#,
        escaped, terminal_id
    );
    run_osascript(&script).await?;
    Ok(())
}

async fn focus_terminal(terminal_id: &str) -> Result<()> {
    let script = format!(
        r#"tell application "Ghostty"
    activate
    set focused of terminal id "{}" to true
end tell"#,
        terminal_id
    );
    run_osascript(&script).await?;
    Ok(())
}

async fn close_terminal(terminal_id: &str) -> Result<()> {
    let script = format!(
        r#"tell application "Ghostty" to close terminal id "{}""#,
        terminal_id
    );
    run_osascript(&script).await?;
    Ok(())
}

async fn run_osascript(script: &str) -> Result<String> {
    let output = tokio::process::Command::new("osascript")
        .args(["-e", script])
        .output()
        .await
        .context("failed to run osascript")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Ghostty AppleScript failed (requires Ghostty 1.3+). Error: {}",
            stderr.trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
