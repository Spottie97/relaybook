<!-- relaybook:managed -->
# /relaybook

Text after `/relaybook` is the argument list. `cursor-agent` reads this file from `.cursor/commands/` as well.

If the user passed arguments, run `relaybook` with those arguments and reply in one line.

If there are no arguments, run `relaybook status` and list only the choices that apply:
- Switch mode: `relaybook mode manual` or `relaybook mode auto`
- Change checkpoints: `relaybook gates none`, `relaybook gates plan`, or `relaybook gates plan,review`
- Start a run: ask for the PRD path, then run `relaybook run <prd>`
- Approve, only when a run is waiting: `relaybook approve`
- Inbox, only when reports are unreviewed: `relaybook inbox`

Ask the user to pick one, then run that command.
