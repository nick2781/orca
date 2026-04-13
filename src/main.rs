use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use serde_json::json;

use orca::cli::{Commands, EscalationAction};
use orca::cli::daemon_cmd::DaemonAction;
use orca::cli::task_cmd::TaskAction;
use orca::cli::worker_cmd::WorkerAction;
use orca::cli::plan_cmd::PlanAction;
use orca::cli::review_cmd::ReviewAction;
use orca::cli::setup_cmd::SetupAction;
use orca::config::Config;
use orca::daemon::Daemon;
use orca::daemon::server::IpcClient;
use orca::protocol::{RpcRequest, RpcResponse};

#[derive(Parser)]
#[command(name = "orca", version, about = "Multi-agent orchestrator: Claude Code brain + Codex workers")]
struct Cli {
    /// Project directory (defaults to current directory)
    #[arg(long, global = true)]
    project_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let project_dir = cli.project_dir
        .unwrap_or_else(|| std::env::current_dir().expect("failed to get current directory"));

    let config = Config::load(&project_dir)
        .unwrap_or_else(|_| Config::default());

    let command = match cli.command {
        Some(cmd) => cmd,
        None => {
            println!("orca v{}", env!("CARGO_PKG_VERSION"));
            println!("Run `orca --help` for usage.");
            return Ok(());
        }
    };

    match command {
        Commands::Daemon { action } => handle_daemon(action, config, &project_dir).await,
        Commands::Task { action } => handle_task(action, &config, &project_dir).await,
        Commands::Worker { action } => handle_worker(action, &config, &project_dir).await,
        Commands::Plan { action } => handle_plan(action, &config, &project_dir).await,
        Commands::Review { action } => handle_review(action, &config, &project_dir).await,
        Commands::Merge { task_ids, all_accepted } => {
            handle_merge(task_ids, all_accepted, &config, &project_dir).await
        }
        Commands::Escalation { action } => {
            handle_escalation(action, &config, &project_dir).await
        }
        Commands::Init => handle_init(&project_dir),
        Commands::Setup { action } => handle_setup(action),
        Commands::Config => handle_config(&config),
        Commands::McpServer => {
            println!("MCP server not implemented yet");
            Ok(())
        }
    }
}

// -- Daemon ------------------------------------------------------------------

async fn handle_daemon(action: DaemonAction, config: Config, project_dir: &PathBuf) -> Result<()> {
    match action {
        DaemonAction::Start { foreground: _ } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| {
                            tracing_subscriber::EnvFilter::new(&config.daemon.log_level)
                        }),
                )
                .init();

            let daemon = Daemon::new(config, project_dir.clone())?;
            daemon.run().await
        }
        DaemonAction::Stop => {
            let socket_path = config.socket_path(project_dir);
            if socket_path.exists() {
                std::fs::remove_file(&socket_path)
                    .with_context(|| format!("failed to remove socket {}", socket_path.display()))?;
                println!("Daemon stopped (socket removed: {})", socket_path.display());
            } else {
                println!("No daemon socket found at {}", socket_path.display());
            }
            Ok(())
        }
        DaemonAction::Status => {
            let socket_path = config.socket_path(project_dir);
            match IpcClient::connect(&socket_path).await {
                Ok(mut client) => {
                    let req = RpcRequest::new("ping", json!({}));
                    match client.call(&req).await {
                        Ok(resp) if resp.error.is_none() => {
                            println!("Daemon is running (socket: {})", socket_path.display());
                        }
                        Ok(resp) => {
                            println!("Daemon responded with error: {:?}", resp.error);
                        }
                        Err(e) => {
                            println!("Daemon socket exists but not responding: {e}");
                        }
                    }
                }
                Err(_) => {
                    println!("Daemon is not running (no socket at {})", socket_path.display());
                }
            }
            Ok(())
        }
    }
}

// -- Task --------------------------------------------------------------------

async fn handle_task(action: TaskAction, config: &Config, project_dir: &PathBuf) -> Result<()> {
    let socket_path = config.socket_path(project_dir);

    match action {
        TaskAction::List { filter } => {
            let mut params = json!({});
            if let Some(f) = filter {
                params["state"] = json!(f);
            }
            let resp = ipc_call(&socket_path, "orca_status", params).await?;
            print_response(&resp);
        }
        TaskAction::Detail { id } => {
            let resp = ipc_call(&socket_path, "orca_task_detail", json!({"task_id": id})).await?;
            print_response(&resp);
        }
        TaskAction::Cancel { id } => {
            let resp = ipc_call(&socket_path, "orca_cancel", json!({"task_id": id})).await?;
            print_response(&resp);
        }
        TaskAction::Retry { id } => {
            // Retry transitions a rejected task back to pending
            let resp = ipc_call(&socket_path, "orca_review", json!({"task_id": id, "verdict": "retry"})).await?;
            print_response(&resp);
        }
    }

    Ok(())
}

