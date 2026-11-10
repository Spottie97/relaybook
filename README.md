<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
    <img alt="Relaybook" src="assets/logo-light.svg" width="420">
  </picture>
</h1>

<p align="center">Shared work ledger for coding agents.</p>

<p align="center">
  <a href="https://github.com/Spottie97/relaybook/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/Spottie97/relaybook/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/Spottie97/relaybook/releases"><img alt="Latest release" src="https://img.shields.io/github/v/release/Spottie97/relaybook"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
  <img alt="Platforms: macOS arm64 and Linux x86_64" src="https://img.shields.io/badge/platforms-macOS%20arm64%20%7C%20Linux%20x86__64-lightgrey">
</p>

<p align="center">
  <a href="#quick-start">Install</a>
  · <a href="#how-it-works">How it works</a>
  · <a href="#auto-mode">Auto mode</a>
  · <a href="#docs">Docs</a>
  · <a href="CONTRIBUTING.md">Contributing</a>
</p>

## What it is

Relaybook is a shared coordination layer for coding agents. One Rust binary keeps tasks, progress, and decisions in a SQLite ledger, so agents in the same repo stop stepping on each other.

Graphify maps the code. Relaybook tracks the work. Use Graphify for structure and Relaybook for state.

## Why

Agents step on the same task, re-explain state every session, and pay context cost for work that is already finished. The ledger is the hand-off. One agent plans, another executes, and the report comes back through an inbox. Finished work stays behind a command.

## Features

- Tasks, an atomic claim, dependencies, and an append-only event log
- A short report on `relaybook task done`, and `relaybook inbox` for the planner
- Auto mode: Claude plans and reviews, Cursor implements, one task at a time
- `/relaybook` in Claude Code and Cursor
- Compact `relaybook context` by default, with the full history one flag away

## Quick start

```sh
curl -fsSL https://raw.githubusercontent.com/Spottie97/relaybook/main/install.sh | sh
```

Then, in the project:

```sh
relaybook setup
```

Upgrade an existing install with:

```sh
curl -fsSL https://raw.githubusercontent.com/Spottie97/relaybook/main/install.sh | sh -s -- --upgrade
```

Any agent that can run a shell command can join. There is no server and no protocol.

<img alt="Demo: setup, claim a task, file a report, and read the inbox" src="docs/demo.gif" width="720">

## How it works

```mermaid
flowchart LR
  planner[Planner] --> ledger[Ledger]
  ledger --> executor[Executor]
  executor --> report[Report]
  report --> inbox[Inbox]
  inbox --> review[Review]
```

```sh
relaybook task list --ready
relaybook task claim greet --by cursor
relaybook task done greet --changed "said hello" --verified "cargo test"
relaybook inbox
```

The executor claims a ready task and finishes with a report under 1,500 bytes. The planner runs `relaybook inbox`, then `relaybook report accept greet` or `relaybook task reopen greet --reason "needs a test"`. A reopened task returns to `relaybook task list --ready`.

## Auto mode

Manual mode is the default. `/relaybook` shows the current status and a menu. `/relaybook mode` prints the current mode. `/relaybook mode manual` and `/relaybook mode auto` set it. `/relaybook gates` prints the checkpoints. `/relaybook gates none`, `/relaybook gates plan`, and `/relaybook gates plan,review` set them. The terminal commands do the same: `relaybook mode` and `relaybook gates` print the current value, and a value sets it.

| Action | Slash command | Terminal |
| --- | --- | --- |
| Status and the menu | `/relaybook` | `relaybook status` |
| Mode | `/relaybook mode` or `/relaybook mode auto` | `relaybook mode` or `relaybook mode auto` |
| Checkpoints | `/relaybook gates` or `/relaybook gates plan` | `relaybook gates` or `relaybook gates plan` |
| Start a run | `/relaybook run docs/prd.md` | `relaybook run docs/prd.md` |
| Approve a checkpoint | `/relaybook approve` | `relaybook approve` |
| Inbox | `/relaybook inbox` | `relaybook inbox` |

```sh
relaybook mode auto
relaybook gates plan
relaybook run docs/prd.md
```

`plan` pauses after planning. Continue with `relaybook approve`. Limits, safety checks, and a PRD template are in [docs/auto-mode.md](docs/auto-mode.md).

## Token savings

On this ledger, `relaybook context --all` is 102,072 bytes (about 25,518 tokens) and the default `relaybook context` is 6,386 bytes (about 1,597 tokens), about 94% less. Those counts are bytes/4 estimates. Done tasks stay behind `relaybook context --all`. On a 40-done fixture, context fell from 4,567 bytes (about 1,141 tokens) to 1,482 bytes (about 370 tokens).

Graphify answers where the code lives. RTK compacts command output. Relaybook compacts the work ledger. The always-loaded planner and executor blocks are 520 and 544 bytes. `scripts/measure-context.sh --check` guards those budgets.

## FAQ

**Where does the data live?** In `.relaybook/ledger.db` at the git root. SQLite is compiled into the binary. There is no daemon.

**What if two agents claim the same task?** One `relaybook task claim` wins. The other exits 1 with `already claimed`.

**Which machines can install a release?** macOS arm64 and Linux x86_64. On another machine, run `cargo build --release`.

## Docs

- [Commands](docs/commands.md)
- [Configuration](docs/configuration.md)
- [Auto mode](docs/auto-mode.md)
- [Ledger](docs/ledger.md)
- [Troubleshooting](docs/troubleshooting.md)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
