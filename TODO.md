# Orca TODO

## P0: Worker IPC communication [Next]

Replace session log polling + keystroke injection with direct daemon IPC.

### New CLI subcommands (`orca worker`)

```bash
orca worker exec --task-id <id> "git add hello.py"   # daemon executes on behalf
orca worker done --task-id <id> --files hello.py      # report completion
orca worker progress --task-id <id> "creating file"   # report progress
orca worker escalate --task-id <id> "need decision"   # escalate to CC
```

### RPC handlers in daemon

- `worker_exec`: run shell command in worktree, return stdout/stderr
- `worker_done`: transition task to Done/Review, close pane
- `worker_progress`: log event, update task metadata
- `worker_escalate`: create EscalationRequest, notify CC terminal

### Codex prompt replaces AGENTS.md template

All orca instructions go in the codex prompt argument:
- Task description + constraints
- `orca worker exec` for git/sandbox-limited commands
- `orca worker done` when finished
- No modification to project's AGENTS.md

### Files to change

- `src/cli/worker_cmd.rs`: add Exec/Done/Progress/Escalate actions
- `src/daemon/mod.rs`: add RPC handlers
- `src/daemon/executor.rs`: update start_task to pass prompt (not write AGENTS.md)
- `src/worker/codex.rs`: remove generate_agents_md, agents_md_template.txt
- `src/main.rs`: handle new worker subcommands

## P1: Session log monitor as fallback [Deferred]

Keep session log monitoring as fallback when codex doesn't use orca CLI:
- Reduce to background safety net, not primary detection
- Remove keystroke injection code
- Remove is_approval_request_message (no longer needed)

## P2: Split pane focus [Small fix]

After creating a split pane, focus back to origin terminal.
Code is written but needs verification.

## P3: MCP push notifications [Open]

Daemon cannot push to CC (MCP is request-response only).
Current workaround: AppleScript focus + macOS notification.
Possible future: MCP subscription/notification extension.