// -- Worker ------------------------------------------------------------------

async fn handle_worker(action: WorkerAction, config: &Config, project_dir: &PathBuf) -> Result<()> {
    let socket_path = config.socket_path(project_dir);

    match action {
        WorkerAction::List => {
            let resp = ipc_call(&socket_path, "orca_worker_list", json!({})).await?;
            print_response(&resp);
        }
        WorkerAction::Connect { id, auto } => {
            let params = json!({
                "worker_id": id,
                "auto": auto,
            });
            let resp = ipc_call(&socket_path, "orca_worker_connect", params).await?;
            print_response(&resp);
        }
        WorkerAction::Kill { id } => {
            let resp = ipc_call(&socket_path, "orca_worker_kill", json!({"worker_id": id})).await?;
            print_response(&resp);
        }
    }

    Ok(())
}

// -- Plan --------------------------------------------------------------------

async fn handle_plan(action: PlanAction, config: &Config, project_dir: &PathBuf) -> Result<()> {
    let socket_path = config.socket_path(project_dir);

    match action {
        PlanAction::Submit { file } => {
            let contents = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read plan file: {file}"))?;
            let plan: serde_json::Value = serde_json::from_str(&contents)
                .with_context(|| format!("failed to parse plan file as JSON: {file}"))?;
            let resp = ipc_call(&socket_path, "orca_plan", plan).await?;
            print_response(&resp);
        }
    }

    Ok(())
}

// -- Review ------------------------------------------------------------------

async fn handle_review(action: ReviewAction, config: &Config, project_dir: &PathBuf) -> Result<()> {
    let socket_path = config.socket_path(project_dir);

    match action {
        ReviewAction::Accept { task_id } => {
            let resp = ipc_call(
                &socket_path,
                "orca_review",
                json!({"task_id": task_id, "verdict": "accepted"}),
            ).await?;
            print_response(&resp);
        }
        ReviewAction::Reject { task_id, feedback } => {
            let resp = ipc_call(
                &socket_path,
                "orca_review",
                json!({"task_id": task_id, "verdict": "rejected", "feedback": feedback}),
            ).await?;
            print_response(&resp);
        }
    }

    Ok(())
}

// -- Merge -------------------------------------------------------------------

async fn handle_merge(
    task_ids: Vec<String>,
    all_accepted: bool,
    config: &Config,
    project_dir: &PathBuf,
) -> Result<()> {
    let socket_path = config.socket_path(project_dir);

    if all_accepted {
        // Query status to find accepted tasks, then merge each
        let status_resp = ipc_call(&socket_path, "orca_status", json!({"state": "accepted"})).await?;
        if let Some(result) = &status_resp.result {
            if let Some(tasks) = result.get("tasks").and_then(|t| t.as_array()) {
                for task in tasks {
                    if let Some(tid) = task.get("id").and_then(|v| v.as_str()) {
                        let resp = ipc_call(
                            &socket_path,
                            "orca_merge",
                            json!({"task_id": tid}),
                        ).await?;
                        print_response(&resp);
                    }
                }
                if tasks.is_empty() {
                    println!("No accepted tasks to merge.");
                }
            }
        } else if let Some(err) = &status_resp.error {
            eprintln!("Error: {} (code {})", err.message, err.code);
        }
    } else if task_ids.is_empty() {
        println!("No task IDs specified. Use --all-accepted or provide task IDs.");
    } else {
        for tid in &task_ids {
            let resp = ipc_call(&socket_path, "orca_merge", json!({"task_id": tid})).await?;
            print_response(&resp);
        }
    }

    Ok(())
}

// -- Escalation --------------------------------------------------------------

