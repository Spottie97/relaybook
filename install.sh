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
uninstall=0
graphify_added="none"
rtk_added="none"
rtk_init=0

for arg in "$@"; do
  case "$arg" in
    --no-graphify) no_graphify=1 ;;
    --upgrade) upgrade=1 ;;
    --with-rtk) with_rtk=1 ;;
    --yes) assume_yes=1 ;;
    --uninstall) uninstall=1 ;;
    *)
      echo "unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

manifest_dir="${XDG_DATA_HOME:-$HOME/.local/share}/relaybook"
manifest="$manifest_dir/install.json"

json_get() {
  file=$1
  key=$2
  python3 - "$file" "$key" <<'PY'
import json, sys
try:
    data = json.load(open(sys.argv[1]))
except FileNotFoundError:
    sys.exit(0)
value = data.get(sys.argv[2], "")
if value is True:
    print("true")
elif value is False:
    print("false")
else:
    print(value)
PY
}

ask_tool() {
  prompt=$1
  command_text=$2
  if [ "$assume_yes" -eq 1 ]; then
    return 0
  fi
  if [ ! -t 0 ]; then
    echo "$command_text"
    return 1
  fi
  printf '%s [y/N] ' "$prompt"
  read -r answer || answer=""
  case "$answer" in
    y|Y|yes|YES) return 0 ;;
    *) return 1 ;;
  esac
}

uninstall_machine() {
  if [ ! -f "$manifest" ] && [ ! -x "${RELAYBOOK_INSTALL_DIR:-$HOME/.local/bin}/relaybook" ]; then
    echo "nothing installed"
    exit 0
  fi
  recorded=""
  graphify_state="none"
  rtk_state="none"
  init_state="false"
  if [ -f "$manifest" ]; then
    recorded=$(json_get "$manifest" binary)
    graphify_state=$(json_get "$manifest" graphify)
    rtk_state=$(json_get "$manifest" rtk)
    init_state=$(json_get "$manifest" rtk_init)
  fi
  target="${recorded:-${RELAYBOOK_INSTALL_DIR:-$HOME/.local/bin}/relaybook}"
  if [ -f "$target" ]; then
    rm -f "$target"
    echo "removed $target"
  fi
  if command -v relaybook >/dev/null 2>&1; then
    found=$(command -v relaybook)
    case "$found" in
      */.cargo/bin/relaybook) echo "cargo uninstall relaybook" ;;
      *) echo "kept $found (not installed by install.sh)" ;;
    esac
  fi
  case "$graphify_state" in
    pre-existing) echo "kept graphify (already installed)" ;;
    uv)
      if ask_tool "Remove Graphify installed by the installer?" "uv tool uninstall graphifyy"; then
        uv tool uninstall graphifyy
        echo "removed graphifyy"
      fi
      ;;
    pipx)
      if ask_tool "Remove Graphify installed by the installer?" "pipx uninstall graphifyy"; then
        pipx uninstall graphifyy
        echo "removed graphifyy"
      fi
      ;;
  esac
  if [ "$init_state" = "true" ]; then
    if ask_tool "Remove the RTK Claude hook?" "rtk init -g --uninstall"; then
      rtk init -g --uninstall
      echo "removed rtk hook"
    fi
  fi
  case "$rtk_state" in
    pre-existing) echo "kept rtk (already installed)" ;;
    brew)
      if ask_tool "Remove RTK installed by the installer?" "brew uninstall rtk"; then
        brew uninstall rtk
        echo "removed rtk"
      fi
      ;;
  esac
  echo "project files stay until you run relaybook uninstall in each project"
  rm -f "$manifest"
  rmdir "$manifest_dir" 2>/dev/null || true
  exit 0
}

write_manifest() {
  mkdir -p "$manifest_dir"
  python3 - "$manifest" "$target" "$graphify_added" "$rtk_added" "$rtk_init" <<'PY'
import json, sys
path, binary, graphify, rtk, init = sys.argv[1:]
json.dump(
    {
        "binary": binary,
        "graphify": graphify,
        "rtk": rtk,
        "rtk_init": init == "1",
    },
    open(path, "w"),
    indent=2,
)
open(path, "a").write("\n")
PY
}

if [ "$uninstall" -eq 1 ]; then
  uninstall_machine
fi

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

if command -v graphify >/dev/null 2>&1 || [ -x "$HOME/.local/bin/graphify" ]; then
  graphify_added="pre-existing"
fi
if command -v rtk >/dev/null 2>&1; then
  rtk_added="pre-existing"
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
    if uv tool install graphifyy; then
      graphify_added="uv"
    else
      echo "graphify install failed; Relaybook is still installed" >&2
    fi
  elif command -v pipx >/dev/null 2>&1; then
    if pipx install graphifyy; then
      graphify_added="pipx"
    else
      echo "graphify install failed; Relaybook is still installed" >&2
    fi
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
    if brew install rtk; then
      rtk_added="brew"
    else
      echo "warning: rtk install failed; Relaybook is still installed" >&2
    fi
  else
    echo "rtk was not installed. See https://www.rtk-ai.app/ or run: brew install rtk" >&2
  fi
fi

if [ "$with_rtk" -eq 1 ] && command -v rtk >/dev/null 2>&1; then
  if [ "$assume_yes" -eq 1 ]; then
    if rtk init -g; then
      rtk_init=1
    else
      echo "warning: rtk init failed; Relaybook is still installed" >&2
    fi
  elif [ -t 0 ]; then
    printf 'Add the RTK hook to your global Claude settings? [y/N] '
    read -r answer || answer=""
    case "$answer" in
      y|Y|yes|YES)
        if rtk init -g; then
          rtk_init=1
        else
          echo "warning: rtk init failed; Relaybook is still installed" >&2
        fi
        ;;
      *) echo "skipped rtk init; run: rtk init -g" ;;
    esac
  else
    echo "skipped rtk init; run: rtk init -g"
  fi
fi

write_manifest

"$target" --version
if [ "$no_graphify" -eq 0 ]; then
  if command -v graphify >/dev/null 2>&1; then
    graphify --version 2>/dev/null || echo "graphify installed"
  elif [ -x "$HOME/.local/bin/graphify" ]; then
    "$HOME/.local/bin/graphify" --version 2>/dev/null || echo "graphify installed"
  fi
fi
