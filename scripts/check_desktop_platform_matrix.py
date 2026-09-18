#!/usr/bin/env python3
"""Hold the desktop build matrices to packaging/desktop-platforms.txt.

A platform shell-release.yml's build-shell matrix builds with no row there ships
a desktop whose ServerPlatform resolves no target triple, so it can never fetch
an engine; a triple preview-server.yml builds with no row there provisions a
preview no desktop asks for. native_engine.rs's own test holds that enum to the
same file; these are the matrix halves, which no Rust test can see.
"""

import os
import sys
from pathlib import Path

import yaml

ROOT = Path(os.environ.get("DESKTOP_PLATFORM_ROOT") or Path(__file__).resolve().parent.parent)
PLATFORMS = ROOT / "packaging" / "desktop-platforms.txt"
MATRIX = ROOT / ".github" / "workflows" / "shell-release.yml"
PREVIEW = ROOT / ".github" / "workflows" / "preview-server.yml"


def listed():
    # read_text raises on a missing file by design: an unreadable list must not
    # read as an empty set, which is a subset of any matrix.
    rows = [line.split("#")[0].split() for line in PLATFORMS.read_text().splitlines()]
    rows = [row for row in rows if row]
    if malformed := [row for row in rows if len(row) != 3]:
        sys.exit(f"::error::desktop-platforms.txt rows are not `os arch triple`: {malformed}")
    return {tuple(row) for row in rows}


def include(path, job):
    # A matrix runs the product of its free axes too, so reading include alone
    # would report those builds as not happening.
    matrix = yaml.safe_load(path.read_text())["jobs"][job]["strategy"]["matrix"]
    if free := sorted(set(matrix) - {"include", "exclude"}):
        sys.exit(f"::error::{path.name} {job} matrix has free axes {free}, whose product builds "
                 "platforms this checker cannot see")
    return matrix["include"]


def built():
    return {(entry["os"], entry["arch"]) for entry in include(MATRIX, "build-shell")}


def previewed():
    return {entry["triple"] for entry in include(PREVIEW, "build")}


def main():
    rows = listed()
    want, have = {row[:2] for row in rows}, built()
    print(f"desktop-platforms.txt: {sorted(want)}")
    print(f"build-shell matrix:    {sorted(have)}")
    for pair in sorted(have - want):
        print(f"::error::build-shell builds {pair}, which desktop-platforms.txt does not "
              "list; that desktop would ship unable to fetch an engine", file=sys.stderr)
    for pair in sorted(want - have):
        print(f"::error::desktop-platforms.txt lists {pair}, which build-shell does not build",
              file=sys.stderr)
    triples, preview = {row[2] for row in rows}, previewed()
    print(f"desktop-platforms.txt triples: {sorted(triples)}")
    print(f"preview-server build matrix:   {sorted(preview)}")
    for triple in sorted(preview - triples):
        print(f"::error::preview-server builds {triple}, which desktop-platforms.txt does not "
              "list; no desktop would ever ask for that preview", file=sys.stderr)
    for triple in sorted(triples - preview):
        print(f"::error::desktop-platforms.txt lists {triple}, which preview-server does not build",
              file=sys.stderr)
    return 0 if (want, triples) == (have, preview) else 1


if __name__ == "__main__":
    sys.exit(main())
