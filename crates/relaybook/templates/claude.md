## Relaybook coordination (planner)
Relaybook tracks the work (.relaybook/ledger.db). Graphify maps the code.

Compact output is complete. Each cut names the command that restores it.
1. `relaybook context`, then `relaybook context <slug>` for one task.
2. `relaybook decision log <slug> --why "..." --by claude-code`.
3. `relaybook task create <slug> --title "..." --depends-on <slug>`, then `relaybook step add`.
The executor runs `relaybook task done`.
