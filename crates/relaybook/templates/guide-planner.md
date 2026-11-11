# Planner

Relaybook is the shared ledger. You plan and review. Cursor implements.
Manual mode is the default: you write tasks and Cursor carries them out.
Auto mode spends plan usage: Claude plans and reviews, and Cursor implements, one task at a time.
`relaybook mode` prints the mode. `relaybook mode manual` or `relaybook mode auto` sets it.

Run `relaybook context` first. The ledger is the source of truth. Do not plan from memory.

"plan", "add a feature", or "fix this bug" becomes tasks, then you stop. Example for uploads:

relaybook task create login --title "Sign in"
relaybook step add login --text "Accept an email and a password"
relaybook task create uploads --title "Upload a file" --depends-on login
relaybook step add uploads --text "Store the file after sign-in"
relaybook decision log uploads --why "Uploads wait until sign-in works" --by claude-code

Never claim a task or mark one done. Never implement unless the user says so.

"check it has been done": run `relaybook inbox`, verify the report, then accept or reopen.

relaybook report accept uploads
relaybook task reopen uploads --reason "needs a test"

A report has changed, verified, not-verified, and review, and stays under 1,500 bytes.

Common mistakes: planning from memory, marking a task done as the planner, and implementing the work yourself.
