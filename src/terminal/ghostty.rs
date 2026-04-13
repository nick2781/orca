use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

use super::Terminal;
use crate::config::TerminalConfig;

/// Number of panes created so far. First pane splits right, subsequent split down.
static PANE_COUNT: AtomicU32 = AtomicU32::new(0);

/// Terminal adapter for Ghostty on macOS.
///
/// Uses `osascript` to send keybindings to the Ghostty process.
/// Keybindings are configurable via `orca.toml [terminal]`.
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
pub struct GhosttyTerminal {
    split_right_key: u8,
    split_down_key: u8,
    split_down_shift: bool,
}

impl GhosttyTerminal {
    pub fn new(config: &TerminalConfig) -> Self {
        Self {
            split_right_key: config.split_right_key,
            split_down_key: config.split_down_key,
            split_down_shift: config.split_down_shift,
        }
    }
}

#[async_trait]
impl Terminal for GhosttyTerminal {
    async fn create_pane(&self, cmd: &str, label: &str) -> Result<String> {
        let pane_id = Uuid::new_v4().to_string();
        let count = PANE_COUNT.fetch_add(1, Ordering::Relaxed);

        tracing::info!(pane_id = %pane_id, label = %label, pane_num = count, "creating ghostty split pane");

        // First worker: split right. Subsequent: split down in worker column.
        if count == 0 {
            send_keystroke(self.split_right_key, false).await?;
        } else {
            send_keystroke(self.split_down_key, self.split_down_shift).await?;
        }

        // Wait for split to initialize
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Type the command into the new pane
        send_text(cmd).await?;

        // Press Enter
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        send_keystroke(36, false).await?; // Return key

        Ok(pane_id)
    }

    async fn close_pane(&self, pane_id: &str) -> Result<()> {
        tracing::info!(pane_id = %pane_id, "ghostty: split closes when process exits");
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

/// Send a keystroke via osascript to the frontmost Ghostty window.
async fn send_keystroke(key_code: u8, with_shift: bool) -> Result<()> {
    let modifier = if with_shift {
        "command down, shift down"
    } else {
        "command down"
    };

    let script = format!(
        r#"tell application "System Events"
    tell process "Ghostty"
        key code {} using {{{}}}
    end tell
end tell"#,
        key_code, modifier
    );

    let output = tokio::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!(
            "osascript keystroke (key_code={}) failed: {}",
            key_code,
            stderr
        );
        anyhow::bail!(
            "Failed to send keystroke to Ghostty. \
             Make sure 'System Events' has Accessibility permission. \
             System Settings → Privacy & Security → Accessibility → enable your terminal. \
             Error: {}",
            stderr
        );
    }

    Ok(())
}

/// Type text into the focused pane via osascript.
async fn send_text(text: &str) -> Result<()> {
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");

    let script = format!(
        r#"tell application "System Events"
    tell process "Ghostty"
        keystroke "{}"
    end tell
end tell"#,
        escaped
    );

    let output = tokio::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!("osascript text input failed: {}", stderr);
        anyhow::bail!("Failed to type text in Ghostty pane. Error: {}", stderr);
    }

    Ok(())
}
