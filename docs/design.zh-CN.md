# Orca — 多 Agent 编排器

**状态：Draft**

[English](design.md) | 中文

## 改动记录

| 日期 | 改动内容 |
|------|----------|
| 2026-04-13 | 初始版本 |

---

> CC (Claude Code) 作为主脑编排 Codex worker 的本地多 agent 协作框架。daemon 中心架构，原生终端集成，Rust 实现，单二进制分发。

## 1. 问题陈述

当前 AI coding agent 生态的核心矛盾：

- **Claude Code (CC)**：分析和表达能力强，但编码细节较差
- **Codex**：写代码能力不错，但表达啰嗦、业务理解差

现有多 agent 方案（oh-my-claudecode、claude-squad、CCCC 等）的不足：

| 问题 | 说明 |
|------|------|
| tmux 依赖 | 几乎所有方案都绑定 tmux，无法用原生终端分屏 |
| CC→Codex 编排缺失 | 没有项目完整实现 CC 做主脑 + Codex 做 worker |
| 提权机制粗糙 | 要么全自动要么全手动，无分级提权 |
| 通信脆弱 | 多数基于终端缓冲区文本解析，不可靠 |

## 2. 设计目标

| 目标 | 说明 |
|------|------|
| CC 做主脑 | plan、review、困难决策由 CC 处理 |
| Codex 做 worker | 代码实现、测试等纯执行工作由 Codex 处理 |
| 对等协作 | 不是纯主从，worker 有执行自主权，遇到问题可以回来讨论 |
| 并发执行 | 多个 worker 并行处理独立任务 |
| 分级提权 | worker 自行解决 → CC 自动判断 → 升级到用户 |
| 终端无关 | Ghostty/iTerm2/任何终端，不依赖 tmux |
| 可扩展 worker | 抽象 Worker trait，第一版只实现 Codex |
| 开源友好 | 单二进制分发，一行安装，配置简单 |

## 3. 架构

### 3.1 三层架构总览

```
┌──────────────────────────────────────────────────────────────┐
│  CC (Claude Code) — 大脑                                      │
│  通过 MCP tools 与 daemon 交互                                 │
│  职责：plan、review、困难决策、用户沟通                          │
└──────────────────┬───────────────────────────────────────────┘
                   │ MCP (Unix Socket)
                   ▼
┌──────────────────────────────────────────────────────────────┐
│  orcad (daemon) — 编排层                                      │
│                                                              │
│  ┌────────────┐ ┌────────────┐ ┌────────────┐ ┌───────────┐ │
│  │  任务队列   │ │  依赖图    │ │  隔离管理   │ │  提权路由  │ │
│  │  & 调度器  │ │           │ │           │ │           │ │
│  └────────────┘ └────────────┘ └────────────┘ └───────────┘ │
│  ┌────────────┐ ┌────────────┐ ┌────────────┐               │
│  │  Worker    │ │  状态存储   │ │  终端管理   │               │
│  │  注册表    │ │           │ │           │               │
│  └────────────┘ └────────────┘ └────────────┘               │
└──────┬──────────────┬──────────────┬────────────────────────┘
       │              │              │
       ▼              ▼              ▼
  ┌─────────┐   ┌─────────┐   ┌─────────┐
  │ Worker 1│   │ Worker 2│   │ Worker 3│    终端 pane
  │ (Codex) │   │ (Codex) │   │ (Codex) │    (Ghostty/iTerm2)
  │         │   │         │   │         │
  │ worktree│   │ worktree│   │ 同目录  │
  └─────────┘   └─────────┘   └─────────┘
```

### 3.2 分层职责

| 层 | 组件 | 职责 | 不做什么 |
|---|------|------|---------|
| 大脑 | CC (Claude Code) | plan 拆分、code review、方案决策、用户交互 | 不直接写代码 |
| 编排 | orcad (daemon) | 任务调度、隔离决策、提权路由、worker 生命周期、状态持久化 | 不做 LLM 推理 |
| 执行 | Worker (Codex) | 代码实现、测试、git 操作 | 不做架构决策 |

### 3.3 通信协议

| 路径 | 协议 | 说明 |
|------|------|------|
| CC → MCP server | stdio (stdin/stdout) | CC spawn `orca mcp-server` 进程 |
| MCP server → orcad | Unix Socket (JSON-RPC) | MCP server 作为 orcad 的薄客户端 |
| CLI → orcad | Unix Socket (JSON-RPC) | 人工操作 / 调试 |
| orcad → Worker | Unix Socket per worker | 结构化 task spec 下发 |
| Worker → orcad | 同上，反向 | 进度上报、结果提交、提权请求 |

