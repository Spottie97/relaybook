# AgentMesh

**AgentMesh is a shared coordination layer for coding agents.** A single Rust binary
that keeps tasks, progress, and decisions in one SQLite ledger, so agents working in
the same repo stop stepping on each other.

Graphify maps the code — the "what is this and where" (the map). AgentMesh tracks the
work — the "who is doing what and why" (the bus). Use Graphify for structure, AgentMesh
for state.

## Quick start
`agentmesh init` → `agentmesh task create auth --title "Add auth"` →
`agentmesh task claim auth --by cursor` → work → `agentmesh progress report auth
--note "..."` → `agentmesh task done auth`

Any agent that can run a shell command can join. No protocol, no server, no lock-in.

## Build

```sh
cargo build --release
```

The binary is `target/release/agentmesh`. It is a single static executable: SQLite is
compiled in via `rusqlite` with the `bundled` feature. There is no server and no daemon.

## Ledger

`agentmesh init` creates `.agentmesh/ledger.db` at the git repository root (or in the
current directory when you are not in a work tree). Later commands walk up from the
working directory until they find that file.

The database runs in WAL mode with a 5 second busy timeout. Tasks, steps, and decisions
are the current projection. The `events` table is append-only (`task_created`, `claimed`,
`progress`, `decision`, `status_changed`); AgentMesh never updates or deletes a row there.
`agentmesh task block <slug> --reason "..."` sets `status=blocked` on an open or claimed
task and records why. Blocking a task that is already blocked succeeds and writes no new
event. A done task cannot be blocked.

WAL sidecar files (`.db-wal`, `.db-shm`) are gitignored. The ledger file itself is safe to
commit after a command exits; each write checkpoints so the `.db` file holds the data.

## Commands

Every command prints plain text by default. Add `--json` for a single JSON envelope on
stdout. Exit `0` on success, `1` on an operational failure, and `2` on a usage error or
when the ledger is missing.

```text
agentmesh init
agentmesh context [slug]
agentmesh task create <slug> --title "..."
agentmesh task list [--status open|claimed|done|blocked]
agentmesh task claim <slug> --by <agent>
agentmesh task block <slug> --reason "..."
agentmesh task done <slug>
agentmesh step add <slug> "..."
agentmesh step check <slug> <n>
agentmesh progress report <slug> --note "..."
agentmesh decision log <slug> --why "..." --by <agent>
agentmesh install --claude --cursor
```

Slugs match `[a-z0-9]+(-[a-z0-9]+)*`. Checklist positions start at 1.
`install` appends the planner block to `CLAUDE.md` (`--claude`) and the executor block
to `AGENTS.md` (`--cursor`). A second run does not change a file that already has the
AgentMesh markers or the coordination heading. Text outside the block is kept.

## JSON

Success and failure both use one envelope:

```json
{"ok": true, "data": {}, "error": null}
{"ok": false, "data": null, "error": "already claimed"}
```

`--json` writes that object to stdout and nothing else. Human-readable errors go to stderr.

## Claims and progress

`task claim` takes a write lock and runs `UPDATE ... WHERE status='open'`. One agent
wins. A missing slug fails with `no such task: <slug>`. Any other status, including a
claim that lost the race, fails with `already claimed` and exit code 1.

`progress report` reads `git rev-parse HEAD` when that succeeds and stores the commit
in the event payload. A second report for the same task and commit fails with
`already reported for <sha>`. Outside a git checkout the commit is null, and each report
is appended. `task done` on a task that is already done succeeds and does not write
another event.

## Library

`agentmesh-core` owns the ledger. A future MCP adapter, or any other front end, should
call that crate. It should not invent a second protocol or a second store. The `agentmesh`
binary is only the CLI over the same API.

## License

Apache-2.0. See [LICENSE](LICENSE).
