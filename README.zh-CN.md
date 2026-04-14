# Orca

> **这是一个实验性项目，正在积极开发中。尚不能用于日常工作。**

多 Agent 编排器：Claude Code 做大脑，Codex 做执行。

[English](README.md) | 中文

Orca 让 Claude Code (CC) 充当大脑 —— 负责规划、审查和决策 —— 同时将实现任务分发给多个并行运行的 Codex worker。一个轻量 daemon 通过 Unix Socket 结构化消息协调一切。

## 当前范围

- Daemon + Unix Socket IPC、状态持久化、任务调度
- 从 plan 提交到 review 的任务生命周期
- 基于文件重叠的隔离决策（worktree / same-dir）
- Claude Code 可用的 MCP server
- Codex worker 在用户可见终端分屏中执行
- 基于 Codex session log 的完成检测
- 主动把提权通知带回主终端

目前还没有：

- 真实 Codex 工作流的端到端验证
- WezTerm / kitty / Zellij 适配器
- Ghostty 原生 CLI split API（现在仍依赖 AppleScript）

## 终端支持

Orca 需要终端支持**可编程分屏** —— 从外部进程创建新的 split pane 并执行命令。

| 终端 | 分屏 API | 状态 |
|------|---------|------|
| **WezTerm** | `wezterm cli split-pane -- cmd` | 计划支持 |
| **kitty** | `kitten @ launch --type=window cmd` | 计划支持 |
| **iTerm2** | AppleScript / Python API | 支持 |
| **Ghostty** | AppleScript 分屏与聚焦 | 支持 |
| **Zellij** | `zellij action new-pane -- cmd` | 计划支持 |
| **任何终端** | 手动模式（用户自己分屏 + 运行命令）| 支持的兜底方案 |

**Ghostty 用户**：Orca 现在已经能通过 AppleScript 在 Ghostty 里分屏、聚焦和定位终端。未来如果 Ghostty 提供原生 CLI split API，Orca 会优先切过去。见 [ghostty-org/ghostty#2353](https://github.com/ghostty-org/ghostty/discussions/2353)。

## 三级提权

当发生提权时，daemon 会主动通知主 agent：
- 聚焦 CC 的终端窗格
- 发送 macOS 系统通知，显示提权摘要

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

Pre-release / 当前分支：

```bash
curl -fsSL https://raw.githubusercontent.com/Nick2781/orca/main/install.sh | sh
```

稳定 release 通道：

```bash
curl -fsSL https://github.com/Nick2781/orca/releases/latest/download/install.sh | sh
```

`main` 分支安装器在还没有 GitHub release 时会自动回退到源码构建。等 tagged release 可用后，`releases/latest/download/install.sh` 就是更稳定的安装入口。

或从源码构建：

```bash
git clone https://github.com/Nick2781/orca.git
cd orca && cargo build --release
```

## 技术栈

Rust, tokio, clap, serde, rmcp (MCP SDK), tracing

## License

MIT
