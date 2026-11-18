# Orca workflow

Manual work with `workflow=gated` is implemented. The default stays `workflow=classic`: the same task commands, and no review, release, or graph gate. Auto mode with `workflow=classic` still plans and reviews with Claude, and `relaybook run` still pushes before sign-off. With `workflow=gated`, auto mode plans with Claude, implements with Cursor, and reviews with Codex. It does not push before sign-off. `relaybook approve` publishes only after `relaybook release authorize` for that HEAD.

## Minimum version

Gated work requires Relaybook 0.4.0 or later (ledger schema 8). Orca checks `relaybook --version` >= 0.4.0 before gated work. A ledger newer than this binary is refused, including a read-only open, with `this ledger needs a newer relaybook`. While `workflow=gated`, schema 8 triggers refuse the 0.3.3 task-report, accept, claim, and run writes with `workflow=gated: this ledger needs relaybook 0.4.0 or later (schema 8)`. With `workflow=classic`, the task-report, accept, claim, and run-insert triggers do not fire. A run update that leaves `write_stamp` unchanged still fails when that row's stored `workflow` is `gated`. The triggers are an accidental-old-binary guard, not a security boundary. A gated open that has to recreate a missing trigger includes `"ledger_guard": "restored"` in that command's `--json` data, including when the only restored trigger is `relaybook_guard_rereview_authorize` or `relaybook_guard_rereview_publish`.

## Roles

