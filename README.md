# Orca

> **WARNING: This is an experimental project under active development. NOT ready for production use.** Core architecture is implemented but not yet battle-tested. Use at your own risk.

Multi-agent orchestrator: Claude Code brain + Codex workers.

[中文](README.zh-CN.md) | English

Orca lets Claude Code (CC) act as the brain -- planning, reviewing, and making decisions -- while dispatching implementation tasks to multiple Codex workers running in parallel. A lightweight daemon coordinates everything through structured messages, no terminal buffer parsing required.

## Why Orca?

The current AI coding agent ecosystem has a core tension:

- **Claude Code**: excellent analysis and reasoning, but weaker at code details
- **Codex**: solid code generation, but verbose output and poor business context

Existing tools (oh-my-claudecode, claude-squad, CCCC, etc.) fall short:

| Problem | Orca's Approach |
|---------|-----------------|
| Nearly every tool depends on tmux | Native terminal support (Ghostty, iTerm2, any terminal) |
| No CC-to-Codex orchestration | CC as brain, Codex as workers, daemon as middle layer |
| All-or-nothing permissions | 3-level escalation: worker auto-fix -> CC decides -> user confirms |
| Communication via terminal buffer text parsing | Structured JSON-RPC over Unix Socket |

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Nick2781/orca/main/install.sh | sh
```

Or build from source:

```bash
cargo install --path .
```

## Quick Start

```bash
# Initialize in your project
cd my-project
orca init

# Set up Claude Code MCP integration
orca setup mcp

# Start the daemon
orca daemon start
```

Once running, orca MCP tools are automatically available inside Claude Code:

| MCP Tool | Description |
|----------|-------------|
| `orca_plan` | Submit an execution plan (tasks + dependency graph) |
| `orca_status` | View global status and pending escalation requests |
| `orca_task_detail` | View details for a single task |
| `orca_decide` | Respond to a worker's escalation request |
| `orca_review` | Review a completed task (accept / reject) |
| `orca_cancel` | Cancel a task |
| `orca_worker_list` | View worker status |
| `orca_merge` | Merge branches that passed review |

## Architecture

```
┌──────────────────────────────────────────────┐
│  CC (Claude Code) — Brain                    │
│  Planning, review, hard decisions, user talk │
└──────────────────┬───────────────────────────┘
                   │ MCP (stdio -> Unix Socket)
                   ▼
┌──────────────────────────────────────────────┐
│  orcad (daemon) — Orchestration layer        │
│  Task scheduling, escalation routing,        │
│  isolation decisions, state persistence      │
└──────┬──────────────┬──────────────┬─────────┘
       ▼              ▼              ▼
  ┌─────────┐   ┌─────────┐   ┌─────────┐
  │ Worker 1│   │ Worker 2│   │ Worker 3│
  │ (Codex) │   │ (Codex) │   │ (Codex) │
  └─────────┘   └─────────┘   └─────────┘
  Terminal panes (Ghostty / iTerm2 / any)
```

### Three-Layer Responsibilities

| Layer | Component | Responsibilities | Does NOT do |
|-------|-----------|-----------------|-------------|
| Brain | CC (Claude Code) | Plan decomposition, code review, design decisions, user interaction | Write code directly |
| Orchestration | orcad (daemon) | Task scheduling, isolation decisions, escalation routing, worker lifecycle, state persistence | LLM inference |
| Execution | Worker (Codex) | Code implementation, tests, git operations | Architecture decisions |

## 3-Level Escalation

```
Worker encounters a problem
      │
      ▼
Level 0: Worker self-resolves
  Fix compile errors, retry simple test failures
      │ can't resolve
      ▼
Level 1: Daemon -> CC auto-handles
  Design choices, timeout retries, test failure analysis
      │ CC is unsure
      ▼
Level 2: CC -> User
  Architecture changes, dangerous operations, out-of-scope work
