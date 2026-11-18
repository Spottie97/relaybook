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

With `workflow=gated`, `relaybook run` starts a run and stores `gated` on that row. Later changes to the setting do not change the row. The run plans with Claude and implements with Cursor. Review and release review call Codex. A halt does not push. An approved release review waits at `gate_release`. It does not push or open a pull request. `relaybook approve` publishes only after authorization, as below.

`relaybook approve` at `gate_release` checks that `relaybook release authorize` recorded this branch at the current HEAD. It then pushes that exact commit (`git push --no-follow-tags -u origin <sha>:refs/heads/<branch>`) and runs `gh pr create` once. Annotated tags reachable from that commit, including a sign-off tag, stay unpushed. A second approve that overlaps the first does not push or replace the completed run. A ledger has at most one live gated run: status `active`, `waiting`, or `paused`, or stage `publishing` after the push has started. Starting or resuming another gated run while one is live fails with `a gated run is already live` and does not push. Classic runs are outside that limit. Once the push has started, `relaybook run stop`, `relaybook run complete`, and `relaybook run add` leave the row and its `write_stamp` unchanged. `relaybook run --resume` does not release the claim or push or open a pull request. The publishing process still writes the completed run, or stops that run if publish fails. A save from any other process is refused while the stage is `publishing` and does not change `write_stamp`. If that process dies after the push has started, the row stays at `publishing` and keeps the one live gated slot. See [Known limits](#known-limits). Resuming a different run while one is publishing does not push or open a pull request. Without the authorization, `relaybook approve` and `relaybook run --resume` exit 1 with "waiting for sign-off: relaybook release authorize --sha <HEAD>" and push nothing. If HEAD moves after authorization, publish stops with "HEAD moved after authorization" and does not push. If `origin` is missing, publish stops without a pull request. Before the run starts, a `relaybook/<stem>` branch that already exists on a remote is refused. The same local tracking refs are compared around each agent call.

While the run is active, waiting, or paused, or while its stage is `publishing`, `relaybook review start`, `relaybook review record`, `relaybook report accept` for a task that run owns, a release review, and changing the workflow fail with "a gated run owns this; use relaybook approve / run --resume". The task check uses that task's owning run, so a newer run that does not own the task does not allow the write. The ledger write checks that again, so a gated run that starts while a confirmation is still pending still refuses. `relaybook release authorize` is allowed when no gated run is live, or when the run is waiting at `gate_release`. The authorization write checks that again. When that run is waiting on the authorized branch, the write stores its id. A run that resumes into another stage while the challenge is pending is refused, and no authorization row is stored. `relaybook release status` stays available. Stop the run before leaving gated mode. A workflow change made while the run is stopped makes `relaybook run --resume` stop with "workflow changed during run", and that stop does not push. Resuming a classic run, or a run whose stored workflow is unset, after the setting changed to gated stops the same way and does not push. A workflow change while an agent is still running, including a timeout or a guard stop, stops with that same message and does not push.

`relaybook run stop`, `relaybook run complete`, and `relaybook run add` do not change a run whose push has started. `relaybook run watch` still follows the log. Those commands otherwise still run, and they do not push. `relaybook report accept --all` rechecks the workflow and live-run ownership inside the write. It is refused when `workflow=gated`, and when any pending report belongs to a live gated run, before it changes a verdict.

`relaybook config set workflow classic` asks `Leave gated workflow?` at a terminal and requires the typed challenge in [Sign-off](orca-workflow.md#sign-off). The line starts with `classic`. `y` does not confirm. A value that trims to `classic` is the same change. Without a terminal the command fails with `leaving workflow=gated requires confirmation at a TTY` and the setting stays `gated`. The same failure happens if `workflow` becomes `gated` after the command reads it and before the write commits, unless the terminal confirmation already happened. Declining fails with `leaving workflow=gated was not confirmed`. Setting `workflow=gated`, and setting `workflow=classic` when it is already `classic`, does not ask. `relaybook mode` does not ask either.

The manual gated workflow is in [orca-workflow.md](orca-workflow.md). Classic auto mode still pushes as described above. A classic run does not call Codex.

### Codex review

Preflight checks `claude`, `cursor-agent` (or `agent`), and `codex`. `codex login status` must exit 0. A missing `codex` stops with `codex: not on PATH` before any git command. Any other status stops with `codex: not signed in; run codex login`.

The default sandbox is `read-only`. The argv is `codex exec -s read-only --ephemeral --color never -C <root> -m <model> --output-schema <schema> -o <file>`, and the packet is stdin. `<model>` is the `codex_model` setting, default `gpt-6.1-sol`. `codex login status` does not take `-m`. `<file>` is `codex-last.md` in a new directory outside the repository. That directory is created at mode 0700, and creation fails if the path already exists, including as a symlink. The schema file is mode 0600. Relaybook opens the verdict file without following a symlink. It ignores the file unless it is a regular file owned by the Relaybook user or, in isolated mode, by the account named in `RELAYBOOK_CODEX_USER`. A world-accessible directory is ignored. `relaybook config set codex_sandbox isolated` omits `-s read-only`. Use that only when the operator already runs Codex as its own OS user on a read-only mount. Set `RELAYBOOK_CODEX_USER` to that account when it is not the Relaybook user. Relaybook then grants only that account access to the directory and schema. If the account is missing or the grant fails, Codex is not started. Relaybook does not pass `--add-dir`, `--dangerously-*`, `--approve-for-me`, `--trust`, `--worktree`, or `codex apply`, and it does not check that the mount exists or that the account is unprivileged.

The packet is the reviewer guide, the handoff JSON, and `git diff` for the reviewed range. Task review and release review refresh the graph before the handoff, the same way `relaybook review start` does. `graph_sha` is `HEAD` when that refresh is `built`. A skipped or failed refresh leaves `graph_sha` null and `graph_context` as `no graph`, and the review still runs. A packet over 64 KiB, or a prompt plus packet over 1,048,576 characters, stops with `diff too large for auto review; review manually` and does not call Codex.

Before Claude plans, the run refreshes step `plan` and appends `Graph: <graph_sha or no graph> at <commit>` to the planner prompt. `<commit>` is `HEAD` at that refresh. Before Cursor implements a task, the run refreshes step `implement` for that slug and appends `Base: <HEAD>` and the same Graph line. Those rows are stored on `graph_builds` for that run. A skipped or failed refresh does not stop the run. The runner calls `graphify update .` and does not call `graphify extract`. A classic run does not refresh the graph, and its prompts stay the same.

Codex must exit 0 and leave one JSON object in the `-o` file: `{"verdict":"approved"|"changes-requested","body":{...}}`. The object has those two keys and no other outer property. The body has exactly the review keys and no others. Each value is an array of strings. The body is at most 1,500 bytes. Relaybook records it with reviewer `codex` on the review row started for that invocation. That start write checks the report, run generation, and stage read before the graph refresh, stores that report id and a unique invocation token, and claims the run generation in the same write. A task report replaced during the refresh is `report changed during graph refresh` and is not accepted. The verdict is recorded only when that same review row is still pending, its invocation token matches, and the run's `write_stamp` is still the generation claimed at launch. Otherwise the result is `stale review invocation`. It is not approved and the report is not accepted. A reused review id cannot satisfy that check. A stale non-zero exit, a stale malformed file, or a stale halt leaves the replacement run unchanged and does not continue a run that is waiting at a checkpoint. An approved task review records the verdict, accepts only the report captured when that review started, and advances the run in the same ledger write. A failure rolls all three back. Resume that finds the report already accepted for an approved review at HEAD continues from that result. `changes-requested` reopens the task and Cursor runs again. An approved release review moves the run to `gate_release` with status `waiting`. A release review of `changes-requested` stops with `release review requested changes` and does not push. A non-zero exit, or a missing, empty, unparsable, or schema-invalid `-o` file, is a failure. It is never an approval. Two consecutive failures stop the run. If the ledger mark changed during that invocation, the run stops immediately with `ledger changed during review` and does not count the failure or retry. A vanished `codex` binary stops immediately and does not retry. A classified Codex usage limit is not one of those ordinary failures. See [Reviewer fallback](#reviewer-fallback).

### Reviewer fallback

`review_fallback` defaults to `off`. A Codex usage limit is recognized only when every signal is present: exit code 1, an empty or missing `-o` file, stderr containing one of the two phrases captured from `codex-cli 0.169.0` (`You’ve hit your usage limit`, whose apostrophe is U+2019, or `Quota exceeded. Check your plan and billing details.`), stderr containing none of `401`, `input_too_large`, or `Not logged in`, and `codex --version` printing exactly `codex-cli 0.169.0`. The phrase must be on stderr. The same text on stdout or in the `-o` file is an ordinary failure. `exceeded retry limit, last status: 429` is an ordinary failure. Any other version is an ordinary failure and does not fall back.

Those captured lines print a zone-less local clock (`try again at 6:39 AM`, `try again later`). Relaybook does not parse that text. The retry time is always `now` plus `codex_backoff_min` (default 60, range 5 to 1440), stored as a UTC timestamp. An advisory ISO timestamp, if one is ever supplied, is kept only when SQLite says it is inside the next 7 days. A time in the past or further out uses the backoff.

With `review_fallback=off`, the run pauses (`codex|<retry_after>`), `failure_streak` stays 0, and no other reviewer is called. `relaybook run --resume` before that time fails with `codex limited until <t>` and does not call Codex. With `review_fallback=sonnet`, the same limit leaves the run active and the next review is Claude model `claude-sonnet-5-5`. That model id is a code constant, not a setting. The live line is `● Reviewing <slug> (claude-sonnet-5-5 — fallback: codex usage limit until <t>)`. The review row stores `reviewer=claude`, `reviewer_model`, and `fallback_reason`. One `reviewer_events` row records `codex_limited`, and the first Sonnet review in the run records `switched_to_fallback`.

The Sonnet argv is `claude -p <prompt> --model claude-sonnet-5-5 --permission-mode dontAsk --tools Read,Grep,Glob --allowedTools Read,Grep,Glob --disallowedTools Edit,Write,NotebookEdit,Bash,WebFetch,WebSearch --no-session-persistence --output-format json --strict-mcp-config --restricted`. The packet is stdin. The working directory is the repository root. Relaybook refuses to spawn that process if `--tools` or `--allowedTools` names anything other than `Read`, `Grep`, or `Glob`, if `--permission-mode` is not `dontAsk`, or if the argv contains `--dangerously-*`, `--add-dir`, `--resume`, `--continue`, `--fallback-model`, or `--mcp-config`. `--strict-mcp-config` is not `--mcp-config`. The verdict is the JSON envelope's `result` text, parsed with the same review schema as Codex. If the envelope names a model, it must be `claude-sonnet-5-5`. An error, a limit, or any other model is a failure and is never an approval. Sonnet and the Codex probe use the same invocation token, report snapshot, generation, and stage check as a Codex review, and the same worktree guard.

When the backoff has elapsed, Relaybook runs one probe before the next review: `codex exec` with the configured sandbox and `codex_model`, an empty stdin, and a strict schema `{"ok":true}`. The prompt starts with `Probe.`. A probe does not refresh the graph. Probe ok records `probe_ok` and `switched_to_codex` and the next review is Codex. Probe still limited records `probe_limited` and a new backoff. An ordinary probe failure stops with `codex probe failed (not a usage limit)` and does not fall back. At most one probe runs per window. Probes count toward `max_agent_calls`.

`fallback_rereview` defaults to `before-release`. Each approved Sonnet review stays pending until an approved Codex row points at it with `rereview_of`. Those re-reviews run, oldest first, when Codex is available, before the next review. The packet is the original diff plus that review, with graph context `no graph`. `commit_sha` on the new row is current HEAD. A re-review does not accept or reopen a task. `changes-requested` stops with `codex re-review requested changes on <slug>`. While any re-review is pending, the release review is Codex, not Sonnet. If Codex is still limited at `release_review`, the run pauses with `waiting for Codex re-review (<n> pending, retry after <t>)`. `relaybook release authorize` and `relaybook approve` then fail with `codex re-review pending`. A database trigger on `release_authorizations` raises the same error when `fallback_rereview` is `before-release` or unset. With `fallback_rereview=off`, Sonnet may do the release review, nothing blocks authorization, and pending items are still listed in `relaybook status` and the pull-request body.

Making `review_fallback` `sonnet`, or `fallback_rereview` `off`, is a loosening change. It is refused while a gated run is live (`a gated run is already live`) and otherwise requires the typed challenge at a terminal. The verbs are `sonnet` and `off`. Making a setting stricter does not ask. `codex_backoff_min` does not ask. `user_version` stays 8. A read-write open adds the new columns, `reviewer_events`, and the trigger when they are missing.

Just before Codex runs, Relaybook snapshots HEAD, the branch name, the worktree fingerprint, every ref (`git for-each-ref --format=%(objectname) %(refname)`), the bytes of the git config file (`git rev-parse --git-path config`), and a hash of the hooks directory (`git rev-parse --git-path hooks`). After Codex exits, a difference stops the run before a verdict is recorded. Nothing is pushed. The stop reason is `reviewer edited the worktree`, `reviewer moved HEAD`, `reviewer changed refs: <ref>`, `reviewer changed git config`, `reviewer changed hooks`, `created a tag`, or `pushed outside relaybook`. A commit moves HEAD and the branch ref; that stop is `reviewer moved HEAD`. A new tag stops with `created a tag`. A remote-tracking ref stops with `pushed outside relaybook`. Any other ref names the first changed ref. The hooks hash covers the directory from `git rev-parse --git-path hooks`: each entry's name and permission bits, the bytes of a regular file, and, for a symlink, the target path plus the followed file. Relaybook follows a symlink for a bounded number of hops, includes that file's permission bits and bytes, and stops if it finds a cycle, a directory target, a target that is too large, or a target it cannot read. It does not walk a symlinked directory. The ledger mark stored when the review started is the maximum id of events, reports, decisions, and steps, plus the latest task update time. It is not every database write. Relaybook checks that mark for every Codex outcome, including a non-zero exit and an unparsable verdict, inside the write that checks the invocation. A change stops the run with `ledger changed during review`, does not record a verdict, and does not retry.

### Security notes

V2 verification: PASS. In read-only mode the packet is the whole review: this container's `codex exec -s read-only` cannot run shell commands, so do not assume Codex read the worktree. Isolated mode is the operator's setup. Relaybook cannot verify it. The snapshot compares the worktree fingerprint, HEAD, the branch name, local refs from `git for-each-ref` (including local remote-tracking refs), the git config file, and the hooks hash. A local remote-tracking ref can show that a push updated this clone. The snapshot does not see a push that left no local ref change. The ledger check is the mark described above: the maximum id of events, reports, decisions, and steps, plus the latest task update time. It is not every database write. The snapshot and the mark detect those changes. They do not prevent them. A Sonnet fallback review is not a second vendor: it is the same Claude account, with Read, Grep, and Glob. It does not have Edit, Write, or shell. The maintainer keeps push credentials off the agent machine and protects `main` on GitHub with branch protection or rulesets. Relaybook leaves that login and those settings unchanged. See [Sign-off](orca-workflow.md#sign-off).

## Known limits

A real Codex usage-limit response from the production account (evidence E3) has not been captured. The classifier accepts only the two stderr phrases run against `codex-cli 0.169.0` in the local stub. It does not accept workspace-credit or spend-cap wording, and it does not parse the zone-less `try again at 6:39 AM` text. `claude --help` on the installed CLI (2.1.325) lists `--strict-mcp-config` and `--restricted`. `--restricted` is the settings restriction that was verified: `--setting-sources` lists `user`, `project`, and `local`, and no empty value was verified, so Relaybook does not pass `--setting-sources`. `--bare` is not used, because that help text says Anthropic auth is then only `ANTHROPIC_API_KEY` or `apiKeyHelper`.

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
