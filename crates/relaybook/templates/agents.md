## Relaybook coordination (executor)
.relaybook/ledger.db. Compact output is complete; each cut names its command.
1. `relaybook task list --ready`, then `relaybook task claim <slug> --by cursor`.
2. `relaybook context <slug>`, then `relaybook step check <slug> <n>`.
3. `relaybook progress report <slug> --note "<summary>"`.
4. `relaybook decision log <slug> --why "..." --by cursor`.
5. `relaybook task done <slug>`, or `relaybook task unblock <slug>` when resolved.
