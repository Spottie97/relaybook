#!/bin/sh
# Install the Relaybook binary and, unless --no-graphify is passed, Graphify.
# Supported platforms: linux x86_64, macos aarch64, and Windows x86_64 or ARM64 under Git Bash.
# Overrides: RELAYBOOK_INSTALL_DIR, RELAYBOOK_RELEASE_BASE, RELAYBOOK_OS, RELAYBOOK_ARCH.
set -eu

repo="Spottie97/relaybook"
release_base="${RELAYBOOK_RELEASE_BASE:-https://github.com/${repo}/releases/latest/download}"
no_graphify=0
upgrade=0
with_rtk=0
assume_yes=0
uninstall=0
graphify_added="none"
rtk_added="none"
rtk_init=0
path_added=0
path_dir=""
windows=0

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

os_name="${RELAYBOOK_OS:-$(uname -s)}"
case "$os_name" in
  MINGW*|MSYS*|CYGWIN*) windows=1 ;;
  *)
    if [ "${OS:-}" = "Windows_NT" ]; then
      echo "install.sh needs Git Bash on Windows. From PowerShell, run:" >&2
      echo "  irm https://raw.githubusercontent.com/Spottie97/relaybook/main/install.ps1 | iex" >&2
      exit 1
    fi
    ;;
esac

if [ "$windows" -eq 1 ]; then
  if [ -z "${LOCALAPPDATA:-}" ]; then
    echo "LOCALAPPDATA is not set" >&2
    exit 1
  fi
  local_app=$(cygpath -u "$LOCALAPPDATA")
  manifest_dir="$local_app/relaybook"
  if [ -n "${RELAYBOOK_INSTALL_DIR:-}" ]; then
    install_dir="$RELAYBOOK_INSTALL_DIR"
  else
    install_dir=$(cygpath -u "$LOCALAPPDATA/Programs/relaybook/bin")
  fi
else
  install_dir="${RELAYBOOK_INSTALL_DIR:-$HOME/.local/bin}"
  manifest_dir="${XDG_DATA_HOME:-$HOME/.local/share}/relaybook"
fi
manifest="$manifest_dir/install.json"

run_python() {
  if command -v python3 >/dev/null 2>&1; then
    python3 "$@"
  elif command -v python >/dev/null 2>&1; then
    python "$@"
  else
    echo "python3 is required to write the install manifest" >&2
    exit 1
  fi
}

json_get() {
  file=$1
  key=$2
  run_python - "$file" "$key" <<'PY'
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
  binary_present=0
  if [ -x "${RELAYBOOK_INSTALL_DIR:-$HOME/.local/bin}/relaybook" ] || [ -f "$install_dir/relaybook.exe" ]; then
    binary_present=1
  fi
  if [ ! -f "$manifest" ] && [ "$binary_present" -eq 0 ]; then
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
  target="${recorded:-$install_dir/relaybook}"
  if [ "$windows" -eq 1 ] && [ -z "$recorded" ]; then
    target="$install_dir/relaybook.exe"
  fi
  case "$target" in
    [A-Za-z]:*) target=$(cygpath -u "$target") ;;
  esac
  if [ -f "$target" ]; then
    rm -f "$target"
    echo "removed $target"
  fi
  path_state=$(json_get "$manifest" path_added)
  path_entry=$(json_get "$manifest" path_dir)
  if [ "$path_state" = "true" ] && [ -n "$path_entry" ]; then
    modify_user_path remove "$path_entry" >/dev/null
    echo "removed PATH entry $path_entry"
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
    winget)
      if ask_tool "Remove RTK installed by the installer?" "winget uninstall --id rtk-ai.rtk -e"; then
        powershell.exe -NoProfile -Command "winget uninstall --id rtk-ai.rtk -e"
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
  stored=$target
  if [ "$windows" -eq 1 ]; then
    stored=$(cygpath -w "$target")
  fi
  run_python - "$manifest" "$stored" "$graphify_added" "$rtk_added" "$rtk_init" "$path_added" "$path_dir" <<'PY'
import json, sys
path, binary, graphify, rtk, init, path_added, path_dir = sys.argv[1:]
json.dump(
    {
        "binary": binary,
        "graphify": graphify,
        "rtk": rtk,
        "rtk_init": init == "1",
        "path_added": path_added == "1",
        "path_dir": path_dir,
    },
    open(path, "w"),
    indent=2,
)
open(path, "a").write("\n")
PY
}

modify_user_path() {
  action=$1
  dir=$2
  # shellcheck disable=SC2016
  RELAYBOOK_PATH_ACTION=$action RELAYBOOK_PATH_DIR=$dir powershell.exe -NoProfile -Command '
    $dir = $env:RELAYBOOK_PATH_DIR
    $action = $env:RELAYBOOK_PATH_ACTION
    $user = [Environment]::GetEnvironmentVariable("Path", "User")
    if ([string]::IsNullOrEmpty($user)) { $user = "" }
    $norm = $dir.Replace([char]47, [char]92).TrimEnd([char]92)
    $items = @()
    if ($user -ne "") { $items = @($user.Split([char]59) | Where-Object { $_ -ne "" }) }
    $kept = @()
    $found = $false
    foreach ($item in $items) {
      if ($item.Replace([char]47, [char]92).TrimEnd([char]92) -ieq $norm) { $found = $true }
      else { $kept += $item }
    }
    if ($action -eq "add") {
      if ($found) { Write-Output "present"; exit 0 }
      if ($kept.Count -eq 0) { $updated = $norm } else { $updated = $norm + [char]59 + ($kept -join [char]59) }
      [Environment]::SetEnvironmentVariable("Path", $updated, "User")
      Write-Output "added"
      exit 0
    }
    if (-not $found) { Write-Output "absent"; exit 0 }
    [Environment]::SetEnvironmentVariable("Path", ($kept -join [char]59), "User")
    Write-Output "removed"
  '
}

