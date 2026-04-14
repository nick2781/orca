# Orca — Multi-Agent Orchestrator

**Status: Draft**

English | [中文](design.zh-CN.md)

## Changelog

| Date | Change |
|------|--------|
| 2026-04-13 | Initial version |

---

> CC (Claude Code) 作为主脑编排 Codex worker 的本地多 agent 协作框架。daemon 中心架构，原生终端集成，Rust 实现，单二进制分发。

## 1. Problem Statement

当前 AI coding agent 生态的核心矛盾：

- **Claude Code (CC)**: 分析和表达能力强，但编码细节较差
- **Codex**: 写代码能力不错，但表达啰嗦、业务理解差

现有多 agent 方案（oh-my-claudecode, claude-squad, CCCC 等）的不足：

| 问题 | 说明 |
|------|------|
| tmux 依赖 | 几乎所有方案都绑定 tmux，无法用原生终端分屏 |
| CC→Codex 编排缺失 | 没有项目完整实现 CC 做主脑 + Codex 做 worker |
| 提权机制粗糙 | 要么全自动要么全手动，无分级提权 |

### Why NOT tmux

几乎所有多 agent 编排工具（claude-squad, multiclaude, oh-my-claudecode 等）都依赖 tmux。
orca 选择不用 tmux，原因：

| 问题 | 说明 |
|------|------|
| **快捷键体验差** | tmux 的 prefix key（Ctrl+B）+ 命令键学习成本高，与终端、编辑器快捷键冲突 |
| **与现代终端不兼容** | 在 Ghostty/kitty 等终端中，tmux 的复制、滚动、鼠标行为与原生体验冲突，需要大量配置才能用 |
| **视觉割裂** | tmux 的 pane 边框、状态栏与终端原生 UI 风格不一致 |
| **额外依赖** | 用户需要安装和配置 tmux，增加入门门槛 |
| **现代终端已有原生 API** | Ghostty 1.3+ (AppleScript)、WezTerm (CLI)、kitty (remote control)、iTerm2 (Python API) 都提供了可编程的分屏接口 |

orca 的策略：通过终端适配层（Terminal trait）直接使用各终端的原生 API。
用户看到的是自己终端的原生分屏，不是 tmux 的虚拟终端。
| 通信脆弱 | 多数基于终端缓冲区文本解析，不可靠 |

## 2. Design Goals

| Goal | Description |
|------|-------------|
| CC 做主脑 | plan、review、困难决策由 CC 处理 |
| Codex 做 worker | 代码实现、测试等纯执行工作由 Codex 处理 |
| 对等协作 | 不是纯主从，worker 有执行自主权，遇到问题可以回来讨论 |
| 并发执行 | 多个 worker 并行处理独立任务 |
| 分级提权 | worker 自行解决 → CC 自动判断 → 升级到用户 |
| 终端无关 | Ghostty/iTerm2/任何终端，不依赖 tmux |
| 可扩展 worker | 抽象 Worker trait，第一版只实现 Codex |
| 开源友好 | 单二进制分发，一行安装，配置简单 |

## 3. Architecture

### 3.1 Three-Layer Overview

