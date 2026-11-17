# Changelog

All notable changes to Relaybook are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - Unreleased

### Added

- Schema 7. An older ledger migrates in place: reports gain `commit_sha`, `base_sha`, and `graph_sha`, and the ledger stores reviews, graph builds, and release authorizations.
- `reviewer` and `workflow` settings. Defaults stay `reviewer=claude` and `workflow=classic`. `workflow=gated` requires `reviewer=codex`.
- `relaybook task done` records `commit_sha` in classic and gated mode. Gated claim and done require a clean tree.
- Gated graph refresh records `graph_sha` and a `graph_context` of at most 4,000 bytes. Relaybook runs `graphify update .` at those step boundaries.
- `relaybook review start` and `relaybook review record` for a task. A changes-requested verdict reopens the task. Gated `relaybook report accept` requires an approved review at `HEAD`.
- Release review, `relaybook release authorize`, and `relaybook release status`. The maintainer authorizes one commit at a terminal after an approved release review at `HEAD`.
- `relaybook guide reviewer` for Codex. Planner, executor, and skill text tell agents never to run `graphify extract`.

### Changed

- The prompt hook prints `N new reports: relaybook inbox`.
- Docs describe the manual gated workflow as implemented and pin the minimum version: gated work requires Relaybook 0.4.0 or later (ledger schema 7). 0.3.3 and earlier open a schema 7 ledger and ignore the gates.
- With `workflow=classic`, auto mode still pushes the run branch before sign-off. With `workflow=gated`, `relaybook run`, `relaybook run --resume`, and `relaybook approve` exit 1 before any agent, git, or push: "relaybook run is not available with workflow=gated in this build". `relaybook run stop`, `relaybook run complete`, `relaybook run watch`, and `relaybook run add` still run. `relaybook config set workflow classic` requires confirmation at a terminal when the setting is `gated`. A value that trims to `classic` is the same change. The write checks the current workflow again, so a concurrent change to `gated` fails and the setting stays `gated` unless that confirmation already happened. Without a terminal the command fails and the setting stays `gated`. Setting `workflow=gated` does not ask. Codex routing in the auto runner is not built.
- The checks behind gated review, release authorization, and graph refresh are shared. Their messages and JSON are unchanged.
- `scripts/agent-smoke.sh` checks a real `graphify` binary: an existing merge driver is kept, `CLAUDE.md` drift is warned, and `relaybook uninstall --dry-run --graphify` lists Graphify's side effects. Its graphify phase clears inherited `GIT_DIR`, `GIT_WORK_TREE`, and `GIT_INDEX_FILE` and uses a temporary HOME. The agent phase still uses the caller's HOME, and the optional build writes under the checkout. The script stays manual and is not part of CI.

### Fixed

- `relaybook setup` keeps an existing `merge.graphify.driver`, including a driver in global or system git config and the driver `graphify hook install` registers. When that key is unset in the effective git configuration, setup still registers a repository-local `graphify merge-driver %O %A %B`.
- `relaybook setup` warns `CLAUDE.md: warn (graphify claude install changed CLAUDE.md)` when that command changes `CLAUDE.md`. The warning does not fail setup.
- `relaybook uninstall --dry-run --graphify` lists what Graphify's uninstall commands change: the Graphify section of `CLAUDE.md`, Graphify hooks in `.claude/settings.json`, `.cursor/rules/graphify.mdc`, Graphify git hooks, the `graphify-out/graph.json` merge attribute, and `merge.graphify.*`.

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
