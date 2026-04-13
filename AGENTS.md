# AGENTS.md

Orca — Multi-agent orchestrator. Rust project.

## Project Overview

Single Rust binary serving as daemon (`orca daemon start`), CLI client, and MCP server (`orca mcp-server`). Manages task lifecycle via Unix socket JSON-RPC.

## Architecture

```
src/main.rs         → CLI entry point (clap)
src/daemon/mod.rs   → Daemon struct, RPC handlers
src/daemon/server.rs → Unix socket IPC
src/daemon/state.rs  → State persistence (state.json + ledger.jsonl)
src/daemon/scheduler.rs → DAG dependency graph
src/worker/mod.rs   → Worker trait
src/worker/codex.rs → Codex CLI adapter
src/isolation.rs    → Git worktree management
src/terminal/       → Terminal pane management (ghostty/iterm2/manual)
src/mcp.rs          → MCP server (rmcp)
src/config.rs       → Configuration (orca.toml)
src/types.rs        → Core types (TaskSpec, Task, TaskState, Plan)
src/escalation.rs   → Escalation types and routing
src/protocol.rs     → JSON-RPC types
```

## Key Types

- `TaskSpec` — Task definition (id, title, description, files, isolation mode)
- `Task` — Runtime task (spec + state + worker assignment)
- `TaskState` — State machine: pending → assigned → running → done → review → accepted → completed
- `Plan` — Collection of tasks + dependency edges
- `Worker` trait — Async trait for agent adapters (spawn, dispatch, health_check, interrupt, cleanup)
- `WorkerMessage` — Parsed output: Done, Escalate, Blocked, Progress, Output

## Build & Test

```bash
cargo build              # Build
cargo test               # Run all tests
cargo clippy -- -D warnings  # Lint (must pass clean)
cargo fmt                # Format
```

## Rules

- Comments in English
- Follow existing patterns in the codebase
- Functions < 50 lines, files < 300 lines
- Run `cargo test` before committing
- Run `cargo clippy -- -D warnings` — must pass clean
- Commit format: `<type>: <description>`
- Do not modify files outside the task scope
- Do not add dependencies without justification

## Module Boundaries

- `cli/` and `mcp.rs` talk to daemon ONLY via IPC client (never import daemon internals)
- `worker/` does not depend on `daemon/`
- `isolation.rs` does not depend on `daemon/`
- `terminal/` does not depend on `daemon/`
- `daemon/mod.rs` orchestrates worker, isolation, terminal

## System Architecture

```
CC (Claude Code) — Brain
  │ MCP (stdio → Unix Socket)
  ▼
orcad (daemon) — Orchestration layer
  │ Task scheduling + escalation routing + isolation
  ▼
Worker (Codex × N) — Execution layer
  │ Code implementation + testing
  └ Terminal panes (Ghostty/iTerm2)
```

Three layers: CC does plan/review/decisions. Daemon does scheduling/state/routing. Workers do coding/testing/git.

## Common Operations

### Add a new RPC method

1. Add match arm in `daemon/mod.rs` `handle_request`
2. Implement handler function
3. Add corresponding MCP tool in `mcp.rs`
4. Add corresponding CLI command in `cli/`

### Add a new Worker type

1. Create file in `worker/`, implement `Worker` trait
2. Register in `daemon/mod.rs`

## Testing

Tests live in `tests/` directory:
- `tests/types_test.rs` — Core type tests
- `tests/config_test.rs` — Configuration tests
- `tests/state_test.rs` — State persistence tests
- `tests/protocol_test.rs` — IPC roundtrip tests
- `tests/scheduler_test.rs` — DAG scheduling tests
- `tests/codex_worker_test.rs` — Output parsing tests
- `tests/isolation_test.rs` — Isolation decision tests
- `tests/executor_test.rs` — Execution engine tests
- `tests/e2e_test.rs` — Full daemon lifecycle test
