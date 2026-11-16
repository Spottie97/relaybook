# Orca workflow (planned)

> Planned. Relaybook 0.3.3 does not implement this workflow. Lines marked **Today** describe what the binary does now. Everything else is a plan, and nothing in it is enforced yet.

## Roles

- Orca drives Relaybook one command at a time with the global `--json` flag and reads each JSON result. Orca does not use `relaybook run` at first.
- Claude plans. Cursor implements and fixes reopened tasks. Codex reviews; it does not edit code or write to the ledger, and Orca records its verdict.
- Orca records its own ledger entries with `--by orca`. Codex reviews are recorded as `--by codex`. A Claude review needs the maintainer's approval at the time.
- The maintainer signs off on the release.

## Steps

1. Claude plans tasks in the ledger.
2. Orca hands one task to Cursor. A claimed task counts as dispatched.
3. Cursor implements, commits, and reports with `relaybook task done`.
4. Codex reviews that exact commit. Orca records the verdict. Changes requested reopen the task for Cursor.
5. Orca accepts a task only after Codex approves the commit that is checked out.
6. When every task is accepted, Codex reviews the whole branch at its final commit, including whether the PRD is covered. Orca decides the run is complete from that verdict, and the maintainer confirms.
7. The maintainer signs off on that exact commit. Nothing is published before this step.

## Nothing is published before sign-off

Planned rule: nothing is pushed before the maintainer signs off. That covers every branch push and every pull request.

**Today:**

- Manual-mode commands never run `git push` or `gh`. Only the auto runner does.
- `relaybook run` pushes the run branch to `origin` after every accepted review and whenever it halts, then pushes again and runs `gh pr create` without a checkpoint. See [auto-mode.md](auto-mode.md#pushes-and-the-pull-request). That is why this workflow does not use `relaybook run`.
- Relaybook does not stop an agent, Orca, or a person from running `git push` in manual mode. Until sign-off is built, the rule depends on Orca and the agents' instructions.

## Graphify for every agent

Planned: Claude, Cursor, and Codex all use the Graphify graph for context and code lookup, with `graphify query`, `graphify explain`, `graphify path`, `graphify affected`, and `graphify god-nodes`. Relaybook runs `graphify update .` before every agent step and after every commit it records, and Graphify's git hooks keep the graph current in between. Every handoff records the commit the graph was built from, so no agent works from a stale graph without knowing it. Graphify output that Relaybook adds to an agent's prompt has a byte limit. `graphify extract` never runs automatically.

**Today:**

- Relaybook runs Graphify only from `relaybook setup`, `relaybook uninstall --graphify`, and the post-merge and post-rewrite hooks that setup installs. The auto runner never calls Graphify.
- `graphify hook install`, which setup runs, adds Graphify's own post-commit, post-checkout, post-merge, and post-rewrite hooks. `relaybook setup --no-hooks` skips them.
- Relaybook does not record which commit the graph was built from, and does not check whether the graph exists or is current.
- In auto mode Claude may run `graphify` commands. Cursor reads `.cursor/rules/graphify.mdc`, which Graphify writes. Relaybook gives Codex no Graphify instructions.
- Relaybook never runs `graphify extract`. The opt-in workflow from `relaybook setup --ci` runs it in GitHub Actions when an LLM API secret is set. See [configuration.md](configuration.md#how-the-graph-stays-current).