```
┌──────────────────────────────────────────────────────────────┐
│  CC (Claude Code) — Brain                                    │
│  MCP tools 与 daemon 交互                                     │
│  职责：plan、review、困难决策、用户沟通                          │
└──────────────────┬───────────────────────────────────────────┘
                   │ MCP (Unix Socket)
                   ▼
┌──────────────────────────────────────────────────────────────┐
│  orcad (daemon) — Orchestration Layer                        │
│                                                              │
│  ┌────────────┐ ┌────────────┐ ┌────────────┐ ┌───────────┐ │
│  │ Task Queue │ │ Dependency │ │ Isolation  │ │ Escalation│ │
│  │ & Scheduler│ │   Graph    │ │  Manager   │ │  Router   │ │
│  └────────────┘ └────────────┘ └────────────┘ └───────────┘ │
│  ┌────────────┐ ┌────────────┐ ┌────────────┐               │
│  │  Worker    │ │  State     │ │  Terminal  │               │
│  │  Registry  │ │  Store     │ │  Manager   │               │
│  └────────────┘ └────────────┘ └────────────┘               │
└──────┬──────────────┬──────────────┬────────────────────────┘
       │              │              │
       ▼              ▼              ▼
  ┌─────────┐   ┌─────────┐   ┌─────────┐
  │ Worker 1│   │ Worker 2│   │ Worker 3│    Terminal panes
  │ (Codex) │   │ (Codex) │   │ (Codex) │    (Ghostty/iTerm2)
  │         │   │         │   │         │
  │ worktree│   │ worktree│   │ same dir│
  └─────────┘   └─────────┘   └─────────┘
```

### 3.2 Layer Responsibilities

| Layer | Component | Responsibilities | Does NOT do |
|-------|-----------|-----------------|-------------|
| Brain | CC (Claude Code) | plan 拆分、code review、方案决策、用户交互 | 不直接写代码 |
| Orchestration | orcad (daemon) | 任务调度、隔离决策、提权路由、worker 生命周期、状态持久化 | 不做 LLM 推理 |
| Execution | Worker (Codex) | 代码实现、测试、git 操作 | 不做架构决策 |

### 3.3 Communication Protocol

| Path | Protocol | Description |
|------|----------|-------------|
| CC → MCP server | stdio (stdin/stdout) | CC spawn `orca mcp-server` 进程 |
| MCP server → orcad | Unix Socket (JSON-RPC) | MCP server 作为 orcad 的薄客户端 |
| CLI → orcad | Unix Socket (JSON-RPC) | 人工操作 / 调试 |
| orcad → Worker | Unix Socket per worker | 结构化 task spec 下发 |
| Worker → orcad | Same, reverse | 进度上报、结果提交、提权请求 |

Note: MCP is request-response. CC discovers pending escalations by polling `orca_status`.
Daemon does NOT push to CC — CC pulls at its own cadence.

## 4. Task Lifecycle

### 4.1 Task Spec

CC 下发 plan 后，daemon 将每个子任务转化为标准 Task Spec：

```json
{
  "id": "task-001",
  "title": "implement user auth middleware",
  "description": "Add JWT validation middleware to gateway router",
  "context": {
    "files": ["apps/gateway/internal/handler/routes.go"],
    "references": ["docs/adr/001-repository-pattern.md"],
    "constraints": "follow existing middleware pattern in auth.go"
  },
  "isolation": "worktree",
  "depends_on": [],
  "priority": 1
}
```

### 4.2 State Machine

```
                    CC dispatches plan
                          │
                          ▼
                    ┌──────────┐
                    │ pending  │
                    └────┬─────┘
                         │ daemon assigns to worker
                         ▼
                    ┌──────────┐
          ┌────────│ assigned │
          │        └────┬─────┘
          │             │ worker starts
          │             ▼
          │        ┌──────────┐
          │        │ running  │◄──────────────┐
          │        └────┬─────┘               │
          │             │                     │
          │        ┌────┴────┐                │
          │        ▼         ▼                │
          │   ┌────────┐ ┌──────────┐         │
          │   │ done   │ │ blocked  │         │
          │   └───┬────┘ └────┬─────┘         │
          │       │           │               │
          │       │      ┌────┴────┐          │
          │       │      ▼         ▼          │
          │       │ ┌────────┐ ┌────────┐     │
          │       │ │esc → CC│ │esc →   │     │
          │       │ │(auto)  │ │ user   │     │
          │       │ └───┬────┘ └───┬────┘     │
          │       │     │          │           │
          │       │     │  decision made      │
          │       │     └─────┬────┘          │
          │       │           └───► resumed ──┘
          │       ▼
          │  ┌──────────┐
          │  │ review   │ ← CC reviews output
          │  └────┬─────┘
          │       │
          │  ┌────┴────┐
          │  ▼         ▼
          │ accepted  rejected ──► pending (rework)
          │  │
          ▼  ▼
     ┌──────────┐
     │ completed│
     └──────────┘
```

