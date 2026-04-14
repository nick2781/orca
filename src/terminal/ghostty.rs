use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use async_trait::async_trait;

use super::Terminal;
use crate::config::TerminalConfig;

/// Whether the first worker pane has been created (splits right).
/// Subsequent panes split down to stack in the worker column.
static FIRST_SPLIT_DONE: AtomicBool = AtomicBool::new(false);

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
    pub fn new(config: &TerminalConfig) -> Self {
        // Capture the focused terminal NOW (at daemon startup time,
        // while the user's Ghostty window is still in front).
        let origin_id = tokio::runtime::Handle::current()
            .block_on(get_focused_terminal())
            .unwrap_or_default();

        if origin_id.is_empty() {
            tracing::warn!("could not capture Ghostty terminal UUID at startup — splits may go to wrong window");
        } else {
            tracing::info!(terminal_id = %origin_id, "captured origin Ghostty terminal");
        }

        Self {
            _config: config.clone(),
            origin_terminal_id: origin_id,
        }
    }
}

#[async_trait]
impl Terminal for GhosttyTerminal {
    async fn create_pane(&self, cmd: &str, label: &str) -> Result<String> {
        // Use the origin terminal (captured at startup) as the split target.
        let parent_id = if self.origin_terminal_id.is_empty() {
            // Fallback: try to get current focused terminal.
            get_focused_terminal().await?
        } else {
            self.origin_terminal_id.clone()
        };

        // First worker splits right (creates worker column),
        // subsequent workers split down (stack in the column).
        let direction = if FIRST_SPLIT_DONE
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            "right"
        } else {
            "down"
        };

        let new_id = split_terminal(&parent_id, direction)
            .await
            .with_context(|| format!("failed to split ghostty terminal {direction}"))?;

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
        tracing::info!(pane_id = %pane_id, "ghostty: use Cmd+[ / Cmd+] to navigate splits");
        Ok(())
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