注意：MCP 是 request-response 模式。CC 通过轮询 `orca_status` 发现待处理的提权请求。daemon 不会主动 push 给 CC。

## 4. 任务生命周期

### 4.1 Task Spec（任务规格）

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

### 4.2 状态机

```
                    CC 下发 plan
                          │
                          ▼
                    ┌──────────┐
                    │ pending  │ 待分配
                    └────┬─────┘
                         │ daemon 分配给 worker
                         ▼
                    ┌──────────┐
          ┌────────│ assigned │ 已分配
          │        └────┬─────┘
          │             │ worker 开始执行
          │             ▼
          │        ┌──────────┐
          │        │ running  │ 执行中 ◄────────┐
          │        └────┬─────┘                │
          │             │                      │
          │        ┌────┴────┐                 │
          │        ▼         ▼                 │
          │   ┌────────┐ ┌──────────┐          │
          │   │ done   │ │ blocked  │ 阻塞     │
          │   └───┬────┘ └────┬─────┘          │
          │       │           │                │
          │       │      ┌────┴────┐           │
          │       │      ▼         ▼           │
          │       │ ┌────────┐ ┌────────┐      │
          │       │ │提权→CC │ │提权→   │      │
          │       │ │(自动)  │ │ 用户   │      │
          │       │ └───┬────┘ └───┬────┘      │
          │       │     │          │            │
          │       │     │  决策完成              │
          │       │     └─────┬────┘           │
          │       │           └───► resumed ───┘
          │       ▼
          │  ┌──────────┐
          │  │ review   │ ← CC 审查产出
          │  └────┬─────┘
          │       │
          │  ┌────┴────┐
          │  ▼         ▼
          │ accepted  rejected ──► pending（返工）
          │  │
          ▼  ▼
     ┌──────────┐
     │ completed│ 完成
     └──────────┘
```

### 4.3 状态说明

| 状态 | 触发者 | 说明 |
|------|--------|------|
| pending | CC via plan | 任务创建，等待分配 |
| assigned | daemon | daemon 根据依赖图和 worker 空闲状态分配 |
| running | worker | worker 开始执行 |
| blocked | worker | 遇到需要决策的问题 |
| blocked → 提权 CC | daemon | daemon 判断 CC 能自动处理的提权 |
| blocked → 提权用户 | daemon/CC | CC 也不确定，升级到用户 |
| review | daemon | 执行完成，等待 CC review |
| accepted/rejected | CC | review 结果 |
| completed | daemon | 任务最终完成，变更已合并 |

### 4.4 依赖图执行

daemon 维护 DAG（有向无环图），自动调度：

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

## 5. 提权机制

### 5.1 三级提权模型

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

### 5.2 提权请求结构

Worker → daemon：

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

### 5.3 路由规则

| 类别 | 默认路由 | 说明 |
|------|---------|------|
| `implementation_choice` | CC 自动处理 | CC 根据 codebase 上下文判断 |
| `test_failure` | CC 自动处理 | CC 分析失败原因，调整 spec |
| `timeout` | CC 自动处理 | CC 决定重试/跳过/拆分 |
| `architecture_change` | 用户 | 引入新模式、新依赖 |
| `destructive_operation` | 用户 | push、删文件、改配置 |
| `scope_exceeded` | 用户 | 超出原始 plan 范围 |
| `conflict` | CC 先试 → 用户 | CC 先尝试解决，解决不了给用户 |

### 5.4 路由配置

```toml
[escalation]
auto_approve = ["implementation_choice", "test_failure", "timeout"]
always_user = ["destructive_operation", "architecture_change"]
cc_first = ["conflict", "scope_exceeded"]

[escalation.timeout]
worker_timeout_secs = 300
max_retries = 2
```

### 5.5 用户通知（Level 2）

| 方式 | 实现 |
|------|------|
| CC 终端 | CC 主 pane 显示提权请求，等待用户回复 |
| 系统通知 | macOS Notification Center（可选）|
| 声音 | Terminal bell（可选）|

## 6. Worker 抽象

### 6.1 Worker Trait

