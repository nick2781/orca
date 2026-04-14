use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

use super::Terminal;

/// Terminal adapter that creates panes via iTerm2 AppleScript integration.
pub struct ItermTerminal;

#[async_trait]
impl Terminal for ItermTerminal {
    async fn create_pane(&self, cmd: &str, label: &str) -> Result<String> {
        let pane_id = Uuid::new_v4().to_string();
        let escaped_cmd = cmd.replace('"', "\\\"");

        let script = format!(
            r#"tell application "iTerm2"
    tell current session of current window
        set newSession to (split horizontally with default profile)
        tell newSession
            write text "{escaped_cmd}"
        end tell
    end tell
end tell"#
        );

        tracing::info!(pane_id = %pane_id, label = %label, "creating iterm2 pane");

        tokio::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
            .await?;

        Ok(pane_id)
    }

    async fn close_pane(&self, _pane_id: &str) -> Result<()> {
        Ok(())
    }

    async fn focus_pane(&self, _pane_id: &str) -> Result<()> {
        Ok(())
    }

    async fn send_text(&self, _pane_id: &str, _text: &str) -> Result<()> {
        Ok(())
    }

    fn name(&self) -> &str {
        "iterm2"
    }
}
