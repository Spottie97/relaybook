# Changelog

All notable changes to Relaybook are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - Unreleased

### Added

- Schema 7. An older ledger migrates in place: reports gain `commit_sha`, `base_sha`, and `graph_sha`, and the ledger stores reviews, graph builds, and release authorizations.
- Schema 8. An older ledger gains `runs.workflow`, `runs.write_stamp`, `release_authorizations.method`, `release_authorizations.evidence`, and `remote_checks`. While `workflow=gated`, five triggers refuse the 0.3.3 task-report, accept, claim, and run writes with `workflow=gated: this ledger needs relaybook 0.4.0 or later (schema 8)`. With `workflow=classic`, the task-report, accept, claim, and run-insert triggers do not fire. A run update that leaves `write_stamp` unchanged still fails when that row's stored `workflow` is `gated`. Partial unique index `runs_one_live_gated` allows one gated run that is `active`, `waiting`, or `paused`, or whose stage is `publishing` after the push has started. A read-write open creates that index on an existing schema 8 ledger and does not change `user_version`. A read-write or read-only open of a newer `user_version` fails with `this ledger needs a newer relaybook`. The read-write migration re-reads `user_version` under its write lock, including a database with no `events` table, and does not relabel a newer ledger. `relaybook init` uses that check and does not apply the schema, set `user_version`, or change journal mode before it. A gated open that recreates a missing trigger adds `ledger_guard` `restored` to that command's `--json` data. `scripts/agent-smoke.sh` checks these guards with a 0.3.3 build and with sqlite3 3.45.1.
- `reviewer` and `workflow` settings. Defaults stay `reviewer=claude` and `workflow=classic`. `workflow=gated` requires `reviewer=codex`.
- `relaybook task done` records `commit_sha` in classic and gated mode. Gated claim and done require a clean tree.
- Gated graph refresh records `graph_sha` and a `graph_context` of at most 4,000 bytes. Relaybook runs `graphify update .` at those step boundaries.
- `relaybook review start` and `relaybook review record` for a task. A changes-requested verdict reopens the task. Gated `relaybook report accept` requires an approved review at `HEAD`.
- Release review, `relaybook release authorize`, and `relaybook release status`. The maintainer authorizes one commit at a terminal after an approved release review at `HEAD`.
- Sign-off is a typed challenge on stdin and stdout. `y` is not accepted. `relaybook release authorize` asks for `authorize` plus the first 12 hex digits of the commit. The same challenge covers a Claude review (`claude`), leaving `workflow=gated` (`classic`), `relaybook config set remote_check local` (`local`), the offline release override (`offline`), and a change to `signoff` (`signoff`). Settings use the first 12 hex digits of `HEAD` when the directory has a commit, and otherwise a fresh 12-hex token printed in the prompt. The line must be read within 120 seconds; the clock is checked after the line is read. `RELAYBOOK_SIGNOFF_TIMEOUT_SECS` overrides that window. The authorization `method` is `tty-typed`: a deliberate typed confirmation at a terminal; not proof of a human. `signoff` defaults to `typed`. `signoff=signed-tag` also requires an annotated tag `relaybook-signoff/<sha12>` on that commit. Relaybook resolves `refs/tags/relaybook-signoff/<sha12>` once, peels that object, and checks the signature with `git verify-tag` on that same object. A tag that moves during the check cannot mix a commit with another tag's signature. Relaybook does not create or push the tag. That row's `method` is `tty-typed+signed-tag`, and `evidence` stores the tag name, the tag object SHA, and one verifier line. The authorization write reads `signoff` again inside its ledger transaction and refuses the row when the setting is `signed-tag` unless the method is `tty-typed+signed-tag` with evidence. Changing `signoff` requires the typed challenge.
- `relaybook guide reviewer` for Codex. Planner, executor, and skill text tell agents never to run `graphify extract`.
- Gated `review record` and `release authorize` check every configured remote with `git ls-remote` before the ledger write, and local `refs/remotes` again inside it. A commit already on a remote is refused, including one pushed under another name, as a tag, or as a pull ref. `HEAD` counts even when the release range is empty. If `main` or that commit list cannot be resolved, the check is unknown and fails closed. An unreachable remote or a timeout fails closed, and the wait ends even when a descendant still holds the child's pipes. `remote_check` defaults to `online`. `relaybook config set remote_check local` skips the network check and requires a terminal; local tracking refs still refuse, including when the remote name contains a slash. When `release authorize` cannot reach a remote it asks `Proceed without remote verification?` and records that override on the authorization check. `release status` includes the latest check. URL userinfo, including a password that contains punctuation, is redacted to `***@`. `RELAYBOOK_REMOTE_TIMEOUT_SECS` overrides the 20 second limit.

