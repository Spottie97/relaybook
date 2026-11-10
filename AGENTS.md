## Relaybook coordination (executor)
Relaybook is the shared work ledger (.relaybook/ledger.db). Check it before doing work
and update it as you go.

1. `relaybook task list --ready`, then `relaybook task claim <slug> --by cursor`.
   Claim fails if another agent holds it, or if it is `waiting on` another task; pick
   another ready task if so.
2. `relaybook step check <slug> <n>` as each checklist step completes.
3. After every commit: `relaybook progress report <slug> --note "<commit summary>"`.
4. If reality forces a deviation from the plan: `relaybook decision log <slug> --why
   "..." --by cursor` before continuing.
5. `relaybook task done <slug>` when the checklist is complete.
6. `relaybook task unblock <slug>` when the blocking reason is resolved.
