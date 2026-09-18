#!/usr/bin/env python3
"""Hold shell-release.yml's build-shell matrix to packaging/desktop-platforms.txt.

A platform the matrix builds with no row there ships a desktop whose
ServerPlatform resolves no target triple, so it can never fetch an engine.
native_engine.rs's own test holds that enum to the same file; this is the
matrix half, which no Rust test can see.
"""

import os
import sys
from pathlib import Path

import yaml

ROOT = Path(os.environ.get("DESKTOP_PLATFORM_ROOT") or Path(__file__).resolve().parent.parent)
PLATFORMS = ROOT / "packaging" / "desktop-platforms.txt"
MATRIX = ROOT / ".github" / "workflows" / "shell-release.yml"


def listed():
    # read_text raises on a missing file by design: an unreadable list must not
    # read as an empty set, which is a subset of any matrix.
    rows = (line.split("#")[0].split() for line in PLATFORMS.read_text().splitlines())
    return {(row[0], row[1]) for row in rows if row}


def built():
    job = yaml.safe_load(MATRIX.read_text())["jobs"]["build-shell"]
    return {(entry["os"], entry["arch"]) for entry in job["strategy"]["matrix"]["include"]}


def main():
    want, have = listed(), built()
    print(f"desktop-platforms.txt: {sorted(want)}")
    print(f"build-shell matrix:    {sorted(have)}")
    for pair in sorted(have - want):
        print(f"::error::build-shell builds {pair}, which desktop-platforms.txt does not "
              "list; that desktop would ship unable to fetch an engine", file=sys.stderr)
    for pair in sorted(want - have):
        print(f"::error::desktop-platforms.txt lists {pair}, which build-shell does not build",
              file=sys.stderr)
    return 0 if want == have else 1


if __name__ == "__main__":
    sys.exit(main())
