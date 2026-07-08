# iTerm2 Shift+Enter Verification

Use these steps to verify goose multiline input in a real iTerm2 session.

## Reproduce The Default Behavior

1. Open iTerm2 with a profile that has no custom Shift+Enter binding.
2. Start `goose` in interactive mode.
3. Type `first line`, press Shift+Enter, then type `second line`.
4. If iTerm2 sends plain Enter, goose submits `first line` instead of inserting a newline.
5. Press Option+Enter in the same prompt. goose should insert a newline because Option+Enter sends ESC followed by carriage return.

## Install

1. In goose, run `/terminal-setup`.
2. Confirm that goose detects `iTerm2`.
3. Confirm the preview shows the iTerm2 preferences domain `com.googlecode.iterm2`, `GlobalKeyMap[0xd-0x20000-0x24]`, and a `defaults write` command whose value sends `0x1b 0x0d`.
4. Answer `y` at the `y/N` prompt.
5. Open a new iTerm2 window or restart iTerm2 if the current window does not pick up the change.

## Verify

1. Start `goose` again in iTerm2.
2. Type `first line`, press Shift+Enter, then type `second line`.
3. Confirm the input box contains both lines and does not submit until plain Enter is pressed.
4. Run `/terminal-setup` again and confirm goose reports the binding is already installed instead of adding a duplicate.

## Roll Back

Remove the Shift+Enter entry from iTerm2 Settings > Keys > Key Bindings, or run:

```bash
/usr/bin/defaults delete com.googlecode.iterm2 GlobalKeyMap 0xd-0x20000-0x24
```
