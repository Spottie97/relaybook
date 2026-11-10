---
# relaybook:managed
description: "Relaybook: mode, checkpoints, auto runs, inbox"
argument-hint: "[mode|gates|run|approve|inbox|status] [value]"
allowed-tools: Bash(relaybook:*)
---

!`relaybook status`

When `$ARGUMENTS` is not empty, run `relaybook $ARGUMENTS` and reply in one line.

When `$ARGUMENTS` is empty, show that status. Use AskUserQuestion and offer only what applies:
- Switch mode: `relaybook mode manual` or `relaybook mode auto`
- Change checkpoints: `relaybook gates none`, `relaybook gates plan`, or `relaybook gates plan,review`
- Start a run: ask for the PRD path, then Bash `relaybook run <prd>` with `run_in_background`. Tell the user to watch it with `/relaybook status`. A checkpoint shows up through the prompt hook.
- Approve, only when a run is waiting: `relaybook approve`
- Inbox, only when reports are unreviewed: `relaybook inbox`

Run the relaybook command for the item they pick.
