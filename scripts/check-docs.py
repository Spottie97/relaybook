#!/usr/bin/env python3
"""Fail when a Relaybook example in the README or docs/ does not parse."""

import re
import shlex
import subprocess
import sys
from pathlib import Path
from typing import Optional

ROOT = Path(__file__).resolve().parents[1]


def binary() -> Optional[Path]:
    debug = ROOT / "target" / "debug"
    for name in ("relaybook", "relaybook.exe"):
        path = debug / name
        if path.is_file():
            return path
    return None


FILES = [ROOT / "README.md", *sorted((ROOT / "docs").glob("*.md"))]
FENCE = re.compile(r"^```")
INLINE = re.compile(r"`(relaybook(?:\s[^`]*)?)`")


def commands(text: str) -> list[str]:
    found: list[str] = []
    fenced = False
    pending = ""
    for raw in text.splitlines():
        if FENCE.match(raw.strip()):
            fenced = not fenced
            pending = ""
            continue
        if fenced:
            line = raw.strip()
            if line.startswith("$ "):
                line = line[2:]
            if line.endswith("\\"):
                pending += line[:-1].strip() + " "
                continue
            line = (pending + line).strip()
            pending = ""
            if line.startswith("relaybook ") or line == "relaybook":
                found.append(line)
            continue
        for match in INLINE.finditer(raw):
            found.append(match.group(1).strip())
    return found


def main() -> int:
    bin_path = binary()
    if bin_path is None:
        missing = ROOT / "target" / "debug" / "relaybook"
        print(f"missing {missing}; run cargo build --bin relaybook", file=sys.stderr)
        return 1
    failed = 0
    seen = 0
    for path in FILES:
        for command in commands(path.read_text(encoding="utf-8")):
            seen += 1
            try:
                args = shlex.split(command)
            except ValueError as err:
                print(f"{path.name}: cannot parse {command!r}: {err}", file=sys.stderr)
                failed += 1
                continue
            if not args or args[0] != "relaybook":
                print(f"{path.name}: not a relaybook command: {command}", file=sys.stderr)
                failed += 1
                continue
            result = subprocess.run(
                [str(bin_path), *args[1:], "--help"],
                check=False,
                capture_output=True,
                text=True,
                encoding="utf-8",
            )
            if result.returncode != 0:
                print(f"{path.name}: {command}", file=sys.stderr)
                sys.stderr.write(result.stderr)
                failed += 1
    if seen == 0:
        print("no relaybook examples found", file=sys.stderr)
        return 1
    if failed:
        print(f"{failed} example(s) failed to parse", file=sys.stderr)
        return 1
    print(f"{seen} relaybook examples parse")
    return 0


if __name__ == "__main__":
    sys.exit(main())