### 4.3 State Descriptions

| State | Triggered by | Description |
|-------|-------------|-------------|
| pending | CC via plan | 任务创建，等待分配 |
| assigned | daemon | daemon 根据依赖图和 worker 空闲状态分配 |
| running | worker | worker 开始执行 |
| blocked | worker | 遇到需要决策的问题 |
| blocked → esc CC | daemon | daemon 判断 CC 能自动处理的提权 |
| blocked → esc user | daemon/CC | CC 也不确定，升级到用户 |
| review | daemon | 执行完成，等待 CC review |
| accepted/rejected | CC | review 结果 |
| completed | daemon | 任务最终完成，变更已合并 |

### 4.4 Dependency Graph Execution

daemon 维护 DAG，自动调度：

```
task-001 (auth middleware) ──┐
                             ├──► task-003 (integration test)
task-002 (user model)  ──────┘
                                        │
task-004 (API docs)  ◄──────────────────┘
```

- 无依赖的任务并发分配给不同 worker
- 有依赖的任务等前置完成后自动调度
- CC 在 plan 阶段定义依赖关系，daemon 执行调度

## 5. Escalation Mechanism

### 5.1 Three-Level Model

```
Worker 遇到问题
      │
      ▼
┌─────────────────────────────────────────────┐
│ Level 0: Worker 自行解决                      │
│ - 编译错误自己修                               │
│ - 简单测试失败自己重试                          │
│ - 文件不存在就创建                              │
└─────────────────┬───────────────────────────┘
                  │ 解决不了
                  ▼
┌─────────────────────────────────────────────┐
│ Level 1: Daemon → CC 自动处理                 │
│ - "两种实现方式选哪个" → CC 根据 plan 判断      │
│ - "这个文件要不要改" → CC 根据上下文判断         │
│ - Worker 超时 → CC 决定重试/换方案/跳过         │
│ - 测试持续失败 → CC 分析原因，调整 task spec    │
└─────────────────┬───────────────────────────┘
                  │ CC 也不确定
                  ▼
┌─────────────────────────────────────────────┐
│ Level 2: CC → 用户                            │
│ - 架构变更：引入新依赖、改接口契约               │
│ - 权限操作：push、删分支、改配置                 │
│ - 方案分歧：CC 和 Worker 判断不一致              │
│ - 超出 plan 范围的变更                          │
└─────────────────────────────────────────────┘
```

### 5.2 Escalation Request Structure

Worker → daemon:

```json
{
  "type": "escalation",
  "task_id": "task-001",
  "worker_id": "codex-1",
  "level": "decision",
  "category": "implementation_choice",
  "summary": "JWT validation: middleware vs decorator pattern",
  "options": [
    {"id": "a", "desc": "middleware pattern, consistent with existing code"},
    {"id": "b", "desc": "decorator pattern, more flexible but new pattern"}
  ],
  "context": {
    "relevant_files": ["apps/gateway/internal/handler/auth.go"],
    "worker_recommendation": "a"
  }
}
```

### 5.3 Routing Rules

| Category | Default Route | Description |
|----------|--------------|-------------|
| `implementation_choice` | CC auto | CC 根据 codebase 上下文判断 |
| `test_failure` | CC auto | CC 分析失败原因，调整 spec |
| `timeout` | CC auto | CC 决定重试/跳过/拆分 |
| `architecture_change` | User | 引入新模式、新依赖 |
| `destructive_operation` | User | push、删文件、改配置 |
| `scope_exceeded` | User | 超出原始 plan 范围 |
| `conflict` | CC auto → User | CC 先尝试解决，解决不了给用户 |

