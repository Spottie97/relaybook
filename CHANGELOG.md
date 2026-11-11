# Changelog

All notable changes to Relaybook are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
