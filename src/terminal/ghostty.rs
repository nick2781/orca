use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

use super::Terminal;

/// Terminal adapter that spawns panes via the Ghostty terminal emulator.
pub struct GhosttyTerminal;

#[async_trait]
impl Terminal for GhosttyTerminal {
    async fn create_pane(&self, cmd: &str, label: &str) -> Result<String> {
        let pane_id = Uuid::new_v4().to_string();
        tracing::info!(pane_id = %pane_id, label = %label, "creating ghostty pane");

        tokio::process::Command::new("ghostty")
            .arg("-e")
            .arg(cmd)
            .spawn()?;

        Ok(pane_id)
    }

    async fn close_pane(&self, pane_id: &str) -> Result<()> {
        tracing::warn!(pane_id = %pane_id, "close_pane not supported for ghostty");
        Ok(())
    }

    async fn focus_pane(&self, pane_id: &str) -> Result<()> {
        tracing::warn!(pane_id = %pane_id, "focus_pane not supported for ghostty");
        Ok(())
    }

    fn name(&self) -> &str {
        "ghostty"
    }
}