- Orca drives Relaybook one command at a time with `--json` and reads each JSON result. Orca does not use `relaybook run`.
- Claude plans. Cursor implements and fixes reopened tasks. Codex reviews. Codex does not edit code or write to the ledger. Orca records the verdict.
- Orca records its own ledger entries with `--by orca` on `relaybook decision log` and `relaybook report note`. Reviews are `--by codex`. Claims stay `--by cursor`, because the report author is the claimer.
- A review recorded `--by claude` needs the typed confirmation in [Sign-off](#sign-off). It is stored with `reviewer_override` 1. Any other name fails with `unconfigured reviewer`.
- The maintainer signs off. Orca cannot. `relaybook release authorize` requires a terminal on stdin and stdout.

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
9. The maintainer runs `relaybook release authorize --sha abcdef` at a terminal. The SHA must equal `HEAD`, and an approved release review must exist at that SHA. The prompt is `Authorize this release?` and the typed line starts with `authorize`. See [Sign-off](#sign-off).
10. Orca publishes only after `relaybook release status` shows an authorization at the current `HEAD`. Orca pushes and opens the pull request. Relaybook's manual commands never run `git push` or `gh`.

`relaybook task done` stores `commit_sha` in classic mode as well. Classic mode does not require a clean tree, a review, or a graph refresh.

## Sign-off

`relaybook release authorize`, a review recorded `--by claude`, `relaybook config set workflow classic` while the workflow is `gated`, `relaybook config set remote_check local`, the offline override inside `relaybook release authorize`, and any change of `signoff` each require a typed line. Stdin and stdout must both be terminals. The prompt prints the exact line: a verb, a space, and 12 hex digits. `y` does not confirm. The verbs are `authorize` for release authorization, `offline` when that authorization cannot reach a remote, `claude` for a Claude review, `classic` for leaving gated mode, `local` for `remote_check=local`, and `signoff` for a `signoff` change. For `authorize`, `offline`, and `claude`, the digits are the first 12 of the commit being confirmed. For the settings, they are the first 12 of `HEAD` when the directory has a commit, and otherwise a fresh 12-hex token printed in the prompt. The line must be entered within 120 seconds. The clock starts when the prompt is shown and is checked after the line is read. `RELAYBOOK_SIGNOFF_TIMEOUT_SECS` overrides that window. A late line fails with `confirmation challenge expired`. The JSON `method` is `tty-typed`. That means a deliberate typed confirmation at a terminal; not proof of a human.

`signoff` defaults to `typed`. `relaybook config set signoff signed-tag` also requires an annotated tag `relaybook-signoff/<sha12>` on the authorized commit. Relaybook resolves `refs/tags/relaybook-signoff/<sha12>` once, peels that object with `git rev-parse --verify <object>^{commit}`, and checks the signature with `git verify-tag` on that same object. A tag that moves during the check cannot mix a commit with another tag's signature. It never creates or pushes the tag. A missing tag, a tag on another commit, or a tag that is not a valid signature is refused. The stored `method` is `tty-typed+signed-tag`. `evidence` is three lines: the tag name, the tag object SHA, and one verifier line. The authorization write reads `signoff` again inside its ledger transaction and refuses the row when the setting is `signed-tag` unless that method and evidence are present. Changing `signoff` either way requires the typed confirmation. Setting the current value does not. `signed-tag` does not apply to the other confirmation sites.

A program that can read the prompt can type the line. A signing key that same user can use can satisfy `signed-tag`. Neither proves that a human is present. The maintainer keeps push credentials off the agent machine and protects `main` on GitHub with branch protection or rulesets, including code-owner review. The maintainer pushes and merges from their own machine after `relaybook release status` shows an authorization at HEAD. Relaybook leaves `gh` login and GitHub settings unchanged, and it cannot tell whether those protections are in place.

## Nothing is published before sign-off

`relaybook review record` and `relaybook release authorize` refuse when the branch, `HEAD`, or any commit in `merge-base(main, HEAD)..HEAD` is already on a configured remote. The online check is `git ls-remote` of every remote, before the ledger write. Inside the write, only local `refs/remotes` are read. A commit pushed under another name, a tag, or `refs/pull/*` still counts. A stale `refs/remotes/<remote>/<branch>` still refuses after that remote branch is deleted. Tracking refs are matched to configured remote names, so a slash stays part of the remote name. An unreachable remote, unreadable output, a timeout, or a failure to list that commit range fails closed (`remote <name>: timed out`, `remote <name>: ls-remote failed`, or `remote check failed`). The default limit is 20 seconds; `RELAYBOOK_REMOTE_TIMEOUT_SECS` overrides it. The wait ends at that limit even if a descendant still holds the child's pipes. The `ls-remote` child sets `GIT_TERMINAL_PROMPT=0` and reads no stdin. `relaybook config set remote_check local` skips the network check and requires a terminal; tracking refs still refuse. When authorize cannot verify the remotes it asks `Proceed without remote verification?`, requires the typed `offline` line from [Sign-off](#sign-off), and records `override` 1 on that authorization's check. `relaybook release status` includes `remote`. URL userinfo is stored and printed as `***@`, including a password that contains punctuation. The check does not see a repository that is not a configured remote, a push that went to a different push URL, or a push from another clone. Manual commands still never push. A person or an agent can run `git push` outside Relaybook.

**Auto mode today:**

- With `workflow=classic`, `relaybook run` pushes the run branch to `origin` after every accepted review and whenever it halts, then pushes again and runs `gh pr create` without a checkpoint. See [auto-mode.md](auto-mode.md#pushes-and-the-pull-request).
- With `workflow=gated`, `relaybook run` freezes `gated` on the run row. It plans with Claude, implements with Cursor, and reviews with Codex. It does not push on halt, after review, or before authorization. An approved release review waits at `gate_release`. A ledger has at most one live gated run. Starting or resuming another fails and does not push. `relaybook approve` at `gate_release` pushes the authorized commit with `--no-follow-tags` and opens one pull request. It does not push annotated tags. A second overlapping approve does not push or replace that completed run. Once the push has started, `relaybook run stop`, `relaybook run complete`, `relaybook run add`, and `relaybook run --resume` do not release the claim or push. If the publishing process dies, the run stays at `publishing` and holds the one live gated slot. The push result is unknown. The manual steps are in [Known limits](auto-mode.md#known-limits). Resuming another run while one is publishing does not push. Manual review and report accept follow the task's owning gated run. A workflow change during an agent call does not push. See [auto-mode.md](auto-mode.md#gated-workflow). `relaybook config set workflow classic` requires a terminal, including a value that only differs by surrounding whitespace, and it is refused while that run is active, waiting, or paused. The write checks again, so a run that becomes live during the confirmation still refuses.

## Reviewer edits

`relaybook review start` stores `HEAD`, a worktree fingerprint, and a ledger mark. `relaybook review record` fails with `reviewer changed the repository` or `ledger changed during review` if those change. Orca must write nothing between the two commands. The check detects an edit. It does not sandbox the reviewer.

The auto runner stops after Codex when the worktree, HEAD, a ref, git config, hooks, a tag, a remote-tracking ref, or the ledger mark changed. Nothing is pushed. The stop reasons are in [Codex review](auto-mode.md#codex-review).

## Graphify

In gated mode Relaybook runs `graphify update .` at each step boundary above, and, when that refresh is `built`, `graphify affected` for each file in the reviewed range. It never runs `graphify extract`. Agents may use `graphify query`, `graphify explain`, `graphify path`, `graphify affected`, and `graphify god-nodes`.

`graph_sha` is `HEAD` after a successful update on a clean tree. `.relaybook/` does not count as dirty. A missing `graphify` binary, a failed update, or no `graphify-out/graph.json` afterwards records `skipped` or `failed`, leaves `graph_sha` null, and puts `no graph` in the handoff. The command still succeeds.

`graph_context` is at most 4,000 bytes. A longer result ends with `[truncated]`.

`graphify-out/` must be gitignored. `relaybook setup --ci` tracks `graphify-out/graph.json`, so a repository set up that way cannot use `workflow=gated`. The refresh then records `failed` and `no graph: graphify-out/ is tracked`.

Between steps, Graphify's post-commit and post-checkout hooks and Relaybook's post-merge and post-rewrite hooks may refresh the graph. `relaybook setup --no-hooks` skips the hooks setup installs. Setup also skips Relaybook's hooks when `core.hooksPath` is set, or when Husky or Lefthook is present. The step-boundary refresh still runs.

The auto runner calls Graphify before planning, before each implementation step, and for task review and release review. It records `plan`, `implement`, `review`, and `release-review` on `graph_builds`. The planner prompt includes `Graph: <graph_sha or no graph> at <commit>`. The executor prompt adds `Base: <HEAD>` and that Graph line. A failed refresh does not stop the run. Classic mode does not refresh at claim, done, review, or in the auto runner.

`relaybook setup` and `relaybook uninstall --graphify` still run Graphify's install and uninstall commands. The opt-in workflow from `relaybook setup --ci` runs `graphify extract` in GitHub Actions when an LLM API secret is set, and `graphify update` otherwise. See [configuration.md](configuration.md#how-the-graph-stays-current).
