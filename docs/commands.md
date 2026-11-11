# Commands

Plain text is the default. `relaybook status --json` prints one JSON envelope on stdout and nothing else. Exit `0` on success, `1` on an operational failure, and `2` on a usage error or a missing ledger. Human-readable errors go to stderr.

```json
{"ok": true, "data": {}, "error": null}
{"ok": false, "data": null, "error": "already claimed"}
```

## Daily workflow

```sh
relaybook task list --ready
relaybook task claim greet --by cursor
relaybook progress report greet --note "listed the files"
relaybook task done greet --changed "said hello" --verified "cargo test" --not-verified "a clean macOS install" --review "inbox order"
```

## Tasks

Slugs match `[a-z0-9]+(-[a-z0-9]+)*` and stay permanent. `relaybook task rename` changes the title only.

```sh
relaybook init
relaybook task create greet --title "Say hello"
relaybook task create wave --title "Wave" --depends-on greet
relaybook task list
relaybook task list --status open
relaybook task list --ready
relaybook task list --all
relaybook task depend wave --on greet
relaybook task undepend wave --on greet
relaybook task claim greet --by cursor
relaybook task block greet --reason "stuck on a decision"
relaybook task unblock greet
relaybook task rename greet --title "Say hello clearly"
relaybook task done greet --no-report
relaybook task reopen greet --reason "needs a test"
```

`relaybook task claim` takes a write lock and updates a task only while it is open. A missing slug fails with `no such task: greet`. Any other status, including a claim that lost the race, fails with `already claimed` and exit code 1. A task whose dependencies are not done fails with `waiting on: greet` and stays open.

`blocked` means someone marked the work stuck. `relaybook task block` on a task that is already blocked succeeds and writes no new event. A done task cannot be blocked. `relaybook task unblock` returns a blocked task to open, or to claimed when it still has an owner.

`relaybook task list --ready` shows open tasks whose dependencies are all done. Human output appends `needs: greet` for unfinished dependencies. JSON tasks include a `depends_on` array of every link. Adding a link that already exists, or removing one that does not, succeeds and writes no event. A self-link or a cycle is rejected. A dependency clears itself when the prerequisite is done.

`relaybook progress report` stores `git rev-parse HEAD` when that succeeds. A second report for the same task and commit fails with `already reported for <sha>`. Outside a git checkout the commit is null, and each report is appended.

`relaybook task done` requires a claim. On an open task it fails with `claim it first: relaybook task claim greet --by <agent>` and exit code 1, and it writes nothing. A task that is already done succeeds and does not write another event. `relaybook task done greet --no-report` still stores an empty report and does not store a second one if the task is already done.

## Checklist, decisions, and context

Checklist positions start at 1. Prefer `--text` on `step add`, including when the text starts with a dash. The positional form still works. Pass one of the two. Titles (`--title`), decision text (`--why`), and notes (`--text`) may also start with a dash.

```sh
relaybook step add greet --text "write the command"
relaybook step add greet "write the command"
relaybook step check greet 1
relaybook step edit greet 1 --text "write the command again"
relaybook decision log greet --why "keep the slug" --by cursor
relaybook context
relaybook context greet
relaybook context --all
relaybook context --events 20
```

`relaybook context` shows active tasks only. Checked steps collapse to `n/m done`, and older decisions stay on `relaybook context greet`. Finished tasks stay behind `relaybook context --all`. Events stay behind `relaybook context --events 20`. Each cut names the command that restores it.

On a 40-done fixture, context fell from 4567 bytes (about 1141 tokens) to 1482 bytes (about 370 tokens). A planning burst of one create, five steps, and a decision fell from 232 bytes to 152. Write commands answer with an id, slug, and status. Read commands still return the full record.

## Reports and the hand-back loop

```sh
relaybook task done greet --changed "said hello" --verified "cargo test" --not-verified "a clean macOS install" --review "inbox order"
relaybook report note --by cursor --text "checked the inbox"
relaybook hook prompt
relaybook inbox
relaybook report accept greet
relaybook report accept --note 1
relaybook report accept --all
relaybook task reopen greet --reason "needs a test"
```

The executor claims the task, then finishes with short bullets under 1,500 bytes. A cross-task note uses `relaybook report note`. On the next prompt, `UserPromptSubmit` runs `relaybook hook prompt`. When reports are waiting it prints one line, `N new Cursor reports: relaybook inbox`. Otherwise it prints nothing and exits 0.

The planner runs `relaybook inbox`, then `relaybook report accept greet` or `relaybook task reopen greet --reason "needs a test"`. A reopened task returns to `relaybook task list --ready` with `reopened: needs a test` until it is done again. A larger problem becomes a new task.

## Auto mode

```sh
relaybook run docs/prd.md
relaybook run --new-only docs/prd.md
relaybook run add login
relaybook run stop
```

`relaybook run` adopts open and claimed tasks that have no run. `--new-only` skips that. `relaybook run add` links unfinished tasks to the active run. A done task is rejected.

## Install and setup

```sh
relaybook install --claude --cursor
relaybook setup
```

`relaybook install` appends the planner block to `CLAUDE.md` and the executor block to `AGENTS.md`. A second run does not change a file that already has the Relaybook markers or the coordination heading. Text outside the block is kept. Setup flags are in [configuration.md](configuration.md).
