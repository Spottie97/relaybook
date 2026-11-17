# Orca workflow

Manual work with `workflow=gated` is implemented. The default stays `workflow=classic`: the same task commands, and no review, release, or graph gate. Auto mode still plans and reviews with Claude, and `relaybook run` still pushes before sign-off. Sections marked **Planned** are not enforced.

## Minimum version

Gated work requires Relaybook 0.4.0 or later (ledger schema 7). 0.3.3 and earlier open a schema 7 ledger but ignore the gates, so Orca checks `relaybook --version` >= 0.4.0 before gated work. Triggers stay deferred.

## Roles

- Orca drives Relaybook one command at a time with `--json` and reads each JSON result. Orca does not use `relaybook run`.
- Claude plans. Cursor implements and fixes reopened tasks. Codex reviews. Codex does not edit code or write to the ledger. Orca records the verdict.
- Orca records its own ledger entries with `--by orca` on `relaybook decision log` and `relaybook report note`. Reviews are `--by codex`. Claims stay `--by cursor`, because the report author is the claimer.
- A review recorded `--by claude` needs the maintainer at a terminal. It is stored with `reviewer_override` 1. Any other name fails with `unconfigured reviewer`.
- The maintainer signs off. Orca cannot. `relaybook release authorize` refuses when stdin is not a terminal.

## Settings

Defaults are `reviewer=claude` and `workflow=classic`. `workflow=gated` requires `reviewer=codex`. Set the reviewer first.

```sh
relaybook config
relaybook config set reviewer codex
relaybook config set workflow gated
```

`relaybook review`, `relaybook release`, and `relaybook graph refresh` fail until `workflow=gated` is set.

## Steps

1. Orca runs `relaybook graph refresh --step plan`. The JSON has `commit_sha`, `graph_sha`, and `status` (`built`, `skipped`, or `failed`). Claude then uses `relaybook task create`, `relaybook step add`, and `relaybook decision log`.
2. Orca runs `relaybook task claim greet --by cursor`. The tree must be clean. JSON adds `base_sha`, `graph_sha`, and `graph_status`. A claimed task counts as dispatched.
3. Cursor implements, commits, and runs `relaybook task done greet --changed "said hello" --verified "cargo test" --not-verified "a live login" --review "the diff"`. The tree must be clean. JSON `commit_sha` is `HEAD`. The report keeps the claim's `base_sha` and `graph_sha`. Relaybook then refreshes the graph for step `commit`.
4. Orca runs `relaybook review start greet`. The JSON is the handoff: `slug`, `base_sha`, `commit_sha`, `range`, `graph_sha`, `graph_status`, the report, and `graph_context`. Orca passes that JSON and `relaybook guide reviewer` to Codex. Codex does not open the ledger.
5. Orca runs `relaybook review record greet --by codex --sha abcdef --verdict approved --text '{"findings":[],"security":[],"tests_checked":[],"not_checked":[]}'`. `--sha` must equal `HEAD`. The body is at most 1,500 bytes.
6. `relaybook review record greet --by codex --sha abcdef --verdict changes-requested --text '{"findings":[],"security":[],"tests_checked":[],"not_checked":[]}'` reopens the task. Cursor claims it again.
7. Orca accepts with `relaybook report accept greet` only when the latest task review is `approved` at `commit_sha` equal to `HEAD`. Otherwise the command fails with `no approved review at HEAD`. `relaybook report accept --all` is unavailable in gated mode. `relaybook context greet` includes `latest_review`.
8. When every in-scope task is accepted and none is claimed, Orca runs `relaybook review start --release`. The range is `merge-base(main, HEAD)..HEAD`. The handoff lists those tasks. A release body also has `prd_covered` and `gaps`: `relaybook review record --release --by codex --sha abcdef --verdict approved --text '{"findings":[],"security":[],"tests_checked":[],"not_checked":[],"prd_covered":[],"gaps":[]}'`. Orca decides completion from that verdict. The maintainer confirms.
9. The maintainer runs `relaybook release authorize --sha abcdef` at a terminal. The SHA must equal `HEAD`, and an approved release review must exist at that SHA. The prompt is `Authorize this release?`.
10. Orca publishes only after `relaybook release status` shows an authorization at the current `HEAD`. Orca pushes and opens the pull request. Relaybook's manual commands never run `git push` or `gh`.

`relaybook task done` stores `commit_sha` in classic mode as well. Classic mode does not require a clean tree, a review, or a graph refresh.

## Nothing is published before sign-off

`relaybook review record` and `relaybook release authorize` refuse when `refs/remotes/origin/<branch>` exists. That check misses other remote names and other clones. Manual commands still never push. A person or an agent can run `git push` outside Relaybook.

**Auto mode today:**

- `relaybook run` pushes the run branch to `origin` after every accepted review and whenever it halts, then pushes again and runs `gh pr create` without a checkpoint. See [auto-mode.md](auto-mode.md#pushes-and-the-pull-request). That is why this workflow does not use `relaybook run`.
- Setting `workflow=gated` does not change that loop.

**Planned:**

- Codex routing inside `relaybook run`, including a missing-Codex halt.
- A runloop guard that stops a reviewer who edits the branch or the ledger.
- A release gate that skips pushes and the pull request until sign-off, including when the run halts.
- Graph refresh and handoff SHAs inside the auto runner.

## Reviewer edits

`relaybook review start` stores `HEAD`, a worktree fingerprint, and a ledger mark. `relaybook review record` fails with `reviewer changed the repository` or `ledger changed during review` if those change. Orca must write nothing between the two commands. The check detects an edit. It does not sandbox the reviewer.

## Graphify

In gated mode Relaybook runs `graphify update .` at each step boundary above, and, when that refresh is `built`, `graphify affected` for each file in the reviewed range. It never runs `graphify extract`. Agents may use `graphify query`, `graphify explain`, `graphify path`, `graphify affected`, and `graphify god-nodes`.

`graph_sha` is `HEAD` after a successful update on a clean tree. `.relaybook/` does not count as dirty. A missing `graphify` binary, a failed update, or no `graphify-out/graph.json` afterwards records `skipped` or `failed`, leaves `graph_sha` null, and puts `no graph` in the handoff. The command still succeeds.

`graph_context` is at most 4,000 bytes. A longer result ends with `[truncated]`.

`graphify-out/` must be gitignored. `relaybook setup --ci` tracks `graphify-out/graph.json`, so a repository set up that way cannot use `workflow=gated`. The refresh then records `failed` and `no graph: graphify-out/ is tracked`.

Between steps, Graphify's post-commit and post-checkout hooks and Relaybook's post-merge and post-rewrite hooks may refresh the graph. `relaybook setup --no-hooks` skips the hooks setup installs. Setup also skips Relaybook's hooks when `core.hooksPath` is set, or when Husky or Lefthook is present. The step-boundary refresh still runs.

The auto runner does not call Graphify. That refresh is planned, above. Classic mode does not refresh at claim, done, or review.

`relaybook setup` and `relaybook uninstall --graphify` still run Graphify's install and uninstall commands. The opt-in workflow from `relaybook setup --ci` runs `graphify extract` in GitHub Actions when an LLM API secret is set, and `graphify update` otherwise. See [configuration.md](configuration.md#how-the-graph-stays-current).