async fn handle_escalation(
    action: EscalationAction,
    config: &Config,
    project_dir: &PathBuf,
) -> Result<()> {
    let socket_path = config.socket_path(project_dir);

    match action {
        EscalationAction::List => {
            let resp = ipc_call(&socket_path, "orca_status", json!({})).await?;
            // Extract just the escalations from the status response
            if let Some(result) = &resp.result {
                if let Some(escalations) = result.get("escalations") {
                    println!("{}", serde_json::to_string_pretty(escalations)?);
                } else {
                    println!("[]");
                }
            } else if let Some(err) = &resp.error {
                eprintln!("Error: {} (code {})", err.message, err.code);
            }
        }
        EscalationAction::Decide { id, choice } => {
            let resp = ipc_call(
                &socket_path,
                "orca_decide",
                json!({"escalation_id": id, "decision": choice}),
            ).await?;
            print_response(&resp);
        }
    }

    Ok(())
}

// -- Init --------------------------------------------------------------------

fn handle_init(project_dir: &PathBuf) -> Result<()> {
    // Create .orca/ directory
    let orca_dir = project_dir.join(".orca");
    std::fs::create_dir_all(&orca_dir)
        .with_context(|| format!("failed to create {}", orca_dir.display()))?;
    println!("Created {}", orca_dir.display());

    // Create .agents/worktree/ directory
    let worktree_dir = project_dir.join(".agents").join("worktree");
    std::fs::create_dir_all(&worktree_dir)
        .with_context(|| format!("failed to create {}", worktree_dir.display()))?;
    println!("Created {}", worktree_dir.display());

    // Write orca.toml from defaults
    let config_path = project_dir.join("orca.toml");
    if config_path.exists() {
        println!("orca.toml already exists, skipping");
    } else {
        let default_config = Config::default();
        let toml_str = toml::to_string_pretty(&default_config)
            .context("failed to serialize default config")?;
        std::fs::write(&config_path, toml_str)
            .with_context(|| format!("failed to write {}", config_path.display()))?;
        println!("Created {}", config_path.display());
    }

    // Append to .gitignore if needed
    let gitignore_path = project_dir.join(".gitignore");
    let entries = [".orca/", ".agents/"];
    let existing = std::fs::read_to_string(&gitignore_path).unwrap_or_default();

    let mut to_add = Vec::new();
    for entry in &entries {
        if !existing.lines().any(|line| line.trim() == *entry) {
            to_add.push(*entry);
        }
    }

    if !to_add.is_empty() {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&gitignore_path)
            .with_context(|| format!("failed to open {}", gitignore_path.display()))?;

        // Add a newline separator if file doesn't end with one
        if !existing.is_empty() && !existing.ends_with('\n') {
            writeln!(file)?;
        }

        for entry in &to_add {
            writeln!(file, "{}", entry)?;
        }
        println!("Updated .gitignore with: {}", to_add.join(", "));
    }

    println!("Orca initialized. Edit orca.toml to configure.");
    Ok(())
}

// -- Setup -------------------------------------------------------------------

fn handle_setup(action: SetupAction) -> Result<()> {
    match action {
        SetupAction::Mcp => {
            let exe_path = std::env::current_exe()
                .unwrap_or_else(|_| PathBuf::from("orca"));

            let mcp_config = json!({
                "mcpServers": {
                    "orca": {
                        "command": exe_path.to_string_lossy(),
                        "args": ["mcp-server"]
                    }
                }
            });

            println!("Add the following to ~/.claude/settings.json:\n");
            println!("{}", serde_json::to_string_pretty(&mcp_config)?);
            Ok(())
        }
    }
}

// -- Config ------------------------------------------------------------------

fn handle_config(config: &Config) -> Result<()> {
    let toml_str = toml::to_string_pretty(config)
        .context("failed to serialize config")?;
    println!("{}", toml_str);
    Ok(())
}

// -- Helpers -----------------------------------------------------------------

/// Connect to the daemon and send a single RPC request.
async fn ipc_call(
    socket_path: &std::path::Path,
    method: &str,
    params: serde_json::Value,
) -> Result<RpcResponse> {
    let mut client = IpcClient::connect(socket_path).await
        .with_context(|| {
            format!(
                "failed to connect to daemon at {}. Is the daemon running? Try: orca daemon start",
                socket_path.display()
            )
        })?;

    let request = RpcRequest::new(method, params);
    client.call(&request).await
        .context("RPC call failed")
}

/// Print an RPC response as formatted JSON.
fn print_response(resp: &RpcResponse) {
    if let Some(ref error) = resp.error {
        eprintln!("Error: {} (code {})", error.message, error.code);
        if let Some(ref data) = error.data {
            eprintln!("Details: {}", serde_json::to_string_pretty(data).unwrap_or_default());
        }
    } else if let Some(ref result) = resp.result {
        println!("{}", serde_json::to_string_pretty(result).unwrap_or_default());
    }
}