```

Escalation routing is configurable:

```toml
[escalation]
auto_approve = ["implementation_choice", "test_failure", "timeout"]
always_user = ["destructive_operation", "architecture_change"]
cc_first = ["conflict", "scope_exceeded"]
```

## Smart Isolation

The daemon automatically decides isolation strategy based on file overlap between tasks:

| Situation | Strategy |
|-----------|----------|
| No file overlap with running tasks | Worktree isolation (independent branch, concurrent execution) |
| File overlap with running tasks | Serial queue (wait for preceding task to finish) |
| No file information available | Escalate to CC to decide whether concurrency is safe |

Worktrees are placed under `.agents/worktree/` to keep the project directory clean.

## Configuration

Project-level `orca.toml`:

```toml
[daemon]
socket_path = ".orca/orca.sock"
max_workers = 4
log_level = "info"

[terminal]
provider = "ghostty"        # ghostty | iterm2 | manual
layout = "tabs"
auto_open = true

[worker.codex]
command = "codex"
args = ["--full-auto", "-q"]
timeout_secs = 300
max_retries = 2

[isolation]
worktree_dir = ".agents/worktree"
default_strategy = "auto"   # auto | worktree | serial
target_branch = "main"

[escalation]
auto_approve = ["implementation_choice"]
always_user = ["architecture_change", "destructive_operation"]
cc_first = ["test_failure", "timeout", "conflict"]
```

Global defaults live in `~/.orca/config.toml`; project-level config overrides global.

## CLI Commands

```bash
# Daemon management
orca daemon start|stop|status

# Task management
orca task list [--filter running|blocked|pending]
orca task detail <id>
orca task cancel <id>
orca task retry <id>

# Worker management
orca worker list
orca worker connect [--id <id>] [--auto]
orca worker kill <id>

# Plan submission
orca plan submit <file.json>

# Review
orca review accept <task-id>
orca review reject <task-id> [--feedback "..."]

# Merge
orca merge <task-ids...>
orca merge --all-accepted

# Escalation management
orca escalation list
orca escalation decide <id> --choice <value>

# Setup
orca init
orca setup mcp
orca config
```

## Project Structure

```
project-root/
├── .orca/                  # daemon state (gitignored)
│   ├── orca.sock           # Unix socket
│   ├── state.json          # task/worker state (survives restarts)
│   ├── ledger.jsonl        # event log (auditable)
│   └── logs/               # daemon and worker logs
├── .agents/                # agent workspace (gitignored)
│   └── worktree/           # git worktree directories
├── orca.toml               # project config (version controlled)
```

## Worker Extensibility

The first release ships with a Codex adapter only, but the Worker trait is extensible:

```rust
trait Worker: Send + Sync {
    async fn spawn(&self, worker_id: &str, work_dir: &str) -> Result<()>;
    async fn dispatch(&self, worker_id: &str, task: &TaskSpec) -> Result<()>;
    async fn health_check(&self, worker_id: &str) -> Result<WorkerStatus>;
    async fn interrupt(&self, worker_id: &str) -> Result<()>;
    async fn cleanup(&self, worker_id: &str) -> Result<()>;
}
```

Future adapters could support Gemini CLI, Claude Code workers, Aider, and more.

## Tech Stack

| Component | Choice | Rationale |
|-----------|--------|-----------|
| Language | Rust | Single-binary distribution, performance, safety |
| Async runtime | tokio | Rust async standard |
| CLI | clap | Mature CLI parsing framework |
| IPC | Unix Socket + JSON-RPC | Simple, reliable, debuggable |
| MCP | rmcp | Official Rust MCP SDK |
| Git | git2 (libgit2) | Worktree management |
| Logging | tracing | Structured async logging |

## Comparison

| Dimension | Existing tools | Orca |
|-----------|---------------|------|
| Terminal | Requires tmux | Native terminal (Ghostty / iTerm2 / any) |
| Orchestration | CC plugin or standalone TUI | CC brain + daemon middle layer |
| Communication | Text buffer / filesystem | Structured JSON-RPC over Unix Socket |
| Escalation | All or nothing | 3-level: worker -> CC -> user |
| Isolation | Always worktree | Smart: worktree or serial based on file overlap |
| Workers | Hard-coded | Extensible Worker trait |
| Install | npm/cargo + tmux + dependencies | Single binary, one-line curl |

## License

MIT
