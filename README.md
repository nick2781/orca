# Orca

> **This is an experimental project under active development. NOT ready for production use.**

Multi-agent orchestrator: Claude Code brain + Codex workers.

[中文](README.zh-CN.md) | English

Orca lets Claude Code (CC) act as the brain — planning, reviewing, and making decisions — while dispatching implementation tasks to multiple Codex workers running in parallel. A lightweight daemon coordinates everything through structured messaging over Unix sockets.

## Status

### What works

- Daemon with Unix socket IPC (start/stop/status, PID management)
- Task lifecycle: plan submission → DAG scheduling → worker dispatch → review
- Smart isolation: worktree vs serial based on file overlap
- 3-level escalation routing with configurable rules
- MCP server for Claude Code integration (8 tools)
- CLI with 11 subcommands
- State persistence (state.json + append-only ledger)
- 92 tests passing, clippy clean

### What's in progress

- **Terminal integration**: Workers need to run in visible terminal panes so users can watch Codex work in real time. This requires the terminal to support programmatic split-pane creation — see [Terminal Support](#terminal-support) below.
- **Ghostty CLI API**: We are planning to contribute a PR to [Ghostty](https://github.com/ghostty-org/ghostty) to add `ghostty +action new_split -- <command>` support. See [ghostty-org/ghostty#2353](https://github.com/ghostty-org/ghostty/discussions/2353).

### What's not done yet

- End-to-end tested workflow with real Codex
- CC-to-daemon escalation feedback loop
- `orca worker run <task-id>` CLI for manual pane execution

## Why Orca?

| Problem | Orca's Approach |
|---------|-----------------|
| Nearly every tool depends on tmux | Terminal adapter layer (supports terminals with split APIs) |
| No CC-to-Codex orchestration | CC as brain, Codex as workers, daemon as middle layer |
| All-or-nothing permissions | 3-level escalation: worker → CC auto → user |
| Communication via terminal buffer parsing | Structured JSON-RPC over Unix Socket |

## Architecture

```
┌──────────────────────────────────────────────┐
│  CC (Claude Code) — Brain                    │
│  Planning, review, hard decisions            │
└──────────────────┬───────────────────────────┘
                   │ MCP (stdio → Unix Socket)
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
  Terminal split panes (user watches in real time)
```

Workers run in **user-visible terminal split panes**, not as hidden subprocesses. The user sees Codex working in real time. Task completion is detected by inspecting git state after the worker exits.

### Three layers

| Layer | Component | Does | Does NOT |
|-------|-----------|------|----------|
| Brain | CC | Plan, review, decide | Write code |
| Orchestration | orcad | Schedule, route escalations, manage state | LLM inference |
| Execution | Worker (Codex) | Implement, test, git ops | Architecture decisions |

## Terminal Support

Orca needs terminals that support **programmatic split-pane creation** — the ability to create a new split pane and run a command in it from an external process.

| Terminal | Split API | Status |
|----------|----------|--------|
| **WezTerm** | `wezterm cli split-pane -- cmd` | Supported |
| **kitty** | `kitten @ launch --type=window cmd` | Supported |
| **iTerm2** | AppleScript / Python API | Supported |
| **Ghostty** | None (yet) | [PR planned](https://github.com/ghostty-org/ghostty/discussions/2353) |
| **Zellij** | `zellij action new-pane -- cmd` | Planned |
| **Any terminal** | Manual mode (user splits + runs command) | Fallback |

**Ghostty users**: Ghostty's internal `new_split` action exists but has no external API. We plan to contribute this to Ghostty. In the meantime, orca prints the command for you to manually run in a Ghostty split pane.

## 3-Level Escalation

```
Worker encounters a problem
      │
Level 0: Worker self-resolves (compile errors, simple failures)
      │ can't resolve
Level 1: Daemon → CC auto-handles (design choices, timeouts)
      │ CC is unsure
Level 2: CC → User (architecture changes, dangerous operations)
```

Configurable via `orca.toml`:

```toml
[escalation]
auto_approve = ["implementation_choice", "test_failure", "timeout"]
always_user = ["destructive_operation", "architecture_change"]
cc_first = ["conflict", "scope_exceeded"]
```

## Smart Isolation

| Situation | Strategy |
|-----------|----------|
| No file overlap with running tasks | Git worktree (concurrent) |
| File overlap | Serial queue |
| No file info | Escalate to CC |

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Nick2781/orca/main/install.sh | sh
```

Or build from source:

```bash
git clone https://github.com/Nick2781/orca.git
cd orca && cargo build --release
```

## Quick Start

```bash
cd my-project
orca init                    # Creates orca.toml + .orca/
orca setup mcp               # Prints MCP config for Claude Code
orca daemon start             # Start the daemon
```

MCP tools available in Claude Code:

| Tool | Description |
|------|-------------|
| `orca_plan` | Submit execution plan (tasks + dependencies) |
| `orca_status` | View status + pending escalations |
| `orca_task_detail` | Task details |
| `orca_decide` | Respond to escalation |
| `orca_review` | Accept / reject completed task |
| `orca_cancel` | Cancel task |
| `orca_worker_list` | Worker status |
| `orca_merge` | Merge accepted branches |

### Workflow

In normal use, CC creates and submits plans via MCP — no manual `plan.json` needed.
The `orca plan submit` CLI command is available for testing and debugging.

## CLI

```bash
orca daemon start|stop|status
orca task list|detail|cancel|retry
orca worker list|connect|kill
orca plan submit <file.json>
orca review accept|reject <task-id>
orca merge <task-ids...>
orca escalation list|decide
orca init / setup mcp / config
```

## Configuration

```toml
[daemon]
max_workers = 4

[terminal]
provider = "ghostty"  # ghostty | wezterm | kitty | iterm2 | manual

[worker.codex]
command = "codex"
args = []             # default: interactive mode with approval prompts
timeout_secs = 300

[isolation]
worktree_dir = ".agents/worktree"
default_strategy = "auto"
target_branch = "main"
```

## Worker Extensibility

V1 ships Codex only. The `Worker` trait supports future adapters (Gemini CLI, Aider, etc.):

```rust
#[async_trait]
trait Worker: Send + Sync {
    async fn spawn(&self, worker_id: &str, work_dir: &str) -> Result<()>;
    async fn dispatch(&self, worker_id: &str, task: &TaskSpec) -> Result<()>;
    async fn health_check(&self, worker_id: &str) -> Result<WorkerStatus>;
    async fn interrupt(&self, worker_id: &str) -> Result<()>;
    async fn cleanup(&self, worker_id: &str) -> Result<()>;
}
```

## Roadmap

- [ ] Ghostty CLI API PR ([ghostty-org/ghostty#2353](https://github.com/ghostty-org/ghostty/discussions/2353))
- [ ] WezTerm / kitty adapter implementation
- [ ] `orca worker run <task-id>` for manual pane execution
- [ ] End-to-end tested Codex workflow
- [ ] CC escalation feedback loop
- [ ] Zellij adapter

## Tech Stack

Rust, tokio, clap, serde, rmcp (MCP SDK), tracing

## License

MIT
