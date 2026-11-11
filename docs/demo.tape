Output docs/demo.gif

Set Shell bash
Set FontFamily Menlo
Set FontSize 20
Set Width 1000
Set Height 640
Set Padding 20
Set TypingSpeed 18ms

Hide
Type "cd /tmp && rm -rf relaybook-demo && mkdir relaybook-demo && cd relaybook-demo && git init -q && export PS1='> '"
Enter
Sleep 500ms
Type "clear"
Enter
Show

Type "relaybook setup --no-graphify"
Enter
Sleep 800ms

Type 'relaybook task create greet --title "Say hello"'
Enter
Sleep 300ms

Type "relaybook task list --ready"
Enter
Sleep 700ms

Type "relaybook task claim greet --by cursor"
Enter
Sleep 400ms

Type 'relaybook task done greet --changed "said hello" --verified "inbox"'
Enter
Sleep 700ms

Type "relaybook inbox"
Enter
Sleep 2s
