use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

use super::Terminal;

/// Number of panes created so far. First pane splits right (creating the
/// worker column), subsequent panes split down (stacking inside it).
static PANE_COUNT: AtomicU32 = AtomicU32::new(0);

/// Terminal adapter for Ghostty on macOS.
///
/// Ghostty does not expose an external IPC for split management.
/// We use `osascript` to send the keybinding (Cmd+D / Cmd+Shift+D)
/// to the frontmost Ghostty window, then type the command into the
/// newly focused pane.
///
/// Layout strategy (similar to tmux):
/// ```text
/// ┌──────────┬──────────┐
/// │          │ Worker 1 │
/// │   CC     ├──────────┤
/// │ (main)   │ Worker 2 │
/// │          ├──────────┤
/// │          │ Worker 3 │
/// └──────────┴──────────┘
/// ```
/// - First worker: split right (Cmd+D) — creates the worker column
/// - Subsequent workers: split down (Cmd+Shift+D) — stack in the column
pub struct GhosttyTerminal;

#[async_trait]
impl Terminal for GhosttyTerminal {
    async fn create_pane(&self, cmd: &str, label: &str) -> Result<String> {
        let pane_id = Uuid::new_v4().to_string();
        let count = PANE_COUNT.fetch_add(1, Ordering::Relaxed);

        tracing::info!(pane_id = %pane_id, label = %label, pane_num = count, "creating ghostty split pane");

        if count == 0 {
            // First worker: split right (Cmd+D) to create worker column
            send_keystroke("d", false).await?;
        } else {
            // Subsequent workers: split down (Cmd+Shift+D) within worker column
            send_keystroke("d", true).await?;
        }

        // Wait for the split to initialize and receive focus
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // Type the command into the new pane
        send_text(cmd).await?;

        // Press Enter to execute
        send_keystroke("return", false).await?;

        // Wait a moment, then focus back to the original (CC) pane
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        Ok(pane_id)
    }

    async fn close_pane(&self, pane_id: &str) -> Result<()> {
        tracing::info!(pane_id = %pane_id, "ghostty: split will close when process exits");
        Ok(())
    }

    async fn focus_pane(&self, pane_id: &str) -> Result<()> {
        tracing::info!(pane_id = %pane_id, "ghostty: use Cmd+[ / Cmd+] to navigate splits");
        Ok(())
    }

    fn name(&self) -> &str {
        "ghostty"
    }
}

/// Send a keystroke to the frontmost Ghostty window via osascript.
/// If `with_shift` is true, adds the shift modifier.
async fn send_keystroke(key: &str, with_shift: bool) -> Result<()> {
    let modifier = if with_shift {
        "command down, shift down"
    } else {
        "command down"
    };

    let script = format!(
        r#"tell application "System Events"
    tell process "Ghostty"
        key code {key_code} using {{{modifier}}}
    end tell
end tell"#,
        key_code = key_name_to_code(key),
        modifier = modifier
    );

    let status = tokio::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .await?;

    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        tracing::warn!("osascript keystroke failed: {}", stderr);
    }

    Ok(())
}

/// Type text into the focused pane using osascript keystroke events.
async fn send_text(text: &str) -> Result<()> {
    // Escape for AppleScript string
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");

    let script = format!(
        r#"tell application "System Events"
    tell process "Ghostty"
        keystroke "{}"
    end tell
end tell"#,
        escaped
    );

    let status = tokio::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .await?;

    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        tracing::warn!("osascript text input failed: {}", stderr);
    }

    Ok(())
}

/// Map key names to macOS virtual key codes.
fn key_name_to_code(key: &str) -> u8 {
    match key {
        "d" => 2,
        "return" => 36,
        _ => 0,
    }
}
