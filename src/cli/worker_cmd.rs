use clap::Subcommand;

/// Worker management subcommands.
#[derive(Debug, Subcommand)]
pub enum WorkerAction {
    /// List all workers
    List,

    /// Connect a new worker
    Connect {
        /// Worker ID to assign
        #[arg(long)]
        id: Option<String>,

        /// Automatically configure the worker
        #[arg(long)]
        auto: bool,
    },

    /// Kill a worker process
    Kill {
        /// Worker ID
        id: String,
    },
}