### 5.4 Routing Configuration

```toml
[escalation]
auto_approve = ["implementation_choice", "test_failure", "timeout"]
always_user = ["destructive_operation", "architecture_change"]
cc_first = ["conflict", "scope_exceeded"]

[escalation.timeout]
worker_timeout_secs = 300
max_retries = 2
```

### 5.5 User Notification (Level 2)

| Method | Implementation |
|--------|---------------|
| CC terminal | CC 主 pane 显示提权请求，等待用户回复 |
| System notification | macOS Notification Center (optional) |
| Sound | Terminal bell (optional) |

## 6. Worker Abstraction

### 6.1 Worker Trait

```rust
trait Worker: Send + Sync {
    /// Spawn worker process, return connection handle
    async fn spawn(&self, config: &WorkerConfig) -> Result<WorkerHandle>;

    /// Dispatch task to worker
    async fn dispatch(&self, handle: &WorkerHandle, task: &TaskSpec) -> Result<()>;

    /// Read worker current output/status
    async fn read_output(&self, handle: &WorkerHandle) -> Result<WorkerOutput>;

    /// Send interrupt/cancel signal
    async fn interrupt(&self, handle: &WorkerHandle) -> Result<()>;

    /// Health check
    async fn health_check(&self, handle: &WorkerHandle) -> Result<WorkerStatus>;

    /// Cleanup resources
    async fn cleanup(&self, handle: &WorkerHandle) -> Result<()>;
}
```

### 6.2 Codex Adapter

Codex runs **inside a terminal pane** — the user sees its full interactive output in real time.
Daemon does NOT capture stdout. Instead:

```
orcad
  │
  │ 1. Write AGENTS.md to worktree (output protocol + task context)
  │ 2. Build command: codex --full-auto "<prompt>"
  │ 3. Terminal.create_pane(command) → Ghostty split / iTerm2 split
  │
  ▼
┌──────────────────────────────────┐
│ Terminal Pane (user-visible)      │
│                                  │
│  codex --full-auto "<prompt>"    │
│  ┌────────────────────────────┐  │
│  │ Codex interactive UI       │  │
│  │ (user watches in real time)│  │
│  └────────────────────────────┘  │
│                                  │
│  Reads AGENTS.md for task context│
│  Writes code, runs tests, etc.  │
└──────────────────────────────────┘
  │
  │ On exit: daemon checks worktree git diff
  │          → files changed? → task done → CC review
  │          → no changes? → task blocked
  ▼
orcad detects completion
```

**Key principle:** Worker output is for the **user to watch**, not for the daemon to parse.
Task completion is detected by inspecting **git state** after Codex exits, not by parsing stdout markers.

### 6.3 Task Context via AGENTS.md

Instead of piping prompts via stdin, daemon writes an `AGENTS.md` file to the worktree
before launching Codex. This file contains task description, scoped files, constraints,
and output protocol markers. Codex reads it automatically.

### 6.4 Prompt Generation

The prompt is passed as a CLI positional argument:

```bash
codex --full-auto "Implement task: <title>. <description>. See AGENTS.md for details."
```

The full task context lives in AGENTS.md, the prompt is a concise trigger.

### 6.4 Output Markers

### 6.5 Task Completion Detection

Since Codex runs in a user-visible terminal pane (not as a piped subprocess),
daemon detects task completion by monitoring the **process exit + git state**:

| Signal | Detection | Daemon Action |
|--------|-----------|--------------|
| Codex process exits | Poll pane/process alive status | Inspect worktree |
| Git diff non-empty | `git diff --stat` in worktree | State → review, CC reviews |
| Git diff empty | No changes made | State → blocked, escalate |
| Process crashed | Non-zero exit + no changes | State → blocked, timeout escalation |

AGENTS.md output markers (`[ORCA:DONE]`, `[ORCA:ESCALATE]`, etc.) are **optional hints** —
Codex may or may not output them. The primary completion signal is process exit + git diff.

