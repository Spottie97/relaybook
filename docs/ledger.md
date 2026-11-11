# Ledger

`relaybook init` creates `.relaybook/ledger.db` at the git repository root, or in the current directory when you are not in a work tree. Later commands walk up from the working directory until they find that file.

```sh
relaybook init
```

The database runs in WAL mode with a 5 second busy timeout. Tasks, steps, and decisions are the current projection. The `events` table is append-only. Relaybook never updates or deletes a row there.

`relaybook setup` gitignores WAL sidecar files (`.relaybook/*.db-wal`, `.relaybook/*.db-shm`) and `.relaybook/runs/` inside a `# relaybook:start` block. `.relaybook/ledger.db` stays committable. The ledger file itself is safe to commit after a command exits. Each write checkpoints so the `.db` file holds the data.

`relaybook-core` owns the ledger. A future adapter should call that crate. It should not invent a second protocol or a second store. The `relaybook` binary is the CLI over the same API.

## JSON

Success and failure both use one envelope. `--json` writes that object to stdout and nothing else.

```json
{"ok": true, "data": {}, "error": null}
{"ok": false, "data": null, "error": "already claimed"}
```

```sh
relaybook status --json
```

## Tables

Schema version is `PRAGMA user_version` 6. A new ledger is created at that version. Opening an older file migrates it forward.

| Table | Role |
| --- | --- |
| `tasks` | Slug, title, status (`open`, `claimed`, `done`, `blocked`), owner, timestamps, optional `run_id` |
| `steps` | Checklist rows. Position starts at 1 |
| `decisions` | Rationale and who recorded it |
| `task_deps` | Dependency links |
| `events` | Append-only history |
| `reports` | Task reports and cross-task notes. Not events |
| `settings` | Mode, gates, and limits |
| `runs` | One auto-mode run: PRD, branch, stage, cycle, cost estimate, status |

Event kinds are `task_created`, `claimed`, `progress`, `decision`, `status_changed`, `dependency_changed`, `task_renamed`, and `run_linked`. A progress event is unique per task and commit. A null commit, from a directory that is not a git checkout, is not covered, so those reports append.

Report rows are `task` or `note`. A review sets `verdict` to `accepted` or `reopened`. Run status is `active`, `waiting`, `paused`, `stopped`, or `complete`.

## Migrations

Relaybook applies these when `user_version` is behind 6:

| From | What changes |
| --- | --- |
| 1 to 2 | Add `task_deps`. Rebuild `events` with the kind check and the progress uniqueness index. Event rows are copied unchanged |
| 2 to 3 | Add `reports` |
| 3 to 4 | Add `settings` and `runs` |
| 4 to 5 | Add `tasks.run_id` when that column is missing. Add `runs.failure_streak`, `runs.current_slug`, and `runs.complete_requested` when those columns are missing |
| 5 to 6 | Rebuild `events` so `run_linked` is an allowed kind. Event rows are copied unchanged |

A database that has no `events` table yet receives the current schema directly.
