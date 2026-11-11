## Relaybook coordination (planner)
.relaybook/ledger.db. Graphify maps the code. Compact output is complete; each cut names its command.
1. Hook reports items: `relaybook inbox`, then `relaybook report accept <slug>` or `relaybook task reopen <slug> --reason "..."`. Larger problems become new tasks.
2. `relaybook context`, then `relaybook context <slug>`.
3. `relaybook decision log <slug> --why "..." --by claude-code`.
4. `relaybook task create <slug> --title "..." --depends-on <slug>`, then `relaybook step add <slug> --text "..."`.
