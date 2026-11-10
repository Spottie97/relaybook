## Relaybook coordination (executor)
.relaybook/ledger.db. Compact output is complete; each cut names its command.
1. Claim first: `relaybook task list --ready` has `reopened: <reason>`. `relaybook task claim <slug> --by cursor`.
2. `relaybook step check <slug> <n>`.
3. If you must deviate from the plan: `relaybook decision log <slug> --why "..." --by cursor`.
4. `relaybook task done <slug> --changed "..." --verified "..." --not-verified "..." --review "..."` under 1500 bytes. Cross-task: `relaybook report note --by cursor --text "..."`.
