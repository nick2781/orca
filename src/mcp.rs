use std::path::PathBuf;

use anyhow::Result;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ServiceExt, tool, tool_box};
use serde_json::Value;
use tracing::info;

use crate::daemon::server::IpcClient;
use crate::protocol::RpcRequest;

/// MCP server that forwards tool calls to the orcad daemon via IPC.
#[derive(Clone)]
pub struct OrcaMcp {
    socket_path: PathBuf,
}

impl OrcaMcp {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    /// Helper: connect to the daemon and make an RPC call, returning the
    /// result as a formatted string suitable for MCP tool output.
    async fn rpc_call(&self, method: &str, params: Value) -> String {
        let client = match IpcClient::connect(&self.socket_path).await {
            Ok(c) => c,
            Err(e) => return format!("error: failed to connect to orcad: {e}"),
        };
        let mut client = client;
        let req = RpcRequest::new(method, params);
        match client.call(&req).await {
            Ok(resp) => {
                if let Some(err) = resp.error {
                    format!("error: {} (code {})", err.message, err.code)
                } else if let Some(result) = resp.result {
                    serde_json::to_string_pretty(&result)
                        .unwrap_or_else(|_| result.to_string())
                } else {
                    "ok".to_string()
                }
            }
            Err(e) => format!("error: {e}"),
        }
    }
}

// Tool definitions. Each method is annotated with #[tool] and forwards
// its parameters to the orcad daemon via the IPC socket.
#[tool(tool_box)]
impl OrcaMcp {
    /// Submit a plan (YAML/JSON task tree) for orchestrated execution across
    /// multiple Codex workers. The daemon decomposes the plan into tasks,
    /// assigns git worktrees, and launches workers.
    #[tool(description = "Submit an execution plan to the orca orchestrator")]
    async fn orca_plan(
        &self,
        #[tool(param)] plan: Value,
    ) -> String {
        self.rpc_call("orca_plan", serde_json::json!({ "plan": plan })).await
    }

    /// Get the current status of all tasks managed by the orchestrator,
    /// optionally filtered by state (e.g. "running", "blocked", "done").
    #[tool(description = "Get orchestrator status with optional state filter")]
    async fn orca_status(
        &self,
        #[tool(param)]
        #[serde(default)]
        filter: Option<String>,
    ) -> String {
        self.rpc_call("orca_status", serde_json::json!({ "filter": filter })).await
    }

    /// Get detailed information about a specific task including its logs,
    /// current state, assigned worker, and git branch.
    #[tool(description = "Get detailed info for a specific task")]
    async fn orca_task_detail(
        &self,
        #[tool(param)] task_id: String,
    ) -> String {
        self.rpc_call("orca_task_detail", serde_json::json!({ "task_id": task_id })).await
    }

    /// Respond to a pending escalation from a worker. Escalations occur when
    /// a worker needs human/orchestrator decision on architecture changes,
    /// scope questions, or conflict resolution.
    #[tool(description = "Decide on a pending escalation from a worker")]
    async fn orca_decide(
        &self,
        #[tool(param)] escalation_id: String,
        #[tool(param)] decision: String,
        #[tool(param)] reason: String,
    ) -> String {
        self.rpc_call(
            "orca_decide",
            serde_json::json!({
                "escalation_id": escalation_id,
                "decision": decision,
                "reason": reason,
            }),
        )
        .await
    }

    /// Review a completed task. Verdict is "accept" or "reject". Accepted
    /// tasks are eligible for merge; rejected tasks are sent back to the
    /// worker with feedback.
    #[tool(description = "Review a completed task (accept/reject)")]
    async fn orca_review(
        &self,
        #[tool(param)] task_id: String,
        #[tool(param)] verdict: String,
        #[tool(param)]
        #[serde(default)]
        feedback: Option<String>,
    ) -> String {
        self.rpc_call(
            "orca_review",
            serde_json::json!({
                "task_id": task_id,
                "verdict": verdict,
                "feedback": feedback,
            }),
        )
        .await
    }

    /// Cancel a running or pending task. The worker process is terminated
    /// and the worktree is cleaned up.
    #[tool(description = "Cancel a running or pending task")]
    async fn orca_cancel(
        &self,
        #[tool(param)] task_id: String,
    ) -> String {
        self.rpc_call("orca_cancel", serde_json::json!({ "task_id": task_id })).await
    }

    /// List all connected workers and their current assignments.
    #[tool(description = "List connected workers and their assignments")]
    async fn orca_worker_list(&self) -> String {
        self.rpc_call("orca_worker_list", serde_json::json!({})).await
    }

    /// Merge the git branches of multiple accepted tasks back into the
    /// target branch. Tasks must be in the "accepted" state.
    #[tool(description = "Merge accepted task branches into target branch")]
    async fn orca_merge(
        &self,
        #[tool(param)] task_ids: Vec<String>,
    ) -> String {
        self.rpc_call("orca_merge", serde_json::json!({ "task_ids": task_ids })).await
    }
}

impl ServerHandler for OrcaMcp {
    tool_box!(@derive);

    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: Default::default(),
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .build(),
            server_info: Implementation {
                name: "orca".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            instructions: Some(
                "Orca multi-agent orchestrator. Use these tools to submit plans, \
                 monitor tasks, review results, and manage workers."
                    .into(),
            ),
        }
    }
}

/// Start the MCP server on stdio transport. This is the entry point called
/// by `orca mcp-server`.
pub async fn run_mcp_server(socket_path: PathBuf) -> Result<()> {
    info!("starting MCP server, daemon socket: {}", socket_path.display());

    let server = OrcaMcp::new(socket_path);
    let transport = rmcp::transport::stdio();
    let service = server.serve(transport).await?;

    // Block until the client disconnects
    service.waiting().await?;

    info!("MCP server shut down");
    Ok(())
}