### 6.5 Extensibility

```rust
// V1
struct CodexWorker { /* ... */ }
impl Worker for CodexWorker { /* ... */ }

// Future
struct ClaudeCodeWorker { /* ... */ }
impl Worker for ClaudeCodeWorker { /* ... */ }

struct GeminiCliWorker { /* ... */ }
impl Worker for GeminiCliWorker { /* ... */ }
```

New worker type = implement `Worker` trait + register in daemon's worker registry.

## 7. Isolation & Terminal Integration

### 7.1 Smart Isolation Decision

```
New task arrives
    │
    ▼
Analyze task.context.files
    │
    ├─ No file overlap with any running task
    │   └─► worktree isolation (independent branch, concurrent)
    │
    ├─ File overlap with a running task
    │   └─► serial queue (wait for that task to complete)
    │
    └─ No files info (e.g., "write unit tests")
        └─► CC decides (escalate to ask CC if parallelizable)
```

### 7.2 Worktree Lifecycle

```
task assigned (isolation=worktree)
    │
    ▼
git worktree add .agents/worktree/{task-id} -b orca/{task-id}
    │
    ▼
worker executes in worktree directory
    │
    ▼
task done → CC review
    │
    ├─ accepted → merge branch → git worktree remove
    └─ rejected → rework in same worktree
```

### 7.3 Merge Strategy

| Scenario | Strategy |
|----------|----------|
| No conflicts | daemon auto-merges to target branch |
| Has conflicts | Escalate to CC, CC resolves or escalates to user |
| Dependency chain | Merge in DAG order |

### 7.4 Terminal Integration

#### Architecture

```rust
trait Terminal: Send + Sync {
    /// Create new pane and run command
    async fn create_pane(&self, cmd: &str, label: &str) -> Result<PaneId>;

    /// Close pane
    async fn close_pane(&self, pane: &PaneId) -> Result<()>;

    /// Focus a specific pane
    async fn focus_pane(&self, pane: &PaneId) -> Result<()>;
}
```

Implementations:

| Provider | Method | Mechanism |
|----------|--------|-----------|
| `GhosttyTerminal` | Ghostty 1.3+ AppleScript API | `split terminal id "UUID" direction right`, UUID-based addressing |
| `ItermTerminal` | iTerm2 AppleScript API | `split vertically`, `write text` |
| `WezTermTerminal` | WezTerm CLI | `wezterm cli split-pane -- cmd` (planned) |
| `KittyTerminal` | kitty remote control | `kitten @ launch` (planned) |
| `ManualTerminal` | Print command for user | Fallback for unsupported terminals |

