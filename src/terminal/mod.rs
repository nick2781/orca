pub mod ghostty;
pub mod iterm;
pub mod manual;

use anyhow::Result;
use async_trait::async_trait;

/// Trait representing a terminal emulator that can manage panes.
///
/// Implementations control how worker processes are visually presented
/// to the user -- each worker gets its own terminal pane.
#[async_trait]
pub trait Terminal: Send + Sync {
    /// Create a new pane running the given command.
    /// Returns a pane identifier that can be used with other methods.
    async fn create_pane(&self, cmd: &str, label: &str) -> Result<String>;

    /// Close a previously created pane.
    async fn close_pane(&self, pane_id: &str) -> Result<()>;

    /// Bring focus to a specific pane.
    async fn focus_pane(&self, pane_id: &str) -> Result<()>;

    /// Return the name of this terminal provider.
    fn name(&self) -> &str;
}

/// Create a terminal adapter for the given provider name.
pub fn create_terminal(
    provider: &str,
    config: &crate::config::TerminalConfig,
) -> Box<dyn Terminal> {
    match provider {
        "ghostty" => Box::new(ghostty::GhosttyTerminal::new(config)),
        "iterm2" => Box::new(iterm::ItermTerminal),
        _ => Box::new(manual::ManualTerminal),
    }
}
