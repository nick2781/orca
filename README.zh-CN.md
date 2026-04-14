# Orca

> **这是一个实验性项目，正在积极开发中。尚不能用于日常工作。**

多 Agent 编排器：Claude Code 做大脑，Codex 做执行。

[English](README.md) | 中文

Orca 让 Claude Code (CC) 充当大脑 —— 负责规划、审查和决策 —— 同时将实现任务分发给多个并行运行的 Codex worker。一个轻量 daemon 通过 Unix Socket 结构化消息协调一切。

## 项目状态

### 已完成

- Daemon + Unix Socket IPC（启动/停止/PID 管理）
- 任务生命周期：plan 提交 → DAG 调度 → worker 分派 → review
- 智能隔离：基于文件重叠自动选择 worktree 或串行
- 三级提权路由 + 可配置规则
- MCP Server（8 个 tools，CC 集成）
- CLI（11 个子命令）
- 状态持久化（state.json + append-only ledger）
- 92 个测试通过，clippy clean

### 进行中

- **终端集成**：Worker 需要在可见的终端分屏中运行，用户实时观察 Codex 工作。这需要终端支持可编程分屏 —— 见[终端支持](#终端支持)。
- **Ghostty CLI API**：计划给 [Ghostty](https://github.com/ghostty-org/ghostty) 提 PR 增加 `ghostty +action new_split -- <command>` 支持。见 [ghostty-org/ghostty#2353](https://github.com/ghostty-org/ghostty/discussions/2353)。

### 未完成

- 端到端 Codex 工作流实测
- CC→daemon 提权反馈闭环
- `orca worker run <task-id>` 手动 pane 执行命令

## 为什么做 Orca？

| 问题 | Orca 的方案 |
|------|------------|
| 几乎所有工具都依赖 tmux | 终端适配层（支持有分屏 API 的终端）|
| 没有 CC→Codex 编排 | CC 做大脑，Codex 做 worker，daemon 做中间层 |
| 权限要么全自动要么全手动 | 三级提权：worker → CC 自动判断 → 用户确认 |
| 通信基于终端缓冲区文本解析 | 结构化 JSON-RPC over Unix Socket |

## 架构

```
┌──────────────────────────────────────────────┐
│  CC (Claude Code) — 大脑                      │
│  规划、审查、困难决策                           │
└──────────────────┬───────────────────────────┘
                   │ MCP (stdio → Unix Socket)
                   ▼
┌──────────────────────────────────────────────┐
│  orcad (daemon) — 编排层                      │
│  任务调度、提权路由、隔离决策、状态持久化         │
└──────┬──────────────┬──────────────┬─────────┘
       ▼              ▼              ▼
  ┌─────────┐   ┌─────────┐   ┌─────────┐
  │ Worker 1│   │ Worker 2│   │ Worker 3│
  │ (Codex) │   │ (Codex) │   │ (Codex) │
  └─────────┘   └─────────┘   └─────────┘
  终端分屏（用户实时观察）
```

Worker 在**用户可见的终端分屏**中运行，不是隐藏子进程。用户实时看到 Codex 工作。任务完成通过检查 git 状态判断。

## 终端支持

Orca 需要终端支持**可编程分屏** —— 从外部进程创建新的 split pane 并执行命令。

| 终端 | 分屏 API | 状态 |
|------|---------|------|
| **WezTerm** | `wezterm cli split-pane -- cmd` | 支持 |
| **kitty** | `kitten @ launch --type=window cmd` | 支持 |
| **iTerm2** | AppleScript / Python API | 支持 |
| **Ghostty** | 无（暂时）| [PR 计划中](https://github.com/ghostty-org/ghostty/discussions/2353) |
| **Zellij** | `zellij action new-pane -- cmd` | 计划支持 |
| **任何终端** | 手动模式（用户自己分屏 + 运行命令）| 兜底方案 |

**Ghostty 用户**：Ghostty 内部有 `new_split` action 但没有外部 API。我们计划给 Ghostty 贡献这个功能。在此之前，orca 会打印命令供你手动在 Ghostty 分屏中执行。

## 三级提权

```
Worker 遇到问题
      │
Level 0: Worker 自行解决（编译错误、简单失败）
      │ 解决不了
Level 1: Daemon → CC 自动处理（方案选择、超时重试）
      │ CC 也不确定
Level 2: CC → 用户（架构变更、危险操作）
```

## 智能隔离

| 情况 | 策略 |
|------|------|
| 与运行中任务无文件交集 | Git worktree 隔离（并发）|
| 有文件交集 | 串行队列 |
| 无文件信息 | 提权给 CC 判断 |

## 安装

```bash
curl -fsSL https://raw.githubusercontent.com/Nick2781/orca/main/install.sh | sh
```

或从源码构建：

```bash
git clone https://github.com/Nick2781/orca.git
cd orca && cargo build --release
```

## Roadmap

- [ ] Ghostty CLI API PR（[ghostty-org/ghostty#2353](https://github.com/ghostty-org/ghostty/discussions/2353)）
- [ ] WezTerm / kitty adapter 实现
- [ ] `orca worker run <task-id>` 手动 pane 执行
- [ ] 端到端 Codex 工作流实测
- [ ] CC 提权反馈闭环
- [ ] Zellij adapter

## 技术栈

Rust, tokio, clap, serde, rmcp (MCP SDK), tracing

## License

MIT
