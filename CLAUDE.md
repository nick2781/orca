# CLAUDE.md

Orca — 多 Agent 编排器。Claude Code 做大脑，Codex 做 worker。

@AGENTS.md

## 项目结构

```
orca/
├── src/
│   ├── main.rs              # CLI 入口 (clap)
│   ├── lib.rs               # 模块导出
│   ├── config.rs            # orca.toml + ~/.orca/config.toml 配置解析
│   ├── types.rs             # 核心类型：TaskSpec, Task, TaskState, Plan, Edge
│   ├── escalation.rs        # 提权：EscalationRequest, routing rules
│   ├── protocol.rs          # JSON-RPC 2.0 request/response 类型
│   ├── isolation.rs         # Git worktree 隔离管理
│   ├── mcp.rs               # MCP server (rmcp)，CC 通过此与 daemon 交互
│   ├── cli/                 # CLI 子命令
│   │   ├── mod.rs           # Commands enum
│   │   ├── daemon_cmd.rs    # daemon start/stop/status
│   │   ├── task_cmd.rs      # task list/detail/cancel/retry
│   │   ├── worker_cmd.rs    # worker list/connect/kill
│   │   ├── plan_cmd.rs      # plan submit
│   │   ├── review_cmd.rs    # review accept/reject
│   │   └── setup_cmd.rs     # init, setup mcp
│   ├── daemon/              # Daemon 核心
│   │   ├── mod.rs           # Daemon struct, RPC handler dispatch, 主循环
│   │   ├── scheduler.rs     # DAG 依赖图 + 任务调度
│   │   ├── state.rs         # state.json + ledger.jsonl 持久化
│   │   └── server.rs        # Unix Socket JSON-RPC server/client
│   ├── worker/              # Worker 抽象
│   │   ├── mod.rs           # Worker trait, WorkerMessage enum
│   │   └── codex.rs         # Codex adapter: spawn, parse output, prompt gen
│   └── terminal/            # 终端集成
│       ├── mod.rs           # Terminal trait + factory
│       ├── ghostty.rs       # Ghostty 分屏 (ghostty +action)
│       ├── iterm.rs         # iTerm2 分屏 (AppleScript)
│       └── manual.rs        # 手动模式 fallback
├── tests/                   # 集成测试
├── docs/
│   ├── design.md            # 设计规格 (English)
│   ├── design.zh-CN.md      # 设计规格 (中文)
│   └── plans/               # 实施计划
├── orca.toml.example        # 配置示例
├── install.sh               # 一行安装脚本
└── .github/workflows/       # CI (test+clippy+fmt) + Release
```

## 架构

```
CC (Claude Code) — 大脑
  │ MCP (stdio → Unix Socket)
  ▼
orcad (daemon) — 编排层
  │ 任务调度 + 提权路由 + 隔离决策
  ▼
Worker (Codex × N) — 执行层
  │ 代码实现 + 测试
  └ Terminal panes (Ghostty/iTerm2)
```

三层职责：
- **CC**: plan 拆分、code review、方案决策
- **orcad**: 任务调度、状态持久化、提权路由、worker 生命周期
- **Worker**: 代码实现、测试、git 操作

## 核心概念

### 任务状态机

```
pending → assigned → running → done → review → accepted → completed
                       ↕                         ↓
                    blocked                    rejected → pending (rework)
```

### 通信路径

| 路径 | 协议 |
|------|------|
| CC → MCP server | stdio |
| MCP server → orcad | Unix Socket JSON-RPC |
| CLI → orcad | Unix Socket JSON-RPC |
| orcad → Worker | 进程 stdin/stdout |

### 三级提权

- Level 0: Worker 自行解决
- Level 1: CC 自动判断
- Level 2: 升级到用户

## 开发规范

### 技术栈

- Rust 2021 edition
- tokio (async runtime)
- clap (CLI), serde (serialization), rmcp (MCP)
- tracing (logging)

### 构建和测试

```bash
cargo build              # 编译
cargo test               # 测试
cargo clippy -- -D warnings  # lint
cargo fmt                # 格式化
```

### 代码风格

- 注释: English
- 文档: 中文为主，技术术语保持 English
- Commit: `<type>: <description>` (English)
- 函数: 单一职责，<50 行
- 文件: 单一模块，<300 行

### 模块边界

```
cli → daemon (通过 IPC client)
mcp → daemon (通过 IPC client)
daemon/mod.rs → daemon/scheduler + state + server
daemon/mod.rs → worker + isolation + terminal
worker/codex.rs 不依赖 daemon
isolation.rs 不依赖 daemon
terminal/*.rs 不依赖 daemon
```

## 当前状态

- 基础框架: 完成（types, config, IPC, scheduler, state, worker trait, isolation, terminal, CLI, MCP）
- 未完成: 任务执行引擎 (execution loop)、提权路由实现、Codex 输出适配
- 实验性项目，不可用于生产

## 常见操作

### 新增 RPC 方法

1. 在 `daemon/mod.rs` 的 `handle_request` match 中加新分支
2. 实现 handler 函数
3. 在 `mcp.rs` 中加对应的 MCP tool
4. 在 `cli/` 中加对应的 CLI 命令

### 新增 Worker 类型

1. 在 `worker/` 下创建新文件
2. 实现 `Worker` trait
3. 在 `daemon/mod.rs` 中注册
