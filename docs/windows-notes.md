# Windows notes

Facts checked on 2026-11-11 for the Windows port. Each item cites the page it came from.

## Claude Code hooks

A shell-form hook (`command` with no `args`) runs in Git Bash on Windows, and in PowerShell when Git Bash is not installed. Git for Windows is not required. The `shell` field can force `"bash"` or `"powershell"`.

Relaybook's prompt hook is shell form and uses `command -v`. That syntax runs under Git Bash and fails under PowerShell.

Source: [Hooks reference](https://code.claude.com/docs/en/hooks)

## Claude Code commands

`.claude/commands/*.md` is still loaded and creates a slash command from the file name, the same way as a skill. The docs do not give Windows a different project path. On Windows, `~/.claude` is `%USERPROFILE%\.claude`.

Source: [Explore the .claude directory](https://code.claude.com/docs/en/claude-directory) and [Extend Claude with skills](https://code.claude.com/docs/en/skills)

## Cursor CLI

Cursor CLI has a native Windows install. In PowerShell:

```powershell
irm 'https://cursor.com/install?win32=true' | iex
```

The documented command is `agent` (`agent --version`, `agent update`). The install page does not name `cursor-agent`. Relaybook currently looks for `cursor-agent` on `PATH`.

Source: [CLI installation](https://cursor.com/docs/cli/installation) and [CLI overview](https://cursor.com/docs/cli/overview)

## Cursor commands

Project commands are markdown files in `.cursor/commands`, invoked with `/`. The docs do not describe a different directory on Windows.

Source: [Deeplinks](https://cursor.com/docs/reference/deeplinks) and [Customize Cursor](https://cursor.com/docs/customize-cursor)

## Graphify via uv and pipx

`uv tool install` copies executables into the tool bin directory on Windows instead of symlinking them. The default directory is `%USERPROFILE%\.local\bin` (`uv tool dir --bin`).

`pipx` puts console scripts in `PIPX_BIN_DIR`, default `~/.local/bin`, and copies them on Windows when the directory does not support symlinks. Windows file names keep an extension such as `.exe`, so the command on disk is `graphify.exe`. Both directories are `%USERPROFILE%\.local\bin` unless overridden.

Sources: [uv tools](https://docs.astral.sh/uv/concepts/tools/), [uv storage](https://docs.astral.sh/uv/reference/storage/), [How pipx works](https://pipx.pypa.io/stable/explanation/how-pipx-works.html), [pipx paths](https://pipx.pypa.io/stable/how-to/configure-paths.html)

## RTK

RTK has a native Windows build. Install with `winget install rtk-ai.rtk`, or unzip `rtk-x86_64-pc-windows-msvc.zip` and put `rtk.exe` on `PATH`. Since v0.37.2, `rtk init -g` installs a native hook (`rtk hook claude`) and does not need a Unix shell. Homebrew is not the Windows install path.

Source: [RTK installation](https://www.rtk-ai.app/docs/getting-started/installation/)
