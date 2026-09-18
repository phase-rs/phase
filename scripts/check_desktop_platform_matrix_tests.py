#!/usr/bin/env python3
"""Legs for check_desktop_platform_matrix.py: agreement, a grown matrix, a
dropped platform, a missing list. The direction that matters is refusal: an
unreadable list file reads as an empty set, and the empty set is a subset of any
matrix, so a checker that understood nothing could report a clean pass.
"""

import os
import subprocess
import sys
import tempfile
from pathlib import Path

CHECKER = Path(__file__).resolve().parent / "check_desktop_platform_matrix.py"
MATRIX = """\
jobs:
  build-shell:
    strategy:
      matrix:
        include:
          - {os: linux, arch: x86_64, runner: ubuntu-latest}
          - {os: macos, arch: aarch64, runner: macos-latest}
"""
LISTING = "# fields: os arch triple\nlinux x86_64  x86_64-unknown-linux-musl\nmacos aarch64 aarch64-apple-darwin\n"


def run(matrix, listing):
    root = Path(tempfile.mkdtemp())
    (root / ".github" / "workflows").mkdir(parents=True)
    (root / ".github" / "workflows" / "shell-release.yml").write_text(matrix)
    if listing is not None:
        (root / "packaging").mkdir()
        (root / "packaging" / "desktop-platforms.txt").write_text(listing)
    return subprocess.run(
        [sys.executable, str(CHECKER)], capture_output=True, text=True,
        env={**os.environ, "DESKTOP_PLATFORM_ROOT": str(root)},
    )


agree = run(MATRIX, LISTING)
assert agree.returncode == 0, agree.stderr
assert "('linux', 'x86_64')" in agree.stdout, agree.stdout
assert "('macos', 'aarch64')" in agree.stdout, agree.stdout

grown = run(MATRIX + "          - {os: windows, arch: x86_64, runner: windows-latest}\n", LISTING)
assert grown.returncode != 0, grown.stdout
assert "('windows', 'x86_64')" in grown.stderr, grown.stderr

dropped = run(MATRIX, LISTING + "freebsd x86_64 nope\n")
assert dropped.returncode != 0, dropped.stdout
assert "('freebsd', 'x86_64')" in dropped.stderr, dropped.stderr

absent = run(MATRIX, None)
assert absent.returncode != 0, absent.stdout
assert "desktop-platforms.txt" in absent.stderr, absent.stderr
assert "('linux', 'x86_64')" not in absent.stdout, absent.stdout

print("ok: agreement, grown matrix, dropped platform, missing list")
