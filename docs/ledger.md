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

Schema version is `PRAGMA user_version` 7. A new ledger is created at that version. Opening an older file migrates it forward.

| Table | Role |
| --- | --- |
| `tasks` | Slug, title, status (`open`, `claimed`, `done`, `blocked`), owner, timestamps, optional `run_id` |
| `steps` | Checklist rows. Position starts at 1 |
| `decisions` | Rationale and who recorded it |
| `task_deps` | Dependency links |
| `events` | Append-only history |
| `reports` | Task reports and cross-task notes. Not events. Nullable `commit_sha`, `base_sha`, and `graph_sha` |
| `settings` | Mode, gates, reviewer, workflow, and limits. `reviewer` and `workflow` are rows in this table |
| `runs` | One auto-mode run: PRD, branch, stage, cycle, cost estimate, status |
| `reviews` | Task and release reviews. `verdict` is null until recorded. Scope is `task` or `release` |
| `graph_builds` | One graph refresh: step, commit, and status `built`, `skipped`, or `failed` |
| `release_authorizations` | Maintainer sign-off for one commit. `run_id` is null in manual mode |

Event kinds are `task_created`, `claimed`, `progress`, `decision`, `status_changed`, `dependency_changed`, `task_renamed`, and `run_linked`. A progress event is unique per task and commit. A null commit, from a directory that is not a git checkout, is not covered, so those reports append.

Report rows are `task` or `note`. Accepting or reopening a report sets `reports.verdict` to `accepted` or `reopened`. A row in `reviews` uses `approved` or `changes_requested`, and that column is null while the review is pending. Run status is `active`, `waiting`, `paused`, `stopped`, or `complete`.

## Migrations

Relaybook applies these when `user_version` is behind 7:

| From | What changes |
| --- | --- |
| 1 to 2 | Add `task_deps`. Rebuild `events` with the kind check and the progress uniqueness index. Event rows are copied unchanged |
| 2 to 3 | Add `reports` |
| 3 to 4 | Add `settings` and `runs` |
| 4 to 5 | Add `tasks.run_id` when that column is missing. Add `runs.failure_streak`, `runs.current_slug`, and `runs.complete_requested` when those columns are missing |
| 5 to 6 | Rebuild `events` so `run_linked` is an allowed kind. Event rows are copied unchanged |
| 6 to 7 | Add `reports.commit_sha`, `reports.base_sha`, and `reports.graph_sha` when those columns are missing. Add `reviews`, `graph_builds`, and `release_authorizations` |

A database that has no `events` table yet receives the current schema directly. Gated work requires Relaybook 0.4.0 or later (ledger schema 7). 0.3.3 and earlier open a schema 7 ledger but ignore the gates. See [Minimum version](orca-workflow.md#minimum-version).
