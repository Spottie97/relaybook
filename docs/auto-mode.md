# Auto mode

Manual mode is the default. Auto mode spends plan usage: Claude plans and reviews, and Cursor implements, one task at a time, on a branch. Main changes only when you merge the pull request.

Sign in to Claude Code and `cursor-agent` once. Then:

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

Stop at the next safe boundary with `relaybook mode manual`. A stopped or paused run continues with `relaybook run --resume`. `relaybook run stop` ends it. `relaybook run complete` marks the PRD covered, and the loop opens the pull request when nothing is still open. `relaybook status` shows the mode, the call count, and whether the run is waiting on approval or a usage limit.

## Limits

The primary limits are cycles (20), agent calls (60), reopens per task (2), and the Cursor timeout (30 minutes). Override them on a run with `--max-cycles`, `--max-agent-calls`, `--max-reopens`, and `--cursor-timeout-min`. `est_cost_cap_usd` is optional. Set the stored defaults with `relaybook config set`. See [configuration.md](configuration.md).

Cursor runs with `--force` on the run branch only. Relaybook checks git before and after every agent call. A push to main, a tag, a branch switch, or a code edit by Claude stops the run. Transcripts are in `.relaybook/runs/` and are gitignored.

A usage limit pauses the run. It does not count as a failure. Resume with `relaybook run --resume` after the limit resets. Two consecutive failures, a blocked task, or a reopen past `max_reopens_per_task` stops the run.

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
