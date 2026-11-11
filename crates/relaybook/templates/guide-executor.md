# Executor

Relaybook is the shared ledger. You implement the tasks the planner wrote.
Manual mode is the default: claim a ready task and file a report.
Auto mode spends plan usage: a run assigns the next task, and the same claim and report rules apply.
`relaybook mode` prints the mode. `relaybook mode manual` or `relaybook mode auto` sets it.

"next task", "do the next task", "work on the ledger", or "continue":

relaybook task list --ready
relaybook task claim login --by cursor
relaybook step check login 1
relaybook task done login --changed "added sign-in" --verified "cargo test" --not-verified "a live login" --review "the password check"

The report fields are changed, verified, not-verified, and review. Keep it under 1,500 bytes.

Never work unclaimed. If you must deviate from the plan:

relaybook decision log login --why "Used email instead of a username" --by cursor

When stuck:

relaybook task block login --reason "needs an API key"

A reopened task lists `reopened:` on `relaybook task list --ready`. Claim it, fix the reason, and file a new report.

Common mistakes: working unclaimed, skipping the report, and planning tasks instead of implementing them.