Ghostty integration inspired by [gx-ghostty](https://github.com/ashsidhu/gx-ghostty).

#### Configuration

```toml
[terminal]
provider = "ghostty"        # ghostty | iterm2 | manual
layout = "tabs"             # tabs | manual
auto_open = true            # auto-open pane/tab for new worker
```

#### Worker Pane Display

```
╭─ orca worker: codex-1 ─────────────────────────╮
│ task: implement user auth middleware            │
│ status: running  │  elapsed: 2m30s              │
│ isolation: worktree (.agents/worktree/task-001) │
├─────────────────────────────────────────────────┤
│                                                 │
│  [codex 实时输出，透传显示]                       │
│                                                 │
╰─────────────────────────────────────────────────╯
```

Top status bar rendered by `orca worker connect`, raw worker output below.

## 8. MCP Server & CLI

### 8.1 MCP Tools (CC invokes)

| Tool | Description | Parameters |
|------|-------------|-----------|
| `orca_plan` | Submit execution plan | `tasks: TaskSpec[], dependencies: Edge[]` |
| `orca_status` | View global status | `filter?: "running" \| "blocked" \| "all"` |
| `orca_task_detail` | View single task detail | `task_id: string` |
| `orca_decide` | Respond to escalation | `escalation_id, decision, reason` |
| `orca_review` | Submit review result | `task_id, verdict: "accept" \| "reject", feedback?` |
| `orca_cancel` | Cancel task | `task_id` |
| `orca_worker_list` | View worker status | - |
| `orca_merge` | Trigger merge | `task_ids: string[]` |

### 8.2 CC Typical Workflow

```
CC: orca_plan({tasks: [...], dependencies: [...]})
     │
     ▼ daemon starts scheduling, CC continues other work

CC: orca_status()
     → "task-001: running, task-002: running, task-003: pending (blocked by 001)"

     ▼ CC polls status, discovers escalation

CC: orca_status() → sees escalation pending
CC: orca_task_detail({task_id: "task-001"}) → gets escalation details
CC: orca_decide({escalation_id: "esc-01", decision: "a", reason: "follow existing pattern"})

     ▼ task-001 completes

CC: orca_task_detail({task_id: "task-001"})
     → {status: "review", output: {files_changed: [...], tests_passed: true, diff_summary: "..."}}

CC: orca_review({task_id: "task-001", verdict: "accept"})

     ▼ all tasks complete

CC: orca_merge({task_ids: ["task-001", "task-002", "task-003"]})
```

### 8.3 CLI Commands (human/debug use)

```bash
# Daemon management
orca daemon start          # Start daemon (foreground or background)
orca daemon stop           # Stop daemon
orca daemon status         # Daemon health status

# Task management
orca plan submit plan.json # Submit plan (manual alternative to MCP)
orca task list             # List tasks
orca task detail <id>      # Task detail
orca task cancel <id>      # Cancel task
orca task retry <id>       # Retry failed task

# Worker management
orca worker list           # List all workers
orca worker connect <id>   # Connect to worker pane (manual mode)
orca worker connect --auto # Auto-assign next available worker
orca worker kill <id>      # Force kill worker

# Escalation
orca escalation list                    # List pending escalations
orca escalation decide <id> --choice a  # Manual decision

# Review
orca review <task-id> --accept
orca review <task-id> --reject --feedback "missing error handling"

# Merge
orca merge <task-ids...>
orca merge --all-accepted  # Merge all accepted tasks

# Setup
orca init                  # Initialize orca.toml + .orca/
orca setup mcp             # Write MCP config to ~/.claude/settings.json
orca config show           # Show current configuration
```

### 8.4 MCP Server Startup

```jsonc
// CC MCP config (~/.claude/settings.json)
{
  "mcpServers": {
    "orca": {
      "command": "orca",
      "args": ["mcp-server"],
      "env": {}
    }
  }
}
```

`orca mcp-server` auto-connects to `orcad` on start (auto-starts daemon if not running).
MCP server is a thin client to the daemon, not a separate process with its own state.

## 9. State & Storage

### 9.1 Directory Layout

```
project-root/
├── .orca/                  # orca daemon state (gitignored)
│   ├── orca.sock           # Unix socket
│   ├── state.json          # Current task/worker state (daemon restart recovery)
│   ├── ledger.jsonl        # Append-only event log (auditable)
│   └── logs/
│       ├── daemon.log
│       ├── codex-1.log     # Worker output logs
│       └── codex-2.log
├── .agents/                # Agent workspace (gitignored)
│   └── worktree/
│       ├── task-001/
│       └── task-002/
└── orca.toml               # Project config (version controlled)
```

### 9.2 .gitignore

`orca init` appends:

```gitignore
# orca
.orca/
.agents/
```

`orca.toml` stays in version control for team-shared config.

## 10. Configuration

### 10.1 Project Config (orca.toml)

```toml
[daemon]
socket_path = ".orca/orca.sock"
max_workers = 4
log_level = "info"

[terminal]
provider = "ghostty"        # ghostty | iterm2 | manual
layout = "tabs"             # tabs | manual
auto_open = true            # auto-open pane/tab for new worker

[worker.codex]
command = "codex"
args = ["--full-auto", "--quiet"]
timeout_secs = 300          # per-task timeout
max_retries = 2

[isolation]
worktree_dir = ".agents/worktree"
default_strategy = "auto"   # auto | worktree | serial
target_branch = "main"

[escalation]
auto_approve = ["implementation_choice", "test_failure", "timeout"]
always_user = ["destructive_operation", "architecture_change"]
cc_first = ["conflict", "scope_exceeded"]
worker_timeout_secs = 300
max_retries = 2

[notification]
terminal_bell = true
system_notification = true  # macOS Notification Center
```

### 10.2 Global Config (~/.orca/config.toml)

```toml
[defaults]
terminal_provider = "ghostty"
max_workers = 4
log_level = "info"

# Global worker registry
[workers.codex]
command = "codex"
default_args = ["--full-auto", "--quiet"]

# Future
# [workers.gemini]
# command = "gemini"
# default_args = ["--sandbox"]
```

Project-level `orca.toml` overrides global config.

## 11. Installation

```bash
# One-line install (download pre-built binary from GitHub Releases)
curl -fsSL https://raw.githubusercontent.com/Nick2781/orca/main/install.sh | sh

# Install script does:
# 1. Detect OS + arch
# 2. Download pre-built binary from GitHub Releases
# 3. Place at ~/.orca/bin/orca
# 4. Append PATH if needed
# 5. Print "orca installed! Run: cd your-project && orca init"
```

### Quick Start

```bash
# 1. Install
curl -fsSL https://raw.githubusercontent.com/Nick2781/orca/main/install.sh | sh

# 2. Initialize project
cd my-project && orca init

# 3. Setup CC MCP integration
orca setup mcp

# 4. Use in CC — MCP tools auto-discovered
# CC calls orca_plan to dispatch tasks
```

## 12. Tech Stack

| Component | Choice | Rationale |
|-----------|--------|-----------|
| Language | Rust | Single binary, performance, safety |
| Async runtime | tokio | Industry standard for async Rust |
| CLI framework | clap | Ergonomic CLI arg parsing |
| IPC | Unix socket + JSON-RPC | Simple, reliable, debuggable |
| MCP SDK | rust-sdk or custom | MCP protocol over stdio |
| Serialization | serde + serde_json | Standard Rust JSON handling |
| Terminal UI | ratatui (optional) | Worker pane status bar |
| Git operations | git2 (libgit2) | Worktree management |
| Logging | tracing | Structured async-aware logging |
| Config | toml | Simple, readable config format |

## 13. Competitive Positioning

与现有方案对比，orca 的差异化：

| Dimension | Others | Orca |
|-----------|--------|------|
| Terminal | tmux required | Native terminal (Ghostty/iTerm2/any) |
| Orchestration | CC plugin or standalone TUI | CC as brain + daemon as middle layer |
| Communication | Text buffer / file system | Structured JSON-RPC over Unix socket |
| Escalation | All-or-nothing | 3-level (worker → CC auto → user) |
| Isolation | Always worktree | Smart: worktree or serial based on file overlap |
| Worker model | Hardcoded agents | Extensible Worker trait |
| Installation | npm/cargo + tmux + dependencies | Single binary, one curl |

## 14. Scope & Non-Goals

### In Scope (V1)

- orcad daemon with task scheduling and dependency graph
- Codex worker adapter
- MCP server for CC integration
- CLI for human/debug operations
- Ghostty + iTerm2 + manual terminal integration
- 3-level escalation with configurable routing
- Smart isolation (worktree vs serial)
- State persistence and recovery
- One-line install script

### Non-Goals (V1)

- Web UI / dashboard
- Remote/cloud execution
- Multi-repo orchestration
- Built-in CI integration
- Team/multi-user support
- Windows support (macOS + Linux first)
