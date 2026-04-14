use anyhow::Result;
use async_trait::async_trait;

use super::Terminal;
use crate::config::TerminalConfig;

/// Terminal adapter for Ghostty.
///
/// STATUS: Ghostty does not yet have an external API for programmatic split
/// pane creation. The internal `new_split` action exists but cannot be invoked
/// from CLI or IPC. See: <https://github.com/ghostty-org/ghostty/discussions/2353>
///
/// We are planning to contribute a PR to Ghostty to add CLI split-pane support.
/// Until then, this adapter falls back to printing the command for the user
/// to manually execute in a Ghostty split pane (Cmd+D to split, paste command).
///
/// Future: once Ghostty ships a CLI like `ghostty +action new_split -- cmd`,
/// this adapter will use it directly.
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
        let pane_id = uuid::Uuid::new_v4().to_string();

        // Ghostty has no external API for splits yet.
        // Print clear instructions for the user.
        println!();
        println!("╭─ orca: new worker pane ─────────────────────────────────╮");
        println!("│ {}", label);
        println!("├─────────────────────────────────────────────────────────┤");
        println!("│ Ghostty does not yet support programmatic split panes. │");
        println!("│ Please split manually:                                 │");
        println!("│   1. Press Cmd+D (split right) or Cmd+Shift+D (down)   │");
        println!("│   2. Run the following command in the new pane:         │");
        println!("├─────────────────────────────────────────────────────────┤");
        println!("│ {}", cmd);
        println!("╰─────────────────────────────────────────────────────────╯");
        println!();

        tracing::info!(pane_id = %pane_id, label = %label, "ghostty: manual split required (no CLI API yet)");

        Ok(pane_id)
    }

    async fn close_pane(&self, _pane_id: &str) -> Result<()> {
        Ok(())
    }

    async fn focus_pane(&self, _pane_id: &str) -> Result<()> {
        Ok(())
    }

    fn name(&self) -> &str {
        "ghostty"
    }
}
