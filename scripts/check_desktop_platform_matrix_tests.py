#!/usr/bin/env python3
"""Legs for check_desktop_platform_matrix.py: agreement, a grown matrix, a
dropped platform, a missing list, a free matrix axis, a grown preview matrix, a
platform nothing releases, a malformed row. The direction that matters is
refusal: an unreadable list must raise, not read as an empty list.
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
PREVIEW = """\
jobs:
  build:
    strategy:
      matrix:
        include:
          - {triple: x86_64-unknown-linux-musl}
          - {triple: aarch64-apple-darwin}
"""
RELEASE = """\
jobs:
  build-server:
    strategy:
      matrix:
        include:
          - {triple: x86_64-unknown-linux-musl}
          - {triple: aarch64-apple-darwin}
"""
LISTING = "# fields: os arch triple\nlinux x86_64  x86_64-unknown-linux-musl\nmacos aarch64 aarch64-apple-darwin\n"


def run(matrix, listing, preview=PREVIEW, release=RELEASE):
    root = Path(tempfile.mkdtemp())
    (root / ".github" / "workflows").mkdir(parents=True)
    (root / ".github" / "workflows" / "shell-release.yml").write_text(matrix)
    (root / ".github" / "workflows" / "preview-server.yml").write_text(preview)
    (root / ".github" / "workflows" / "release.yml").write_text(release)
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

free = run(MATRIX + "        arch: [riscv64]\n", LISTING)
assert free.returncode != 0, free.stdout
assert "free axes ['arch']" in free.stderr, free.stderr

previewed = run(MATRIX, LISTING, PREVIEW + "          - {triple: riscv64-unknown-linux-musl}\n")
assert previewed.returncode != 0, previewed.stdout
assert "riscv64-unknown-linux-musl" in previewed.stderr, previewed.stderr

# The reviewer's reproduction: a platform every other matrix gained, which the
# release never builds, so its desktop resolves a download URL that 404s.
unreleased = run(
    MATRIX + "          - {os: linux, arch: riscv64, runner: ubuntu-latest}\n",
    LISTING + "linux riscv64 riscv64-unknown-linux-musl\n",
    PREVIEW + "          - {triple: riscv64-unknown-linux-musl}\n",
)
assert unreleased.returncode != 0, unreleased.stdout
assert "release build-server does not build" in unreleased.stderr, unreleased.stderr
assert "riscv64-unknown-linux-musl" in unreleased.stderr, unreleased.stderr

malformed = run(MATRIX, "linux x86_64\n")
assert malformed.returncode != 0, malformed.stdout
assert "['linux', 'x86_64']" in malformed.stderr, malformed.stderr

print("ok: agreement, grown matrix, dropped platform, missing list, free axis, "
      "grown preview matrix, unreleased platform, malformed row")