if [ "$uninstall" -eq 1 ]; then
  uninstall_machine
fi

os=$os_name
arch="${RELAYBOOK_ARCH:-$(uname -m)}"
binary_name="relaybook"
archive_ext="tar.gz"
if [ "$windows" -eq 1 ]; then
  binary_name="relaybook.exe"
  archive_ext="zip"
  case "$arch" in
    x86_64|amd64|AMD64) win_arch="x86_64" ;;
    aarch64|arm64|ARM64) win_arch="aarch64" ;;
    *)
      case "${PROCESSOR_ARCHITECTURE:-}" in
        AMD64) win_arch="x86_64" ;;
        ARM64) win_arch="aarch64" ;;
        *)
          echo "unsupported architecture: $arch (supported: x86_64, aarch64)" >&2
          exit 1
          ;;
      esac
      ;;
  esac
  artifact="relaybook-windows-$win_arch"
else
  case "$os:$arch" in
    Linux:x86_64) artifact="relaybook-linux-x86_64" ;;
    Darwin:arm64) artifact="relaybook-macos-aarch64" ;;
    *)
      echo "unsupported platform: $os $arch (supported: linux x86_64, macos aarch64, windows x86_64, windows aarch64)" >&2
      exit 1
      ;;
  esac
fi

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
  archive="$artifact.$archive_ext"
  url="$release_base/$archive"
  if [ -d "$release_base" ]; then
    if [ ! -f "$release_base/$archive" ]; then
      echo "could not download $release_base/$archive" >&2
      exit 1
    fi
    cp "$release_base/$archive" "$tmp/$archive"
  elif ! curl -fsSL "$url" -o "$tmp/$archive"; then
    echo "could not download $url" >&2
    exit 1
  fi
  if [ "$archive_ext" = "zip" ]; then
    if command -v unzip >/dev/null 2>&1; then
      unzip -q "$tmp/$archive" -d "$tmp"
    else
      win_zip=$(cygpath -w "$tmp/$archive")
      win_dest=$(cygpath -w "$tmp")
      powershell.exe -NoProfile -Command "Expand-Archive -LiteralPath '$win_zip' -DestinationPath '$win_dest' -Force"
    fi
  else
    tar -xzf "$tmp/$archive" -C "$tmp"
  fi
  if [ ! -f "$tmp/$binary_name" ]; then
    echo "archive did not contain an relaybook binary" >&2
    exit 1
  fi
  chmod +x "$tmp/$binary_name"
  cp "$tmp/$binary_name" "$install_dir/$binary_name.new"
  mv "$install_dir/$binary_name.new" "$target"
  trap - EXIT
  cleanup
}

mkdir -p "$install_dir"
target="$install_dir/$binary_name"
present=0
if [ "$windows" -eq 1 ]; then
  if [ -f "$target" ]; then
    present=1
  fi
elif [ -x "$target" ]; then
  present=1
fi

if [ "$present" -eq 1 ] && [ "$upgrade" -eq 0 ]; then
  echo "already installed; re-run with --upgrade to update"
elif [ "$present" -eq 1 ]; then
  old_version=$("$target" --version 2>/dev/null || echo "unknown")
  download_binary
  new_version=$("$target" --version 2>/dev/null || echo "unknown")
  echo "$old_version -> $new_version"
else
  download_binary
  echo "installed relaybook to $target"
fi

if [ "$windows" -eq 1 ]; then
  if [ -f "$manifest" ]; then
    old_path=$(json_get "$manifest" path_added)
    if [ "$old_path" = "true" ]; then
      path_added=1
      path_dir=$(json_get "$manifest" path_dir)
    fi
  fi
  win_dir=$(cygpath -w "$install_dir")
  path_result=$(modify_user_path add "$win_dir") || exit 1
  path_result=$(printf '%s\n' "$path_result" | tail -n 1)
  if [ "$path_result" = "added" ]; then
    path_added=1
    path_dir=$win_dir
  fi
  echo "Open a new terminal to use relaybook."
elif ! on_path "$install_dir"; then
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
  if [ "$windows" -eq 1 ]; then
    if command -v winget >/dev/null 2>&1 || command -v winget.exe >/dev/null 2>&1; then
      if winget install --id rtk-ai.rtk -e --accept-source-agreements --accept-package-agreements; then
        rtk_added="winget"
      else
        echo "warning: rtk install failed; Relaybook is still installed" >&2
      fi
    else
      echo "rtk was not installed. See https://www.rtk-ai.app/ or run: winget install rtk-ai.rtk" >&2
    fi
  elif command -v brew >/dev/null 2>&1; then
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
