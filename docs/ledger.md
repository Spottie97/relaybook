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

Schema version is `PRAGMA user_version` 8. A new ledger is created at that version. Opening an older file migrates it forward. Opening a file whose `user_version` is newer than 8 fails, on a read-write open and on a read-only open, with `this ledger needs a newer relaybook`. A read-write migration takes the write lock and checks `user_version` again before it changes the file, including when the file has no `events` table. `relaybook init` uses that migration. It does not apply the schema, set `user_version`, or change journal mode first, so a file another process has already created at a newer version is not rewritten to schema 8. When that locked version is newer, the open fails and the stored `user_version` stays as it was.

| Table | Role |
| --- | --- |
| `tasks` | Slug, title, status (`open`, `claimed`, `done`, `blocked`), owner, timestamps, optional `run_id` |
| `steps` | Checklist rows. Position starts at 1 |
| `decisions` | Rationale and who recorded it |
| `task_deps` | Dependency links |
| `events` | Append-only history |
| `reports` | Task reports and cross-task notes. Not events. Nullable `commit_sha`, `base_sha`, and `graph_sha` |
| `settings` | Mode, gates, reviewer, workflow, remote_check, signoff, and limits. `reviewer`, `workflow`, `remote_check`, and `signoff` are rows in this table. `signoff` defaults to `typed` |
| `runs` | One auto-mode run: PRD, branch, stage, cycle, cost estimate, status. Nullable `workflow`, stored when the run starts and left unchanged. `write_stamp` starts at 0 and advances on each save |
| `reviews` | Task and release reviews. `verdict` is null until recorded. Scope is `task` or `release` |
| `graph_builds` | One graph refresh: step, commit, and status `built`, `skipped`, or `failed` |
| `release_authorizations` | Maintainer sign-off for one commit. `run_id` is null in manual mode. A gated run waiting at `gate_release` on that branch supplies `run_id` inside the authorization write. A live gated run that is not waiting at `gate_release` refuses the write. `method` is `tty-typed` or `tty-typed+signed-tag` when `relaybook release authorize` records the row, and null if a caller stores the row without that ceremony while `signoff` is `typed`. The write reads `signoff` again and refuses the row when `signoff` is `signed-tag` unless `method` is `tty-typed+signed-tag` and `evidence` is present. `tty-typed` is a deliberate typed confirmation at a terminal; not proof of a human. `evidence` is null for `tty-typed`. For `tty-typed+signed-tag` it holds the tag name `relaybook-signoff/<sha12>`, the tag object that was verified, and one verifier line |
| `remote_checks` | One gated publish check. `status` is `absent`, `present`, `unknown`, or `local`. `override` is 1 on the authorization check that proceeded without a successful remote query. `review record` and `release authorize` write a row. `release status` reads the latest row |

Event kinds are `task_created`, `claimed`, `progress`, `decision`, `status_changed`, `dependency_changed`, `task_renamed`, and `run_linked`. A progress event is unique per task and commit. A null commit, from a directory that is not a git checkout, is not covered, so those reports append.

Report rows are `task` or `note`. Accepting or reopening a report sets `reports.verdict` to `accepted` or `reopened`. A row in `reviews` uses `approved` or `changes_requested`, and that column is null while the review is pending. Run status is `active`, `waiting`, `paused`, `stopped`, or `complete`.

## Migrations

Relaybook applies these when `user_version` is behind 8:

| From | What changes |
| --- | --- |
| 1 to 2 | Add `task_deps`. Rebuild `events` with the kind check and the progress uniqueness index. Event rows are copied unchanged |
| 2 to 3 | Add `reports` |
| 3 to 4 | Add `settings` and `runs` |
| 4 to 5 | Add `tasks.run_id` when that column is missing. Add `runs.failure_streak`, `runs.current_slug`, and `runs.complete_requested` when those columns are missing |
| 5 to 6 | Rebuild `events` so `run_linked` is an allowed kind. Event rows are copied unchanged |
| 6 to 7 | Add `reports.commit_sha`, `reports.base_sha`, and `reports.graph_sha` when those columns are missing. Add `reviews`, `graph_builds`, and `release_authorizations` |
| 7 to 8 | Add `runs.workflow` and `runs.write_stamp` (default 0) when those columns are missing. Add `release_authorizations.method` and `release_authorizations.evidence` when those columns are missing. Add `remote_checks`. Install the five gated write guards |

A database that has no `events` table yet receives the current schema directly when the locked `user_version` is not newer than 8. Gated work requires Relaybook 0.4.0 or later (ledger schema 8). While `workflow=gated`, five triggers refuse the writes a 0.3.3 binary uses for a task report with no `commit_sha`, accepting a task report, a claim event with no `base_sha` key, a run insert whose `workflow` is not `gated`, and a run update that does not advance `write_stamp`. The statement fails with `workflow=gated: this ledger needs relaybook 0.4.0 or later (schema 8)`, and the transaction rolls back. With `workflow=classic`, the task-report, accept, claim, and run-insert triggers do not fire, so those 0.3.3 writes keep working. A run row whose stored `workflow` is `gated` still rejects an update that leaves `write_stamp` unchanged, including after the setting is switched to `classic`. A JSON `base_sha` of null is a present key and is allowed. Returning to classic is `relaybook config set workflow classic` at a terminal; there is no down-migration. The triggers are an accidental-old-binary guard, not a security boundary: `sqlite3` can drop a trigger, change `workflow`, or insert a review. A read-write open recreates a missing trigger. On a gated ledger that restore adds `ledger_guard` `restored` to that command's `--json` data, including a failed command. The field is omitted when the triggers were already present, on human output, and on a classic ledger. Read-only opens do not restore triggers. See [Minimum version](orca-workflow.md#minimum-version).
