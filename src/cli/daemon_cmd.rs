use clap::Subcommand;

/// Daemon management subcommands.
#[derive(Debug, Subcommand)]
pub enum DaemonAction {
    /// Start the daemon process
    Start {
        /// Run in foreground instead of daemonizing
        #[arg(long)]
        foreground: bool,
    },

    /// Stop the running daemon
    Stop,

    /// Check daemon status
    Status,
}
