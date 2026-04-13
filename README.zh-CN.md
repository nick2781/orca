# Orca

多 Agent 编排器：Claude Code 做大脑，Codex 做执行。

[English](README.md) | 中文

Orca 让 Claude Code (CC) 充当大脑 —— 负责规划、审查和决策 —— 同时将实现任务分发给多个并行运行的 Codex worker。一个轻量 daemon 通过结构化消息协调一切，不依赖终端缓冲区的文本解析。

## 为什么做 Orca？

当前 AI coding agent 生态存在核心矛盾：

- **Claude Code**：分析和表达能力强，但编码细节较差
- **Codex**：写代码能力不错，但表达啰嗦、业务理解差

现有方案（oh-my-claudecode、claude-squad、CCCC 等）的不足：

| 问题 | Orca 的方案 |
|------|------------|
| 几乎所有工具都依赖 tmux | 原生终端支持（Ghostty、iTerm2、任意终端）|
| 没有 CC→Codex 编排 | CC 做大脑，Codex 做 worker，daemon 做中间层 |
| 权限要么全自动要么全手动 | 三级提权：worker 自行解决 → CC 自动判断 → 升级到用户 |
| 通信基于终端缓冲区文本解析 | 结构化 JSON-RPC over Unix Socket |

## 安装

```bash
curl -fsSL https://raw.githubusercontent.com/Nick2781/orca/main/install.sh | sh
```

## 快速开始

```bash
# 在项目中初始化
cd my-project
orca init

# 配置 Claude Code MCP 集成
orca setup mcp

# 启动 daemon
orca daemon start
```

在 Claude Code 中，orca MCP tools 自动可用：

| MCP Tool | 说明 |
|----------|------|
| `orca_plan` | 提交执行计划（含任务和依赖关系）|
| `orca_status` | 查看全局状态和待处理的提权请求 |
| `orca_task_detail` | 查看单个任务详情 |
| `orca_decide` | 回复 worker 的提权请求 |
| `orca_review` | 审查完成的任务（accept / reject）|
| `orca_cancel` | 取消任务 |
| `orca_worker_list` | 查看 worker 状态 |
| `orca_merge` | 合并已通过审查的分支 |

## 架构

```
┌──────────────────────────────────────────────┐
│  CC (Claude Code) — 大脑                      │
│  规划、审查、困难决策、用户沟通                   │
└──────────────────┬───────────────────────────┘
                   │ MCP (stdio → Unix Socket)
                   ▼
┌──────────────────────────────────────────────┐
│  orcad (daemon) — 编排层                      │
│  任务调度、提权路由、隔离决策、状态持久化          │
└──────┬──────────────┬──────────────┬─────────┘
       ▼              ▼              ▼
  ┌─────────┐   ┌─────────┐   ┌─────────┐
  │ Worker 1│   │ Worker 2│   │ Worker 3│
  │ (Codex) │   │ (Codex) │   │ (Codex) │
  └─────────┘   └─────────┘   └─────────┘
  Terminal panes (Ghostty / iTerm2 / 任意终端)
```

### 三层职责

| 层 | 组件 | 职责 | 不做什么 |
|---|------|------|---------|
| 大脑 | CC (Claude Code) | plan 拆分、code review、方案决策、用户交互 | 不直接写代码 |
| 编排 | orcad (daemon) | 任务调度、隔离决策、提权路由、worker 生命周期、状态持久化 | 不做 LLM 推理 |
| 执行 | Worker (Codex) | 代码实现、测试、git 操作 | 不做架构决策 |

## 三级提权机制

```
Worker 遇到问题
      │
      ▼
Level 0: Worker 自行解决
  编译错误自己修、简单测试失败自己重试
      │ 解决不了
      ▼
Level 1: Daemon → CC 自动处理
  方案抉择、超时重试、测试失败分析
      │ CC 也不确定
      ▼
Level 2: CC → 用户
  架构变更、危险操作、超出 plan 范围
```

提权路由可通过配置文件自定义：

