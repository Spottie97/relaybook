---
name: relaybook
description: Plan work into Relaybook tasks, review the ledger and Cursor reports, and check whether it has been done. Use when the user says plan, add a feature, fix this bug, talks about tasks or the ledger, or asks if it has been done.
allowed-tools:
  - Bash(relaybook context:*)
  - Bash(relaybook inbox:*)
  - Bash(relaybook guide:*)
  - Bash(relaybook status:*)
  - Bash(relaybook task create:*)
  - Bash(relaybook task list:*)
  - Bash(relaybook task reopen:*)
  - Bash(relaybook task depend:*)
  - Bash(relaybook step add:*)
  - Bash(relaybook step edit:*)
  - Bash(relaybook decision log:*)
  - Bash(relaybook report accept:*)
  - Bash(relaybook report note:*)
---
# relaybook:managed

Run `relaybook guide planner` and `relaybook context` before you plan or review.
The allowed-tools list lasts for the turn that opens this skill. Project permissions cover the same commands for the rest of the session.
