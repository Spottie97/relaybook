# Auto mode

Manual mode is the default. Auto mode spends plan usage: Claude plans and reviews, and Cursor implements, one task at a time, on a branch. Main changes only when you merge the pull request, but the run branch is pushed to `origin` before that. See [Pushes and the pull request](#pushes-and-the-pull-request).

This page describes `workflow=classic`, the default. With `workflow=gated`, the run does not push until a maintainer has authorized HEAD. See [Gated workflow](#gated-workflow).

Sign in to Claude Code and the Cursor CLI once. The CLI command is `cursor-agent`, or `agent` on Windows. Install it there from PowerShell with `irm 'https://cursor.com/install?win32=true' | iex`. Auto mode runs natively on Windows, and a WSL shell can use the Linux release instead. Then:

```sh
relaybook mode auto
relaybook gates plan
relaybook run docs/prd.md
```

`relaybook run --prd docs/prd.md` is the same start. Pass either the positional path or `--prd`, not both.

A run adopts unfinished tasks that are not already in a run, and prints `adopted N existing tasks: ...`. The planner is told to build on them. `relaybook run --new-only docs/prd.md` leaves those tasks alone. A task that depends on an unfinished task outside the run pulls that task in. If the dependency is blocked or belongs to another run, the run stops and names `relaybook run add <slug>`. `/relaybook run add <slug>` passes that command through.

```sh
relaybook run --new-only docs/prd.md
relaybook run add login
```

## Controls

`/relaybook` in Claude Code or Cursor shows the current status and a menu. With arguments it runs the matching command.

`/relaybook mode` prints the current mode. `/relaybook mode manual` and `/relaybook mode auto` set it. `/relaybook gates` prints the checkpoints. `/relaybook gates none`, `/relaybook gates plan`, and `/relaybook gates plan,review` set them. In a terminal, `relaybook mode` and `relaybook gates` print the current value, and a value sets it.

| Action | Slash command | Terminal |
| --- | --- | --- |
| Status and the menu | `/relaybook` | `relaybook status` |
| Mode | `/relaybook mode` or `/relaybook mode auto` | `relaybook mode` or `relaybook mode auto` |
| Checkpoints | `/relaybook gates` or `/relaybook gates plan` | `relaybook gates` or `relaybook gates plan` |
| Start a run | `/relaybook run docs/prd.md` | `relaybook run docs/prd.md` |
| Follow a run | `/relaybook run watch` | `relaybook run watch` |
| Approve a checkpoint | `/relaybook approve` | `relaybook approve` |
| Inbox | `/relaybook inbox` | `relaybook inbox` |

## Checkpoints

```sh
relaybook gates none
relaybook gates plan
relaybook gates plan,review
relaybook approve
```

- `none` runs until the reviewer says the PRD is covered.
- `plan` pauses after planning. Continue with `relaybook approve`.
- `plan,review` also pauses after every review.

The terminal prints each phase as it happens: `● Planning (claude)`, `● Task greet (cursor), cycle 3`, and `● Reviewing greet (claude)`. Tool calls and the first line of the agent's text print under the phase. The closing line gives the duration, the estimated cost, and the outcome (`done`, `reopened`, `accepted`, or `paused at checkpoint`). `relaybook run --quiet docs/prd.md` prints the phase headers and the closing summary only. The same lines are appended to `.relaybook/runs/<id>/live.log`.

```sh
relaybook run watch
relaybook run --quiet docs/prd.md
```

`relaybook run watch` prints the log so far and follows it until the run leaves `active`. A checkpoint prints `checkpoint: waiting for approval`. A usage limit prints `paused:`. A stop prints `halted:`. A finished run prints `finished`. `relaybook status` includes the latest live lines while a run is active, waiting, or paused.

Stop at the next safe boundary with `relaybook mode manual`. A stopped or paused run continues with `relaybook run --resume`. `relaybook run stop` ends it. `relaybook run complete` marks the PRD covered, and the loop pushes the branch and opens the pull request when nothing is still open. `relaybook status` shows the mode, the call count, and whether the run is waiting on approval or a usage limit.

## Limits

The primary limits are cycles (20), agent calls (60), reopens per task (2), and the Cursor timeout (30 minutes). Override them on a run with `--max-cycles`, `--max-agent-calls`, `--max-reopens`, and `--cursor-timeout-min`. `est_cost_cap_usd` is optional. Set the stored defaults with `relaybook config set`. See [configuration.md](configuration.md).

Cursor runs with `--force` on the run branch only. Relaybook checks git before and after every agent call. A push to main, a push of the run branch by an agent, a tag, a branch switch, or a code edit by Claude stops the run. Transcripts are in `.relaybook/runs/` and are gitignored.

A usage limit pauses the run. It does not count as a failure. Resume with `relaybook run --resume` after the limit resets. Two consecutive failures, a blocked task, or a reopen past `max_reopens_per_task` stops the run.

## Pushes and the pull request

When the repository has an `origin` remote, the run pushes its branch with `git push -u origin HEAD:<branch>`:

- after each review that accepts a task,
- whenever the run halts, for example at a limit, on a blocked task, or at a guardrail,
- before it opens the pull request.

When no run task is unfinished, the run pushes and runs `gh pr create --base main` without asking. Checkpoints pause after planning and after reviews only. There is no checkpoint before a push or the pull request. Without an `origin` remote nothing is pushed, but `gh pr create` still runs. Relaybook never merges and never creates a tag.

## Gated workflow

With `workflow=gated`, `relaybook run` starts a run and stores `gated` on that row. Later changes to the setting do not change the row. The run plans with Claude and implements with Cursor. The review step stops with "codex routing not built" and does not call a reviewer. A halt does not push. When no task is unfinished, the run goes to release review and stops the same way, instead of opening a pull request.

`relaybook approve` at `gate_release` checks that `relaybook release authorize` recorded this branch at the current HEAD. It then pushes that exact commit (`git push --no-follow-tags -u origin <sha>:refs/heads/<branch>`) and runs `gh pr create` once. Annotated tags reachable from that commit, including a sign-off tag, stay unpushed. A second approve that overlaps the first does not push or replace the completed run. A ledger has at most one live gated run: status `active`, `waiting`, or `paused`, or stage `publishing` after the push has started. Starting or resuming another gated run while one is live fails with `a gated run is already live` and does not push. Classic runs are outside that limit. Once the push has started, `relaybook run stop`, `relaybook run complete`, and `relaybook run add` leave the row and its `write_stamp` unchanged. `relaybook run --resume` does not release the claim or push or open a pull request. The publishing process still writes the completed run, or stops that run if publish fails. A save from any other process is refused while the stage is `publishing` and does not change `write_stamp`. If that process dies after the push has started, the row stays at `publishing` and keeps the one live gated slot. See [Known limits](#known-limits). Resuming a different run while one is publishing does not push or open a pull request. Without the authorization, `relaybook approve` and `relaybook run --resume` exit 1 with "waiting for sign-off: relaybook release authorize --sha <HEAD>" and push nothing. If HEAD moves after authorization, publish stops with "HEAD moved after authorization" and does not push. If `origin` is missing, publish stops without a pull request. Before the run starts, a `relaybook/<stem>` branch that already exists on a remote is refused. The same local tracking refs are compared around each agent call.

While the run is active, waiting, or paused, or while its stage is `publishing`, `relaybook review start`, `relaybook review record`, `relaybook report accept` for a task that run owns, a release review, and changing the workflow fail with "a gated run owns this; use relaybook approve / run --resume". The task check uses that task's owning run, so a newer run that does not own the task does not allow the write. The ledger write checks that again, so a gated run that starts while a confirmation is still pending still refuses. `relaybook release authorize` is allowed when no gated run is live, or when the run is waiting at `gate_release`. The authorization write checks that again. When that run is waiting on the authorized branch, the write stores its id. A run that resumes into another stage while the challenge is pending is refused, and no authorization row is stored. `relaybook release status` stays available. Stop the run before leaving gated mode. A workflow change made while the run is stopped makes `relaybook run --resume` stop with "workflow changed during run", and that stop does not push. Resuming a classic run, or a run whose stored workflow is unset, after the setting changed to gated stops the same way and does not push. A workflow change while an agent is still running, including a timeout or a guard stop, stops with that same message and does not push.

`relaybook run stop`, `relaybook run complete`, and `relaybook run add` do not change a run whose push has started. `relaybook run watch` still follows the log. Those commands otherwise still run, and they do not push. `relaybook report accept --all` rechecks the workflow and live-run ownership inside the write. It is refused when `workflow=gated`, and when any pending report belongs to a live gated run, before it changes a verdict.

`relaybook config set workflow classic` asks `Leave gated workflow?` at a terminal and requires the typed challenge in [Sign-off](orca-workflow.md#sign-off). The line starts with `classic`. `y` does not confirm. A value that trims to `classic` is the same change. Without a terminal the command fails with `leaving workflow=gated requires confirmation at a TTY` and the setting stays `gated`. The same failure happens if `workflow` becomes `gated` after the command reads it and before the write commits, unless the terminal confirmation already happened. Declining fails with `leaving workflow=gated was not confirmed`. Setting `workflow=gated`, and setting `workflow=classic` when it is already `classic`, does not ask. `relaybook mode` does not ask either.

The manual gated workflow is in [orca-workflow.md](orca-workflow.md). Classic auto mode still pushes as described above. Routing the auto reviewer to Codex is not built. The review step stops until that exists.

## Known limits

If the process that started a gated push dies after the run reaches stage `publishing`, that row stays at `publishing`. It keeps the one live gated slot. Relaybook does not free it. The push result is unknown: the commit may or may not be on a remote. This edit does not discover which, and it does not write a `remote_checks` row.

Do this only after that process is gone. Stop every Relaybook process that might have the ledger open. Back up the ledger before changing it: copy `.relaybook/ledger.db`, and copy `.relaybook/ledger.db-wal` and `.relaybook/ledger.db-shm` when those files exist.

In one `sqlite3` session, replace `RUN_ID` with the stuck run's id. The statement sets `stage` back to `publish`, `status` to `stopped`, and advances `write_stamp` by 1. The schema 8 guard rejects a gated run update that leaves `write_stamp` unchanged. `changes()` is the row count from this connection, so it has to be read here, not in a later `sqlite3` process.

```sh
sqlite3 .relaybook/ledger.db <<'SQL'
UPDATE runs
SET stage = 'publish',
    status = 'stopped',
    write_stamp = write_stamp + 1
WHERE id = RUN_ID
  AND workflow = 'gated'
  AND stage = 'publishing';
SELECT changes();
SELECT id, stage, status, write_stamp FROM runs WHERE id = RUN_ID;
SELECT COUNT(*) FROM runs
WHERE workflow = 'gated'
  AND (
    status IN ('active', 'waiting', 'paused')
    OR stage = 'publishing'
  );
SQL
```

`changes()` prints `1` when the stuck row was updated. The following row shows `stage` `publish`, `status` `stopped`, and a `write_stamp` one higher than before the update. The count of live gated runs prints `0`. If `changes()` prints `0`, the id was wrong or the row was not at `publishing`, and the ledger is unchanged.

The push result is still unknown after that update. Before publishing that commit again, check the commit on every configured remote. For each remote, run `git ls-remote` and look for that commit on any advertised ref, including another branch name, a tag, or a pull ref. Relaybook's online remote check is the same `git ls-remote` of every configured remote, used when `remote_check` is `online`. This manual update does not make the next publish query remotes. With `remote_check=local`, a later publish does not query the network. Do not publish the commit again if it is already on any remote, or if a remote cannot be queried and the commit may already be published.

## PRD template

A useful PRD states the goal, what is in scope, what is out of scope, and the acceptance checks. Clear checks mean fewer reopens and less plan usage.

```md
# Goal
What the change should make true.

# In scope
The commands or files this run may add.

# Out of scope
What the run must leave alone.

# Acceptance checks
The commands or tests that show the goal is met.
```
