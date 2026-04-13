# CLAUDE.md

Orca — 多 Agent 编排器。Claude Code 做大脑，Codex 做 worker。

@AGENTS.md

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

## 当前状态

- 基础框架: 完成（types, config, IPC, scheduler, state, worker trait, isolation, terminal, CLI, MCP）
- 未完成: 任务执行引擎 (execution loop)、提权路由实现、Codex 输出适配
- 实验性项目，不可用于生产

## 常见操作

### 新增 RPC 方法

1. `daemon/mod.rs` handle_request match 加分支
2. 实现 handler 函数
3. `mcp.rs` 加对应 MCP tool
4. `cli/` 加对应 CLI 命令

### 新增 Worker 类型

1. `worker/` 下创建新文件，实现 `Worker` trait
2. `daemon/mod.rs` 中注册
