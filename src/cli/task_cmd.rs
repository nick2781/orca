use clap::Subcommand;

/// Task management subcommands.
#[derive(Debug, Subcommand)]
pub enum TaskAction {
    /// List tasks (optionally filtered by state)
    List {
        /// Filter by task state (pending, running, done, review, accepted, etc.)
        #[arg(long)]
        filter: Option<String>,
    },

    /// Show detailed info for a task
    Detail {
        /// Task ID
        id: String,
    },

    /// Cancel a running or assigned task
    Cancel {
        /// Task ID
        id: String,
    },

    /// Retry a failed or rejected task
    Retry {
        /// Task ID
        id: String,
    },
}
