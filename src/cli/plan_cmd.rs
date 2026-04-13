use clap::Subcommand;

/// Plan management subcommands.
#[derive(Debug, Subcommand)]
pub enum PlanAction {
    /// Submit a plan file for execution
    Submit {
        /// Path to the plan JSON file
        file: String,
    },
}
