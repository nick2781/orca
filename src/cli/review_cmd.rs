use clap::Subcommand;

/// Review subcommands for accepting or rejecting tasks.
#[derive(Debug, Subcommand)]
pub enum ReviewAction {
    /// Accept a task that is in review state
    Accept {
        /// Task ID
        task_id: String,
    },

    /// Reject a task that is in review state
    Reject {
        /// Task ID
        task_id: String,

        /// Feedback explaining the rejection
        #[arg(long)]
        feedback: Option<String>,
    },
}
