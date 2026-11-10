<!-- relaybook:start -->
## Relaybook coordination (planner)
Relaybook tracks the work (.relaybook/ledger.db). Graphify maps the code.

Compact output is complete. Each cut names the command that restores it.
1. When the hook reports new items, run `relaybook inbox`, then `relaybook report accept <slug>` or `relaybook task reopen <slug> --reason "..."`. Larger problems become new tasks.
2. `relaybook context`, then `relaybook context <slug>` for one task.
3. `relaybook decision log <slug> --why "..." --by claude-code`.
4. `relaybook task create <slug> --title "..." --depends-on <slug>`, then `relaybook step add`.
<!-- relaybook:end -->