```rust
trait Worker: Send + Sync {
    /// 启动 worker 进程，返回连接句柄
    async fn spawn(&self, config: &WorkerConfig) -> Result<WorkerHandle>;

    /// 下发任务
    async fn dispatch(&self, handle: &WorkerHandle, task: &TaskSpec) -> Result<()>;

    /// 读取 worker 当前输出/状态
    async fn read_output(&self, handle: &WorkerHandle) -> Result<WorkerOutput>;

    /// 发送中断/取消信号
    async fn interrupt(&self, handle: &WorkerHandle) -> Result<()>;

    /// 健康检查
    async fn health_check(&self, handle: &WorkerHandle) -> Result<WorkerStatus>;

    /// 清理资源
    async fn cleanup(&self, handle: &WorkerHandle) -> Result<()>;
}
```

### 6.2 Codex 适配器

Codex CLI 通过 stdin/stdout 交互：

```
orcad
  │
  │ spawn: codex --full-auto -q
  │         在 worktree 目录或项目目录中
  ▼
┌──────────────────────────────────┐
│ Codex 进程                        │
│                                  │
│  stdin  ◄── 任务 prompt（从       │
│              TaskSpec 生成自然     │
│              语言 prompt）        │
│                                  │
│  stdout ──► 输出解析：            │
│              - 进度信号            │
│              - 提权请求（约定格式） │
│              - 完成信号            │
│  stderr ──► 错误捕获              │
└──────────────────────────────────┘
```

### 6.3 Prompt 生成

daemon 将 TaskSpec 转化为 Codex 能理解的 prompt：

```
You are working on task: {title}

## Description
{description}

## Files to modify
{context.files}

## Constraints
{context.constraints}

## References
{context.references}

## Working directory
{worktree_path}

## Rules
- When you need a decision, output: [ORCA:ESCALATE] {json}
- When done, output: [ORCA:DONE] {json summary}
- When you hit a blocker, output: [ORCA:BLOCKED] {json reason}
- Do not modify files outside the specified scope
```

### 6.4 输出标记

| 标记 | 含义 | daemon 行为 |
|------|------|------------|
| `[ORCA:DONE]` | 任务完成 | 状态 → review，通知 CC |
| `[ORCA:ESCALATE]` | 需要决策 | 状态 → blocked，路由提权 |
| `[ORCA:BLOCKED]` | 卡住了 | 状态 → blocked，分析原因 |
| `[ORCA:PROGRESS]` | 进度更新 | 更新状态，展示到终端 |
| 无标记输出 | 正常工作输出 | 透传到 terminal pane |

### 6.5 可扩展性

```rust
// V1
struct CodexWorker { /* ... */ }
impl Worker for CodexWorker { /* ... */ }

// 未来
struct ClaudeCodeWorker { /* ... */ }
impl Worker for ClaudeCodeWorker { /* ... */ }

struct GeminiCliWorker { /* ... */ }
impl Worker for GeminiCliWorker { /* ... */ }
```

新增 worker 类型 = 实现 `Worker` trait + 注册到 daemon 的 worker registry。

## 7. 隔离管理与终端集成

### 7.1 智能隔离决策

```
新任务到达
    │
    ▼
分析 task.context.files
    │
    ├─ 与所有运行中任务无文件交集
    │   └─► worktree 隔离（独立分支，并发执行）
    │
    ├─ 与某个运行中任务有文件交集
    │   └─► 串行排队（等那个任务完成后再开始）
    │
    └─ 无文件信息（如 "写单元测试"）
        └─► CC 决定（提权询问 CC 是否可并发）
```

### 7.2 Worktree 生命周期

```
任务分配（隔离方式=worktree）
    │
    ▼
git worktree add .agents/worktree/{task-id} -b orca/{task-id}
    │
    ▼
worker 在 worktree 目录内执行
    │
    ▼
任务完成 → CC review
    │
    ├─ accepted → 合并分支 → git worktree remove
    └─ rejected → 在同一 worktree 中返工
```

### 7.3 合并策略

| 场景 | 策略 |
|------|------|
| 无冲突 | daemon 自动 merge 到目标分支 |
| 有冲突 | 提权给 CC，CC 解决或升级到用户 |
| 依赖链 | 按 DAG 顺序依次 merge |

### 7.4 终端集成

#### 架构

```rust
trait Terminal: Send + Sync {
    /// 创建新 pane 并运行命令
    async fn create_pane(&self, cmd: &str, label: &str) -> Result<PaneId>;

    /// 关闭 pane
    async fn close_pane(&self, pane: &PaneId) -> Result<()>;

    /// 聚焦到指定 pane
    async fn focus_pane(&self, pane: &PaneId) -> Result<()>;
}
```

