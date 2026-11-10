#!/bin/sh
# Install the AgentMesh binary and, unless --no-graphify is passed, Graphify.
# Supported platforms: linux x86_64 and macos aarch64.
# Overrides: AGENTMESH_INSTALL_DIR, AGENTMESH_RELEASE_BASE, AGENTMESH_OS, AGENTMESH_ARCH.
set -eu

repo="Spottie97/AgentMesh"
install_dir="${AGENTMESH_INSTALL_DIR:-$HOME/.local/bin}"
release_base="${AGENTMESH_RELEASE_BASE:-https://github.com/${repo}/releases/latest/download}"
no_graphify=0
upgrade=0

for arg in "$@"; do
  case "$arg" in
    --no-graphify) no_graphify=1 ;;
    --upgrade) upgrade=1 ;;
    *)
      echo "unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

os="${AGENTMESH_OS:-$(uname -s)}"
arch="${AGENTMESH_ARCH:-$(uname -m)}"
case "$os:$arch" in
  Linux:x86_64) artifact="agentmesh-linux-x86_64" ;;
  Darwin:arm64) artifact="agentmesh-macos-aarch64" ;;
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
  if [ ! -f "$tmp/agentmesh" ]; then
    echo "archive did not contain an agentmesh binary" >&2
    exit 1
  fi
  chmod +x "$tmp/agentmesh"
  cp "$tmp/agentmesh" "$install_dir/agentmesh.new"
  mv "$install_dir/agentmesh.new" "$target"
  trap - EXIT
  cleanup
}

mkdir -p "$install_dir"
target="$install_dir/agentmesh"

if [ -x "$target" ] && [ "$upgrade" -eq 0 ]; then
  echo "already installed; re-run with --upgrade to update"
elif [ -x "$target" ]; then
  old_version=$("$target" --version 2>/dev/null || echo "unknown")
  download_binary
  new_version=$("$target" --version 2>/dev/null || echo "unknown")
  echo "$old_version -> $new_version"
else
  download_binary
  echo "installed agentmesh to $target"
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
    uv tool upgrade graphifyy || echo "warning: graphify upgrade failed; AgentMesh is still installed" >&2
  elif command -v pipx >/dev/null 2>&1 && pipx list 2>/dev/null | grep -q graphifyy; then
    pipx upgrade graphifyy || echo "warning: graphify upgrade failed; AgentMesh is still installed" >&2
  else
    echo "warning: could not tell how graphify was installed; skipped the graphify upgrade" >&2
  fi
elif command -v graphify >/dev/null 2>&1; then
  echo "graphify already installed"
else
  if command -v uv >/dev/null 2>&1; then
    uv tool install graphifyy || echo "graphify install failed; AgentMesh is still installed" >&2
  elif command -v pipx >/dev/null 2>&1; then
    pipx install graphifyy || echo "graphify install failed; AgentMesh is still installed" >&2
  else
    echo "graphify was not installed. Install uv or pipx, then run: uv tool install graphifyy" >&2
    echo "or: pipx install graphifyy" >&2
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
