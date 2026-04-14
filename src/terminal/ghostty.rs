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
/// Inspired by [gx-ghostty](https://github.com/ashsidhu/gx-ghostty) which
/// demonstrated that Ghostty 1.3+ exposes a proper scripting dictionary
/// with `split`, `input text`, `send key`, and UUID-based terminal addressing.
///
/// Layout:
/// ```text
/// ┌──────────┬──────────┐
/// │          │ Worker 1 │
/// │   CC     ├──────────┤
/// │ (main)   │ Worker 2 │
/// │          ├──────────┤
/// │          │ Worker 3 │
/// └──────────┴──────────┘
/// ```
/// First worker splits right (creates worker column).
/// Subsequent workers split down (stack in the column).
pub struct GhosttyTerminal {
    _config: TerminalConfig,
}

impl GhosttyTerminal {
    pub fn new(config: &TerminalConfig) -> Self {
        Self {
            _config: config.clone(),
        }
    }
}

#[async_trait]
impl Terminal for GhosttyTerminal {
    async fn create_pane(&self, cmd: &str, label: &str) -> Result<String> {
        // Get the currently focused terminal UUID.
        let focused_id = get_focused_terminal()
            .await
            .context("failed to get focused Ghostty terminal")?;

        // Decide split direction: first worker goes right, rest go down.
        let direction = if FIRST_SPLIT_DONE
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            "right"
        } else {
            "down"
        };

        // Split and get the new terminal's UUID.
        let new_id = split_terminal(&focused_id, direction)
            .await
            .with_context(|| format!("failed to split ghostty terminal {direction}"))?;

        tracing::info!(
            pane_id = %new_id,
            label = %label,
            direction,
            "created ghostty split pane"
        );

        // Send the command to the new pane (by UUID, no focus required).
        input_text(&new_id, cmd).await?;
        send_key(&new_id, "return").await?;

        Ok(new_id)
    }

    async fn close_pane(&self, pane_id: &str) -> Result<()> {
        close_terminal(pane_id).await
    }

    async fn focus_pane(&self, pane_id: &str) -> Result<()> {
        // Ghostty scripting API doesn't have a direct "focus terminal" command.
        // The user can navigate splits with Cmd+[ / Cmd+].
        tracing::info!(pane_id = %pane_id, "ghostty: use Cmd+[ / Cmd+] to navigate splits");
        Ok(())
    }

    fn name(&self) -> &str {
        "ghostty"
    }
}

// ---------------------------------------------------------------------------
// Ghostty AppleScript helpers
//
// These use Ghostty 1.3+'s native scripting dictionary.
// Reference: https://github.com/ashsidhu/gx-ghostty
// ---------------------------------------------------------------------------

/// Get the UUID of the currently focused terminal.
async fn get_focused_terminal() -> Result<String> {
    let script = r#"tell application "Ghostty" to get id of focused terminal of selected tab of front window"#;
    run_osascript(script).await
}

/// Split a terminal and return the new terminal's UUID.
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

/// Send text to a specific terminal (by UUID, no focus change).
async fn input_text(terminal_id: &str, text: &str) -> Result<()> {
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"tell application "Ghostty" to input text "{}" to terminal id "{}""#,
        escaped, terminal_id
    );
    run_osascript(&script).await?;
    Ok(())
}

/// Send a key event to a specific terminal.
async fn send_key(terminal_id: &str, key: &str) -> Result<()> {
    let script = format!(
        r#"tell application "Ghostty" to send key "{}" to terminal id "{}""#,
        key, terminal_id
    );
    run_osascript(&script).await?;
    Ok(())
}

/// Close a terminal pane.
async fn close_terminal(terminal_id: &str) -> Result<()> {
    let script = format!(
        r#"tell application "Ghostty" to close terminal id "{}""#,
        terminal_id
    );
    run_osascript(&script).await?;
    Ok(())
}

/// Execute AppleScript and return trimmed stdout.
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
