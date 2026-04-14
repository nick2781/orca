# Orca TODO — 待解决问题

以下问题需要继续完成。按优先级排序。

## P0: 窗口定位 — 分屏跑到错误窗口 [Done]

**状态**: 已解决。`src/terminal/ghostty_origin.rs` 现在会优先用 CLI 参数、已保存 ID、项目目录匹配来解析 origin terminal，只在最后才 fallback 到 front window。

**结果**:
- Worker 分屏现在会优先落到项目对应的 Ghostty terminal。
- Escalation 通知不再依赖不可靠的 front-window 定位。

**关键文件**: `src/main.rs`, `src/terminal/ghostty.rs`, `src/terminal/ghostty_origin.rs`

## P1: sandbox 审批的实时性 [In Progress]

**当前状态**: 已扩展 `src/daemon/executor.rs` 中的审批检测模式。2026-04-14 的 session logs 里出现了更多变体，例如：
- `sandbox blocks ... requesting elevated execution`
- `sandbox restriction ... requesting permission`
- `blocked by sandboxing`
- `need(s) elevated access ... writable sandbox`

**当前方案**: 继续轮询 session log（1 秒间隔），检测到审批请求后对 worktree 任务自动发送 `y`。

**剩余问题**:
- 轮询本身仍有 1 秒级延迟。
- 更彻底的方案仍然是 file watch 或将 Codex approval 接到 daemon 的 escalation 系统。

**关键文件**: `src/daemon/executor.rs`

## P2: escalation 通知到主窗口 [Mostly Done]

**当前状态**: 随着 P0 解决，`notify_escalation` 的 focus target 已基本修复，通知现在应回到正确的 Ghostty terminal。

**剩余改进**:
- 通知内容还可以更具体，例如直接带上 `orca escalation decide <id> approve`。
- 仍依赖 macOS notification + focus，而不是真正的双向 MCP push。

**关键文件**: `src/daemon/executor.rs`

## P3: codex worktree git 操作的 sandbox 问题 [In Progress]

**当前状态**: 问题已定位得更清楚。2026-04-14 的 worktree session logs 显示：
- 被拒绝的路径是 parent repo 的 `.git/worktrees/.../index.lock`
- 拒绝路径已经是 `/private/tmp/...`，所以不是 `/tmp` vs `/private/tmp` 的 canonicalization 问题
- Codex 记录的 `turn_context` writable roots 仍未覆盖 parent repo，因此 `--add-dir=<project>` 还不足以授权 shared git metadata 写入

**当前方案**: worktree 任务检测到审批请求后自动发送 `y`。

**下一步**:
- 继续确认 Codex CLI 是否有比 `--add-dir` 更强的 sandbox root 配置
- 调查能否通过 `-c` / `.codex/config.toml` 精确放开 parent repo `.git`
- 如果上面都不行，保留 auto-approval 作为 worktree git 的兼容层

**关键文件**: `src/daemon/executor.rs`

## P4: MCP 双向通信 [Open]

**当前状态**: 还没有实质实现。当前仍然依赖 `orca_status` 轮询和终端/macOS 通知补足。

**可能方案**:
- MCP 协议本身不支持 server push
- 可以让 CC 注册一个 webhook/callback
- 或者用 AppleScript 直接向 CC 的终端发送消息
- 或者让 daemon 写一个 notification 文件，CC 通过 file watch 感知

**关键文件**: `src/mcp.rs`, `src/daemon/mod.rs`