实现：

| 提供者 | 方式 | 自动分屏 |
|--------|------|---------|
| `GhosttyTerminal` | CLI `ghostty -e` | 新 tab（暂无 split API）|
| `ItermTerminal` | Python/AppleScript API | 分屏 pane |
| `ManualTerminal` | 用户运行 `orca worker connect` | 不适用 |

#### 配置

```toml
[terminal]
provider = "ghostty"        # ghostty | iterm2 | manual
layout = "tabs"             # tabs | manual
auto_open = true            # 新 worker 时自动开 pane/tab
```

#### Worker Pane 显示

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

顶部状态栏由 `orca worker connect` 进程渲染，下方透传 worker 原始输出。

## 8. MCP Server 与 CLI

### 8.1 MCP Tools（CC 调用）

| 工具 | 说明 | 参数 |
|------|------|------|
| `orca_plan` | 提交执行计划 | `tasks: TaskSpec[], dependencies: Edge[]` |
| `orca_status` | 查看全局状态 | `filter?: "running" \| "blocked" \| "all"` |
| `orca_task_detail` | 查看单个任务详情 | `task_id: string` |
| `orca_decide` | 回复提权请求 | `escalation_id, decision, reason` |
| `orca_review` | 提交审查结果 | `task_id, verdict: "accept" \| "reject", feedback?` |
| `orca_cancel` | 取消任务 | `task_id` |
| `orca_worker_list` | 查看 worker 状态 | - |
| `orca_merge` | 触发合并 | `task_ids: string[]` |

### 8.2 CC 典型工作流

```
CC: orca_plan({tasks: [...], dependencies: [...]})
     │
     ▼ daemon 开始调度，CC 继续做其他事

CC: orca_status()
     → "task-001: running, task-002: running, task-003: pending (blocked by 001)"

     ▼ CC 轮询状态，发现提权请求

CC: orca_status() → 看到待处理的 escalation
CC: orca_task_detail({task_id: "task-001"}) → 获取提权详情
CC: orca_decide({escalation_id: "esc-01", decision: "a", reason: "follow existing pattern"})

     ▼ task-001 完成

CC: orca_task_detail({task_id: "task-001"})
     → {status: "review", output: {files_changed: [...], tests_passed: true, diff_summary: "..."}}

CC: orca_review({task_id: "task-001", verdict: "accept"})

     ▼ 所有任务完成

CC: orca_merge({task_ids: ["task-001", "task-002", "task-003"]})
```

### 8.3 CLI 命令（人工/调试用）

```bash
# Daemon 管理
orca daemon start          # 启动 daemon（前台或后台）
orca daemon stop           # 停止 daemon
orca daemon status         # daemon 健康状态

# 任务管理
orca plan submit plan.json # 提交计划（MCP 的手动替代方式）
orca task list             # 任务列表
orca task detail <id>      # 任务详情
orca task cancel <id>      # 取消任务
orca task retry <id>       # 重试失败任务

# Worker 管理
orca worker list           # 查看所有 worker
orca worker connect <id>   # 连接到 worker pane（手动模式）
orca worker connect --auto # 自动分配下一个可用 worker
orca worker kill <id>      # 强制终止 worker

# 提权
orca escalation list                    # 查看待处理的提权
orca escalation decide <id> --choice a  # 手动决策

# 审查
orca review <task-id> --accept
orca review <task-id> --reject --feedback "missing error handling"

# 合并
orca merge <task-ids...>
orca merge --all-accepted  # 合并所有已通过审查的任务

# 初始化和配置
orca init                  # 初始化 orca.toml + .orca/
orca setup mcp             # 写入 MCP 配置到 ~/.claude/settings.json
orca config show           # 显示当前配置
```

### 8.4 MCP Server 启动方式

