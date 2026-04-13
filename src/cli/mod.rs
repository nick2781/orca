pub mod daemon_cmd;
pub mod plan_cmd;
pub mod review_cmd;
pub mod setup_cmd;
pub mod task_cmd;
pub mod worker_cmd;

use clap::Subcommand;

use self::daemon_cmd::DaemonAction;
use self::plan_cmd::PlanAction;
use self::review_cmd::ReviewAction;
use self::setup_cmd::SetupAction;
use self::task_cmd::TaskAction;
use self::worker_cmd::WorkerAction;

/// Top-level subcommands for the Orca CLI.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Start, stop, or check the daemon
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },

    /// List, inspect, cancel, or retry tasks
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },

    /// List, connect, or kill workers
    Worker {
        #[command(subcommand)]
        action: WorkerAction,
    },

    /// Submit a plan for execution
    Plan {
        #[command(subcommand)]
        action: PlanAction,
    },

    /// Accept or reject a completed task
    Review {
        #[command(subcommand)]
        action: ReviewAction,
    },

    /// Merge accepted task branches
    Merge {
        /// Task IDs to merge
        task_ids: Vec<String>,

        /// Merge all accepted tasks
        #[arg(long)]
        all_accepted: bool,
    },

    /// List or decide on escalations
    Escalation {
        #[command(subcommand)]
        action: EscalationAction,
    },

    /// Initialize orca.toml and .orca/ directory
    Init,

    /// Setup integrations (e.g. MCP)
    Setup {
        #[command(subcommand)]
        action: SetupAction,
    },

    /// Show resolved configuration
    Config,

    /// Run the MCP server (placeholder)
    McpServer,
}

/// Escalation subcommands.
#[derive(Debug, Subcommand)]
pub enum EscalationAction {
    /// List pending escalations
    List,

    /// Decide on an escalation
    Decide {
        /// Escalation ID
        id: String,

        /// Choice to apply
        #[arg(long)]
        choice: String,
    },
}
