use clap::Subcommand;

/// Setup subcommands for integrations.
#[derive(Debug, Subcommand)]
pub enum SetupAction {
    /// Print MCP server configuration for ~/.claude/settings.json
    Mcp,
}