### Changed

- The prompt hook prints `N new reports: relaybook inbox`.
- Docs describe the manual gated workflow as implemented and pin the minimum version: gated work requires Relaybook 0.4.0 or later (ledger schema 8). A newer ledger is refused on read-write and read-only opens, and `relaybook init` does not rewrite one to schema 8. While `workflow=gated`, the schema 8 triggers refuse the old task-report, accept, claim, and run writes. With `workflow=classic`, a run update that leaves `write_stamp` unchanged still fails when that row's stored `workflow` is `gated`. Docs also describe one live gated run per ledger and that a started push stays claimed across `relaybook run stop` and `relaybook run --resume`.
- With `workflow=classic`, auto mode still pushes the run branch before sign-off. With `workflow=gated`, `relaybook run` stores `workflow=gated` on the run and keeps that value. It plans with Claude and implements with Cursor, then stops at review with "codex routing not built". A halt does not push, and neither does a run that reaches release review. `relaybook approve` at `gate_release` publishes only after `relaybook release authorize` for that HEAD: it pushes that exact commit with `git push --no-follow-tags -u origin <sha>:refs/heads/<branch>` and runs `gh pr create` once. Annotated tags reachable from that commit, including a sign-off tag, are not pushed. Only one overlapping approve takes the run, pushes, and opens the pull request; a stale approve does not push or replace the completed run. A ledger has at most one live gated run. Starting or resuming another while one is live fails and does not push. Classic runs are outside that limit. Once the push has started, `relaybook run stop` and `relaybook run --resume` do not release that claim, do not push, and do not open a pull request. The publishing process records completion, or stops the run if publish fails. Resuming a different run while one is publishing does not push or open a pull request. The runner keeps driving the run it claimed. Without that authorization it exits 1 with "waiting for sign-off: relaybook release authorize --sha <HEAD>" and pushes nothing. If HEAD has moved, publish stops with "HEAD moved after authorization" and does not push. A missing origin stops publish without a pull request. While a gated run is active, waiting, or paused, or while its stage is `publishing`, review, report accept for a task owned by that run, release review, and `relaybook config set workflow` fail with "a gated run owns this; use relaybook approve / run --resume". The task check uses that task's owning run, so a newer run that does not own the task does not allow the write. Those checks run again inside the ledger write, so a gated run that becomes live while a confirmation is still pending still refuses. `relaybook release authorize` is allowed when no gated run is live, or when the run is waiting at `gate_release`. The authorization write checks that again. When that run is waiting on the authorized branch, the write stores its id. A run that resumes into another stage while the challenge is pending is refused, and no authorization row is stored. `relaybook release status` stays available. Changing the workflow after the run has stopped, then resuming, stops with "workflow changed during run" and does not push. A classic run, or a run whose stored workflow is unset, also stops without pushing when it is resumed after the setting changed to gated. The same stop happens when the setting changes while an agent is running, including a timeout or a guard stop, so that halt does not push. `relaybook run stop` does not change a run whose push has started. `relaybook run stop`, `relaybook run complete`, `relaybook run watch`, and `relaybook run add` otherwise still run. `relaybook config set workflow classic` requires the typed sign-off challenge at a terminal when the setting is `gated` and no gated run is live. A value that trims to `classic` is the same change. The write checks the current workflow again, so a concurrent change to `gated` fails and the setting stays `gated` unless that confirmation already happened. Without a terminal the command fails and the setting stays `gated`. Setting `workflow=gated` does not ask. Codex routing in the auto runner is not built.
- The checks behind gated review, release authorization, and graph refresh are shared. Their messages and JSON are unchanged.
- Relaybook resolves `graphify`, `claude`, `cursor-agent`, `agent`, and `bash` with `which` 8.0.6. A relative `PATH` entry is returned as an absolute path, so a program found that way still runs when Relaybook sets the command's working directory to the repository root. On Windows, an empty `PATH` entry no longer selects a program in the current directory.
- Relaybook links rusqlite 0.42.0 and libsqlite3-sys 0.40.0. The bundled SQLite engine stays 3.54.1, so SQL, migrations, write-lock rechecks, and the busy timeout are unchanged.
- `scripts/agent-smoke.sh` checks a real `graphify` binary: an existing merge driver is kept, `CLAUDE.md` drift is warned, and `relaybook uninstall --dry-run --graphify` lists Graphify's side effects. Its graphify phase clears inherited `GIT_DIR`, `GIT_WORK_TREE`, and `GIT_INDEX_FILE` and uses a temporary HOME. The agent phase still uses the caller's HOME, and the optional build writes under the checkout. The script stays manual and is not part of CI.

