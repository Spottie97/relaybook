#!/bin/sh
# Install the Relaybook binary and, unless --no-graphify is passed, Graphify.
# Supported platforms: linux x86_64 and macos aarch64.
# Overrides: RELAYBOOK_INSTALL_DIR, RELAYBOOK_RELEASE_BASE, RELAYBOOK_OS, RELAYBOOK_ARCH.
set -eu

repo="Spottie97/relaybook"
install_dir="${RELAYBOOK_INSTALL_DIR:-$HOME/.local/bin}"
release_base="${RELAYBOOK_RELEASE_BASE:-https://github.com/${repo}/releases/latest/download}"
no_graphify=0
upgrade=0
with_rtk=0
assume_yes=0

for arg in "$@"; do
  case "$arg" in
    --no-graphify) no_graphify=1 ;;
    --upgrade) upgrade=1 ;;
    --with-rtk) with_rtk=1 ;;
    --yes) assume_yes=1 ;;
    *)
      echo "unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

os="${RELAYBOOK_OS:-$(uname -s)}"
arch="${RELAYBOOK_ARCH:-$(uname -m)}"
case "$os:$arch" in
  Linux:x86_64) artifact="relaybook-linux-x86_64" ;;
  Darwin:arm64) artifact="relaybook-macos-aarch64" ;;
  *)
    echo "unsupported platform: $os $arch (supported: linux x86_64, macos aarch64)" >&2
    exit 1
    ;;
esac

on_path() {
  case ":$PATH:" in
    *":$1:"*) return 0 ;;
    *) return 1 ;;
  esac
}

profile_file() {
  shell_name=${SHELL:-}
  shell_name=${shell_name##*/}
  case "$shell_name" in
    zsh) printf '%s\n' "$HOME/.zshrc" ;;
    bash) printf '%s\n' "$HOME/.bashrc" ;;
    *) printf '%s\n' "your shell profile" ;;
  esac
}

download_binary() {
  tmp=$(mktemp -d)
  cleanup() {
    rm -rf "$tmp"
  }
  trap cleanup EXIT
  tarball="$artifact.tar.gz"
  url="$release_base/$tarball"
  if ! curl -fsSL "$url" -o "$tmp/$tarball"; then
    echo "could not download $url" >&2
    exit 1
  fi
  tar -xzf "$tmp/$tarball" -C "$tmp"
  if [ ! -f "$tmp/relaybook" ]; then
    echo "archive did not contain an relaybook binary" >&2
    exit 1
  fi
  chmod +x "$tmp/relaybook"
  cp "$tmp/relaybook" "$install_dir/relaybook.new"
  mv "$install_dir/relaybook.new" "$target"
  trap - EXIT
  cleanup
}

mkdir -p "$install_dir"
target="$install_dir/relaybook"

if [ -x "$target" ] && [ "$upgrade" -eq 0 ]; then
  echo "already installed; re-run with --upgrade to update"
elif [ -x "$target" ]; then
  old_version=$("$target" --version 2>/dev/null || echo "unknown")
  download_binary
  new_version=$("$target" --version 2>/dev/null || echo "unknown")
  echo "$old_version -> $new_version"
else
  download_binary
  echo "installed relaybook to $target"
fi

if ! on_path "$install_dir"; then
  profile=$(profile_file)
  echo "warning: $install_dir is not on PATH" >&2
  echo "add this line to $profile:" >&2
  echo "  export PATH=\"$install_dir:\$PATH\"" >&2
fi

if [ "$no_graphify" -eq 1 ]; then
  echo "skipped graphify (--no-graphify)"
elif [ "$upgrade" -eq 1 ] && { command -v graphify >/dev/null 2>&1 || [ -x "$HOME/.local/bin/graphify" ]; }; then
  if command -v uv >/dev/null 2>&1 && uv tool list 2>/dev/null | grep -q graphifyy; then
    uv tool upgrade graphifyy || echo "warning: graphify upgrade failed; Relaybook is still installed" >&2
  elif command -v pipx >/dev/null 2>&1 && pipx list 2>/dev/null | grep -q graphifyy; then
    pipx upgrade graphifyy || echo "warning: graphify upgrade failed; Relaybook is still installed" >&2
  else
    echo "warning: could not tell how graphify was installed; skipped the graphify upgrade" >&2
  fi
elif command -v graphify >/dev/null 2>&1; then
  echo "graphify already installed"
else
  if command -v uv >/dev/null 2>&1; then
    uv tool install graphifyy || echo "graphify install failed; Relaybook is still installed" >&2
  elif command -v pipx >/dev/null 2>&1; then
    pipx install graphifyy || echo "graphify install failed; Relaybook is still installed" >&2
  else
    echo "graphify was not installed. Install uv or pipx, then run: uv tool install graphifyy" >&2
    echo "or: pipx install graphifyy" >&2
  fi
fi

if [ "$with_rtk" -eq 0 ]; then
  :
elif command -v rtk >/dev/null 2>&1; then
  echo "rtk already installed"
else
  if command -v brew >/dev/null 2>&1; then
    brew install rtk || echo "warning: rtk install failed; Relaybook is still installed" >&2
  else
    echo "rtk was not installed. See https://www.rtk-ai.app/ or run: brew install rtk" >&2
  fi
fi

if [ "$with_rtk" -eq 1 ] && command -v rtk >/dev/null 2>&1; then
  if [ "$assume_yes" -eq 1 ]; then
    rtk init -g || echo "warning: rtk init failed; Relaybook is still installed" >&2
  elif [ -t 0 ]; then
    printf 'Add the RTK hook to your global Claude settings? [y/N] '
    read -r answer || answer=""
    case "$answer" in
      y|Y|yes|YES) rtk init -g || echo "warning: rtk init failed; Relaybook is still installed" >&2 ;;
      *) echo "skipped rtk init; run: rtk init -g" ;;
    esac
  else
    echo "skipped rtk init; run: rtk init -g"
  fi
fi

"$target" --version
if [ "$no_graphify" -eq 0 ]; then
  if command -v graphify >/dev/null 2>&1; then
    graphify --version 2>/dev/null || echo "graphify installed"
  elif [ -x "$HOME/.local/bin/graphify" ]; then
    "$HOME/.local/bin/graphify" --version 2>/dev/null || echo "graphify installed"
  fi
fi
