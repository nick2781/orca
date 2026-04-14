# Orca

> **This is an experimental project under active development. NOT ready for production use.**

Multi-agent orchestrator: Claude Code brain + Codex workers.

[中文](README.zh-CN.md) | English

Orca lets Claude Code (CC) plan, review, and decide while Codex workers implement tasks in visible terminal panes. A small daemon coordinates scheduling, state, isolation, and escalation over Unix socket JSON-RPC.

## Current scope

- Daemon with Unix socket IPC, state persistence, and a task scheduler
- Task lifecycle from plan submission to review
- Smart isolation with worktree vs same-dir decisions
- MCP server for Claude Code integration
- Codex worker execution in visible terminal panes
- Log-based completion detection from Codex session logs
- Active escalation notifications back to the main terminal

Still missing:

- End-to-end validation with real Codex workflows
- WezTerm / kitty / Zellij adapters
- A native Ghostty CLI split API instead of AppleScript

## Terminal Support

Orca needs terminals that support **programmatic split-pane creation** — the ability to create a new split pane and run a command in it from an external process.

| Terminal | Split API | Status |
|----------|----------|--------|
| **WezTerm** | `wezterm cli split-pane -- cmd` | Planned |
| **kitty** | `kitten @ launch --type=window cmd` | Planned |
| **iTerm2** | AppleScript / Python API | Supported |
| **Ghostty** | AppleScript split + focus API | Supported |
| **Zellij** | `zellij action new-pane -- cmd` | Planned |
| **Any terminal** | Manual mode (user splits + runs command) | Supported fallback |

**Ghostty users**: Orca works today through AppleScript-driven split, focus, and terminal targeting. A native Ghostty CLI split API would still be better. See [ghostty-org/ghostty#2353](https://github.com/ghostty-org/ghostty/discussions/2353).

## 3-Level Escalation

When an escalation is created, the daemon actively notifies the main agent:
- Focuses CC's terminal pane (brings it to front)
- Sends a macOS notification with the escalation summary

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

Pre-release / current branch:

```bash
curl -fsSL https://raw.githubusercontent.com/Nick2781/orca/main/install.sh | sh
```

Stable release channel:

```bash
curl -fsSL https://github.com/Nick2781/orca/releases/latest/download/install.sh | sh
```

The `main` installer falls back to a source build when no GitHub release exists yet. The release installer is the stable path once tagged releases are available.

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
orca worker list|connect|kill|run <task-id>
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

## Tech Stack

Rust, tokio, clap, serde, rmcp (MCP SDK), tracing

## License

MIT