### Fixed

- `relaybook setup` keeps an existing `merge.graphify.driver`, including a driver in global or system git config and the driver `graphify hook install` registers. When that key is unset in the effective git configuration, setup still registers a repository-local `graphify merge-driver %O %A %B`.
- `relaybook setup` warns `CLAUDE.md: warn (graphify claude install changed CLAUDE.md)` when that command changes `CLAUDE.md`. The warning does not fail setup.
- `relaybook uninstall --dry-run --graphify` lists what Graphify's uninstall commands change: the Graphify section of `CLAUDE.md`, Graphify hooks in `.claude/settings.json`, `.cursor/rules/graphify.mdc`, Graphify git hooks, the `graphify-out/graph.json` merge attribute, and `merge.graphify.*`.
- Tests that run graph refresh with Graphify hidden drop every `PATH` entry where `graphify` resolves, including Windows `.cmd` and `.bat` shims.

## [0.3.3] - 2026-11-12

### Fixed

- `relaybook setup` guards Graphify hooks under every event, including `SessionStart`. An absolute `graphify` path is rewritten, so a committed `.claude/settings.json` does not leak a home directory or fail when Graphify is missing.

## [0.3.2] - 2026-11-11

### Added

- Claude can run the safe Relaybook commands without a prompt. Setup pre-approves context, inbox, guide, status, task create and list, reopen, depend, step add and edit, decision log, and report accept and note. Claim, done, run, approve, setup, uninstall, mode, and gates still ask.

## [0.3.1] - 2026-11-11

### Added

- `relaybook setup` teaches a fresh agent its role. The planner and executor blocks name when to plan, claim, and review. `relaybook guide` prints the worked examples. Claude Code gets a Relaybook skill and a `SessionStart` hook that speaks up only when work is waiting.

## [0.3.0] - 2026-11-11

### Added

- Auto runs print each phase, tool call, and the first line of agent text as they happen, and append the same lines to `.relaybook/runs/<id>/live.log`. `relaybook run watch` follows that log until the run pauses or ends. `relaybook run --quiet` prints the phase headers and the closing summary. `relaybook status` shows the latest live lines.
- Native Windows support: PowerShell `install.ps1` and Git Bash `install.sh`, Windows x64 and ARM64 release archives, the Cursor CLI name `agent`, and a prompt hook that uses the guarded POSIX command or a PowerShell command when Git Bash is absent.

### Fixed

- `relaybook setup` writes `.gitignore` and `.gitattributes` entries in a `# relaybook:start` block. A trailing `# relaybook` comment is not valid git syntax, so those ignore rules and the Graphify merge driver never took effect. Every setup repairs an old attribute line, including `setup` without `--ci`.
- Auto runs adopt unfinished tasks that were created before `relaybook run`. `relaybook run --new-only` skips them. `relaybook run add <slug>` links a task to the active run.

## [0.2.1] - 2026-11-11

### Fixed

- The Linux release binary is fully static (musl), so it runs on older glibc and on Alpine.
- The publish job checks out the repository before it creates the GitHub release.

## [0.2.0] - 2026-11-11

### Added

- SQLite coordination ledger: tasks, atomic claim, append-only events, dependencies, block and unblock, and rename.
- Compact `context` and `task list`, and short output for writes.
- A structured report on `task done`, plus an inbox for reviewing those reports.
- A Claude Code prompt hook that surfaces new reports.
- `relaybook setup`, including Graphify hooks and an opt-in CI workflow.
- A one-line installer with `--upgrade`.
- Auto mode: Claude plans and reviews, Cursor implements, with checkpoints and usage limits.
- `/relaybook` in Claude Code and Cursor, and terminal pickers for `mode` and `gates`.
- `relaybook uninstall` removes what `relaybook setup` added to a project. `install.sh --uninstall` removes the binary and, when the installer added them, Graphify and RTK.
- `relaybook step add <slug> --text "..."` for step text that starts with a dash. The positional form still works.

### Changed

- Renamed from AgentMesh to Relaybook.

### Fixed

- `relaybook uninstall` lists each file once, says `restored` when previous bytes are put back, and says `would remove`, `would restore`, or `would keep` on `--dry-run`. Empty folders setup created are removed.
- `relaybook task done` on an open task fails with `claim it first: relaybook task claim <slug> --by <agent>` and writes nothing.
- Auto mode stops when Claude edits the worktree.
- The Claude Code `/relaybook` command starts with front matter on line 1.