```jsonc
// CC 的 MCP 配置（~/.claude/settings.json）
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

`orca mcp-server` 启动时自动连接到 `orcad`（如果 daemon 没运行则自动启动）。MCP server 是 daemon 的一个薄客户端，不是独立进程。

## 9. 状态与存储

### 9.1 目录结构

```
项目根/
├── .orca/                  # orca daemon 状态（gitignored）
│   ├── orca.sock           # Unix socket
│   ├── state.json          # 当前任务/worker 状态（daemon 重启可恢复）
│   ├── ledger.jsonl        # append-only 事件日志（可审计）
│   └── logs/
│       ├── daemon.log
│       ├── codex-1.log     # worker 输出日志
│       └── codex-2.log
├── .agents/                # Agent 工作空间（gitignored）
│   └── worktree/
│       ├── task-001/
│       └── task-002/
└── orca.toml               # 项目配置（入版本控制）
```

### 9.2 .gitignore

`orca init` 自动追加：

```gitignore
# orca
.orca/
.agents/
```

`orca.toml` 保留在版本控制中，团队共享配置。

## 10. 配置

### 10.1 项目配置（orca.toml）

```toml
[daemon]
socket_path = ".orca/orca.sock"
max_workers = 4
log_level = "info"

[terminal]
provider = "ghostty"        # ghostty | iterm2 | manual
layout = "tabs"             # tabs | manual
auto_open = true            # 新 worker 时自动开 pane/tab

[worker.codex]
command = "codex"
args = ["--full-auto", "-q"]
timeout_secs = 300          # 单个任务超时
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

### 10.2 全局配置（~/.orca/config.toml）

```toml
[defaults]
terminal_provider = "ghostty"
max_workers = 4
log_level = "info"

# 全局 worker 注册（不用每个项目都配）
[workers.codex]
command = "codex"
default_args = ["--full-auto", "-q"]

# 未来
# [workers.gemini]
# command = "gemini"
# default_args = ["--sandbox"]
```

项目级 `orca.toml` 覆盖全局配置。

## 11. 安装

```bash
# 一行安装（从 GitHub Releases 下载预编译二进制）
curl -fsSL https://raw.githubusercontent.com/user/orca/main/install.sh | sh

# 安装脚本做的事：
# 1. 检测 OS + arch
# 2. 从 GitHub Releases 下载预编译二进制
# 3. 放到 ~/.orca/bin/orca
# 4. 追加 PATH（如果需要）
# 5. 打印 "orca installed! Run: cd your-project && orca init"
```

### 快速开始

```bash
# 1. 安装
curl -fsSL https://raw.githubusercontent.com/user/orca/main/install.sh | sh

# 2. 初始化项目
cd my-project && orca init

# 3. 配置 CC MCP 集成
orca setup mcp

# 4. 在 CC 中使用 — MCP tools 自动发现
# CC 调用 orca_plan 下发任务
```

## 12. 技术栈

| 组件 | 选型 | 理由 |
|------|------|------|
| 语言 | Rust | 单二进制分发，性能好，内存安全 |
| 异步运行时 | tokio | Rust 异步标准 |
| CLI 框架 | clap | 成熟的 CLI 参数解析 |
| IPC | Unix Socket + JSON-RPC | 简单、可靠、可调试 |
| MCP SDK | rmcp | 官方 Rust MCP SDK |
| 序列化 | serde + serde_json | Rust JSON 处理标准 |
| 终端 UI | ratatui（可选）| Worker pane 状态栏 |
| Git 操作 | git2 (libgit2) | worktree 管理 |
| 日志 | tracing | 结构化异步日志 |
| 配置 | toml | 简单可读的配置格式 |

## 13. 竞品定位

与现有方案对比，orca 的差异化：

| 维度 | 现有方案 | Orca |
|------|---------|------|
| 终端 | 依赖 tmux | 原生终端（Ghostty/iTerm2/任意）|
| 编排 | CC 插件或独立 TUI | CC 大脑 + daemon 中间层 |
| 通信 | 文本缓冲区 / 文件系统 | 结构化 JSON-RPC over Unix Socket |
| 提权 | 全有或全无 | 三级：worker → CC 自动 → 用户 |
| 隔离 | 总是 worktree | 智能：根据文件重叠决定 worktree 或串行 |
| Worker 模型 | 硬编码 agent | 可扩展 Worker trait |
| 安装 | npm/cargo + tmux + 依赖 | 单二进制，一行 curl |

## 14. 范围与非目标

### V1 范围内

- orcad daemon：任务调度 + 依赖图
- Codex worker 适配器
- MCP server：CC 集成
- CLI：人工操作/调试
- Ghostty + iTerm2 + manual 终端集成
- 三级提权 + 可配置路由
- 智能隔离（worktree vs 串行）
- 状态持久化与恢复
- 一行安装脚本

### V1 非目标

- Web UI / 仪表盘
- 远程/云端执行
- 多仓库编排
- 内置 CI 集成
- 团队/多用户支持
- Windows 支持（macOS + Linux 优先）
