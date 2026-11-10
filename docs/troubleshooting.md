# Troubleshooting

- The installer supports linux x86_64 and macos aarch64. On another machine, use `cargo build --release`.
- If `~/.local/bin` is not on `PATH`, the installer still finishes, then prints the `export PATH=...` line to add to `~/.zshrc` or `~/.bashrc`. `RELAYBOOK_INSTALL_DIR` chooses a different directory. A plain re-run skips an existing binary. Update it with the installer flag `--upgrade`.
- If Graphify is missing, install [uv](https://docs.astral.sh/uv/) and run `uv tool install graphifyy`. `pipx install graphifyy` is the fallback. Setup still finishes and skips the Graphify steps. Skip Graphify during install with `curl -fsSL https://raw.githubusercontent.com/Spottie97/relaybook/main/install.sh | sh -s -- --no-graphify`.
- If `core.hooksPath` is set, or the repo uses Husky or Lefthook, setup does not write `.git/hooks`. It prints where to add post-merge and post-rewrite commands that run `graphify update .` in the background.

## Build

```sh
cargo build --release
```

The binary is `target/release/relaybook`. It is a single static executable. SQLite is compiled in via `rusqlite` with the `bundled` feature. There is no server and no daemon.

Release archives are `relaybook-macos-aarch64.tar.gz` and `relaybook-linux-x86_64.tar.gz`.