```toml
[escalation]
auto_approve = ["implementation_choice", "test_failure", "timeout"]
always_user = ["destructive_operation", "architecture_change"]
cc_first = ["conflict", "scope_exceeded"]
```

## 智能隔离

daemon 根据任务文件是否重叠自动决定隔离方式：

| 情况 | 策略 |
|------|------|
| 与运行中的任务无文件交集 | worktree 隔离（独立分支，并发执行）|
| 与运行中的任务有文件交集 | 串行队列（等前面的任务完成）|
| 无文件信息 | 提权给 CC 判断是否可并发 |

Worktree 统一放在 `.agents/worktree/` 下，不污染项目目录。

## 配置

项目级 `orca.toml`：

```toml
[daemon]
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
```

全局配置 `~/.orca/config.toml` 提供默认值，项目级配置覆盖全局。

## CLI 命令

```bash
# Daemon 管理
orca daemon start|stop|status

# 任务管理
orca task list [--filter running|blocked|pending]
orca task detail <id>
orca task cancel <id>
orca task retry <id>

# Worker 管理
orca worker list
orca worker connect [--id <id>] [--auto]
orca worker kill <id>

# 计划提交
orca plan submit <file.json>

# 审查
orca review accept <task-id>
orca review reject <task-id> [--feedback "..."]

# 合并
orca merge <task-ids...>
orca merge --all-accepted

# 提权管理
orca escalation list
orca escalation decide <id> --choice <value>

# 初始化和配置
orca init
orca setup mcp
orca config
```

## 目录结构

```
项目根/
├── .orca/                  # orca daemon 状态（gitignored）
│   ├── orca.sock           # Unix socket
│   ├── state.json          # 任务/worker 状态（重启恢复）
│   ├── ledger.jsonl        # 事件日志（可审计）
│   └── logs/               # daemon 和 worker 日志
├── .agents/                # Agent 工作空间（gitignored）
│   └── worktree/           # git worktree 目录
├── orca.toml               # 项目配置（入版本控制）
```

## Worker 可扩展性

第一版只实现 Codex adapter，但 Worker trait 支持扩展：

```rust
// 实现 Worker trait 即可接入新的 agent
trait Worker: Send + Sync {
    async fn spawn(&self, worker_id: &str, work_dir: &str) -> Result<()>;
    async fn dispatch(&self, worker_id: &str, task: &TaskSpec) -> Result<()>;
    async fn health_check(&self, worker_id: &str) -> Result<WorkerStatus>;
    async fn interrupt(&self, worker_id: &str) -> Result<()>;
    async fn cleanup(&self, worker_id: &str) -> Result<()>;
}
```

未来可扩展支持 Gemini CLI、Claude Code worker、Aider 等。

## 技术栈

| 组件 | 选型 | 理由 |
|------|------|------|
| 语言 | Rust | 单二进制分发，性能好，安全 |
| 异步运行时 | tokio | Rust 异步标准 |
| CLI | clap | 成熟的 CLI 解析框架 |
| IPC | Unix Socket + JSON-RPC | 简单、可靠、可调试 |
| MCP | rmcp | 官方 Rust MCP SDK |
| Git | git2 (libgit2) | worktree 管理 |
| 日志 | tracing | 结构化异步日志 |

## 与竞品对比

| 维度 | 现有方案 | Orca |
|------|---------|------|
| 终端 | 依赖 tmux | 原生终端（Ghostty/iTerm2/任意）|
| 编排 | CC 插件或独立 TUI | CC 大脑 + daemon 中间层 |
| 通信 | 文本缓冲区 / 文件系统 | 结构化 JSON-RPC over Unix Socket |
| 提权 | 全有或全无 | 三级：worker → CC → 用户 |
| 隔离 | 总是 worktree | 智能：根据文件重叠决定 worktree 或串行 |
| Worker | 硬编码 | 可扩展 Worker trait |
| 安装 | npm/cargo + tmux + 依赖 | 单二进制，一行 curl |

## License

MIT
