use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

use super::Terminal;

/// Terminal adapter that creates split panes via Ghostty's IPC.
///
/// Uses `ghostty +action new_split_right` to create splits in the
/// focused Ghostty window, then sends the command to the new pane.
pub struct GhosttyTerminal;

#[async_trait]
impl Terminal for GhosttyTerminal {
    async fn create_pane(&self, cmd: &str, label: &str) -> Result<String> {
        let pane_id = Uuid::new_v4().to_string();
        tracing::info!(pane_id = %pane_id, label = %label, "creating ghostty split pane");

        // Create a new split pane to the right in the focused window
        let status = tokio::process::Command::new("ghostty")
            .args(["+action", "new_split_right"])
            .status()
            .await?;

        if !status.success() {
            anyhow::bail!("ghostty +action new_split_right failed");
        }

        // Brief pause to let the split initialize
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Send the command to the newly focused split pane via text injection
        // ghostty +action write_screen_text sends text to the focused surface
        let cmd_with_newline = format!("{}\n", cmd);
        let status = tokio::process::Command::new("ghostty")
            .args(["+action", "text", &cmd_with_newline])
            .status()
            .await?;

        if !status.success() {
            // Fallback: if text action is not available, log instruction
            tracing::warn!(
                "ghostty +action text failed — manually run in the new split: {}",
                cmd
            );
        }

        Ok(pane_id)
    }

    async fn close_pane(&self, pane_id: &str) -> Result<()> {
        tracing::info!(pane_id = %pane_id, "ghostty: close_pane — split will close when process exits");
        Ok(())
    }

    async fn focus_pane(&self, pane_id: &str) -> Result<()> {
        // Ghostty supports goto_split:next/previous/up/down/left/right
        // but we don't track pane positions, so this is best-effort
        tracing::info!(pane_id = %pane_id, "ghostty: focus_pane not fully supported — use keybindings to navigate splits");
        Ok(())
    }

    fn name(&self) -> &str {
        "ghostty"
    }
}
