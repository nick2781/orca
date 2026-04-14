use anyhow::Result;
use async_trait::async_trait;

use super::Terminal;

/// Fallback terminal adapter that prints instructions for the user
/// to run commands manually in a separate terminal.
pub struct ManualTerminal;

#[async_trait]
impl Terminal for ManualTerminal {
    async fn create_pane(&self, cmd: &str, label: &str) -> Result<String> {
        let border = "─".repeat(60);
        println!("┌{border}┐");
        println!("│ {:^60} │", format!("Worker: {label}"));
        println!("├{border}┤");
        println!(
            "│ {:60} │",
            "Please run the following command in a new terminal:"
        );
        println!("│ {:60} │", "");
        println!("│ {:60} │", format!("  {cmd}"));
        println!("│ {:60} │", "");
        println!("└{border}┘");

        Ok(label.to_string())
    }

    async fn close_pane(&self, pane_id: &str) -> Result<()> {
        println!("[orca] Worker finished: {pane_id}");
        Ok(())
    }

    async fn focus_pane(&self, pane_id: &str) -> Result<()> {
        println!("[orca] Check worker pane: {pane_id}");
        Ok(())
    }

    async fn send_text(&self, _pane_id: &str, _text: &str) -> Result<()> {
        Ok(())
    }

    fn name(&self) -> &str {
        "manual"
    }
}
