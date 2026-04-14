use clap::Subcommand;

/// Daemon management subcommands.
#[derive(Debug, Subcommand)]
pub enum DaemonAction {
    /// Start the daemon process
    Start {
        /// Run in foreground instead of daemonizing
        #[arg(long)]
        foreground: bool,

        /// Origin terminal UUID for split pane targeting.
        /// When set, splits always happen in this terminal's window.
        #[arg(long)]
        origin_terminal: Option<String>,
    },

    /// Stop the running daemon
    Stop,

    /// Check daemon status
    Status,
}
