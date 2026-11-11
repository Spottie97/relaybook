# Troubleshooting

- The installer supports linux x86_64, macos aarch64, and Windows x64 and ARM64. On another machine, use `cargo build --release`.
- If `~/.local/bin` is not on `PATH`, the installer still finishes, then prints the `export PATH=...` line to add to `~/.zshrc` or `~/.bashrc`. `RELAYBOOK_INSTALL_DIR` chooses a different directory. A plain re-run skips an existing binary. Update it with the installer flag `--upgrade`.
- If Graphify is missing, install [uv](https://docs.astral.sh/uv/) and run `uv tool install graphifyy`. `pipx install graphifyy` is the fallback. Setup still finishes and skips the Graphify steps. Skip Graphify during install with `curl -fsSL https://raw.githubusercontent.com/Spottie97/relaybook/main/install.sh | sh -s -- --no-graphify`.
- If `core.hooksPath` is set, or the repo uses Husky or Lefthook, setup does not write `.git/hooks`. It prints where to add post-merge and post-rewrite commands that run `graphify update .` in the background.

## Build

```sh
cargo build --release
```

The binary is `target/release/relaybook`. SQLite is compiled in via `rusqlite` with the `bundled` feature. The Linux release binary is fully static (musl). The macOS release binary is self-contained and links only system libraries. The Windows release binaries are statically linked. There is no server and no daemon.

Release archives are `relaybook-macos-aarch64.tar.gz`, `relaybook-linux-x86_64.tar.gz`, `relaybook-windows-x86_64.zip`, and `relaybook-windows-aarch64.zip`.

## Windows

- Install [Git for Windows](https://gitforwindows.org/). Relaybook keeps the ledger in a git repository and installs git hooks. Claude Code runs shell hooks in Git Bash when `bash.exe` is on PATH. The `System32` WSL stub does not count; without Git Bash `bash.exe`, those hooks run in PowerShell. PowerShell installs Relaybook with `install.ps1`. Git Bash can use the same `install.sh` one-liner as macOS and Linux.
- The published `relaybook.exe` is unsigned. SmartScreen can show "Windows protected your PC". Choose More info, then Run anyway. If Microsoft Defender quarantines the file, restore `relaybook.exe` and allow it.
- The installer adds `%LOCALAPPDATA%\Programs\relaybook\bin` to the user PATH. That change applies to a new terminal. The installer prints `Open a new terminal to use relaybook.`
- Auto mode uses Claude Code and the Cursor CLI (`cursor-agent`, or `agent` from `irm 'https://cursor.com/install?win32=true' | iex`). A WSL shell can install the Linux archive with the `install.sh` one-liner instead of the Windows exe.
