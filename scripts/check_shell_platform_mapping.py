#!/usr/bin/env python3
"""Every desktop platform the shell release publishes must have an engine mapping.

`shell-release.yml`'s `build-shell` matrix is the set of desktop platforms a tag
publishes. `native_engine.rs`'s `SERVER_TARGET_TRIPLES` is how each of those
desktops resolves which engine binary to fetch. A platform published without a
mapping entry ships a desktop that cannot acquire an engine at all.

`native_engine.rs`'s own unit test pins the pairs it expects, which is what makes
it discriminating against the mapping being deleted or broken. What it cannot see
is the matrix growing past them: nothing there refers to the workflow. This gate
is that tie, and it runs on every pull request rather than at tag time.

Both populations are counted before the subset is checked, because every way of
failing to read either file yields an empty set and an empty matrix is a subset
of any mapping -- a gate that understood nothing would print a pass. A read that
does not find the expected number of entries refuses instead.

The mapping is read by regex over the constant's source, so reformatting that
array breaks this gate loudly rather than silently. A matrix that grows from four
platforms to five fails its count assertion for the same reason, and that is the
event this gate exists to announce: the published platform set changed, so the
mapping has to be checked against it.
"""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path

try:
    import yaml
except ModuleNotFoundError:
    # Exit 2, not 1: `main` reserves 1 for "a published platform has no mapping"
    # and 2 for "I could not read this". A missing parser is the second kind, and
    # collapsing them would make the refusal unreadable from the exit code.
    print("REFUSED: check_shell_platform_mapping: PyYAML is required and was "
          "not found; refusing to check with a weaker method", file=sys.stderr)
    sys.exit(2)

ROOT = Path(os.environ.get("SHELL_PLATFORM_MAPPING_ROOT")
            or Path(__file__).resolve().parent.parent).resolve()

MAPPING_SOURCE = "client/src-tauri/src/native_engine.rs"
SHELL_RELEASE = ".github/workflows/shell-release.yml"
BUILD_JOB = "build-shell"

#: Anchored on the symbol name, not on a type spelling: `&[T]`, `[T; N]` and a
#: type alias all name the same constant.
MAPPING_BLOCK = re.compile(
    r"const SERVER_TARGET_TRIPLES\s*:[^=]*=\s*&?\[(.*?)\n\];", re.S)
MAPPING_ENTRY = re.compile(
    r'\(\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*\)\s*,\s*"([^"]+)"\s*\)')

#: The desktop platforms a tag publishes, and the mapping entries that serve
#: them. Both are expectations, not observations: a change to either is the event
#: this gate reports, so it fails and names which side moved.
PUBLISHED_PLATFORM_COUNT = 4
MAPPED_PLATFORM_COUNT = 4


class Refusal(Exception):
    """A file could not be read the way this gate needs to read it.

    The single authority for every degraded read. No extractor soft-fails by
    returning an empty set, because an empty set passes the subset check.
    """


def mapped_platforms() -> dict[tuple[str, str], str]:
    """The `(os, arch) -> target triple` mapping. The desktop's authority."""
    path = ROOT / MAPPING_SOURCE
    if not path.is_file():
        raise Refusal(f"{MAPPING_SOURCE} does not exist; the platform mapping "
                      "is missing, so no published platform can be checked "
                      "against it")

    block = MAPPING_BLOCK.search(path.read_text(encoding="utf-8"))
    if block is None:
        raise Refusal(f"{MAPPING_SOURCE} has no readable `const "
                      "SERVER_TARGET_TRIPLES` declaration; it was renamed or "
                      "reformatted, and the mapping cannot be read")

    entries = MAPPING_ENTRY.findall(block.group(1))
    mapping = {(os_name, arch): triple for os_name, arch, triple in entries}
    if len(mapping) != MAPPED_PLATFORM_COUNT:
        raise Refusal(
            f"{MAPPING_SOURCE}: SERVER_TARGET_TRIPLES reads as "
            f"{len(mapping)} platform(s), expected {MAPPED_PLATFORM_COUNT}: "
            f"{sorted(mapping)}. Either the mapping changed -- check it against "
            f"{SHELL_RELEASE}'s {BUILD_JOB} matrix and update the expected "
            "count -- or its entries no longer match the shape this gate reads")
    return mapping


def published_platforms() -> set[tuple[str, str]]:
    """The `(os, arch)` pairs `build-shell` publishes a desktop for."""
    path = ROOT / SHELL_RELEASE
    if not path.is_file():
        raise Refusal(f"{SHELL_RELEASE} does not exist; the published platform "
                      "set cannot be read")
    try:
        workflow = yaml.safe_load(path.read_text(encoding="utf-8"))
    except yaml.YAMLError as exc:
        raise Refusal(f"{SHELL_RELEASE} is not parseable YAML: {exc}") from exc

    job = ((workflow or {}).get("jobs") or {}).get(BUILD_JOB)
    if not isinstance(job, dict):
        raise Refusal(f"{SHELL_RELEASE}: job '{BUILD_JOB}' is absent; the job "
                      "that publishes desktop packages was renamed, and this "
                      "gate no longer knows which platforms ship")

    include = (job.get("strategy") or {}).get("matrix", {})
    include = include.get("include") if isinstance(include, dict) else None
    if not isinstance(include, list):
        raise Refusal(f"{SHELL_RELEASE}: {BUILD_JOB} declares no "
                      "strategy.matrix.include list; the published platform set "
                      "cannot be read from this shape")

    platforms: set[tuple[str, str]] = set()
    for entry in include:
        if not isinstance(entry, dict) or not {"os", "arch"} <= entry.keys():
            raise Refusal(f"{SHELL_RELEASE}: {BUILD_JOB} matrix has an entry "
                          f"without both `os` and `arch`: {entry!r}. This gate "
                          "identifies a platform by that pair")
        platforms.add((str(entry["os"]), str(entry["arch"])))

    if len(platforms) != PUBLISHED_PLATFORM_COUNT:
        raise Refusal(
            f"{SHELL_RELEASE}: {BUILD_JOB} publishes {len(platforms)} "
            f"platform(s), expected {PUBLISHED_PLATFORM_COUNT}: "
            f"{sorted(platforms)}. If the matrix gained or lost a platform, "
            f"check {MAPPING_SOURCE}'s SERVER_TARGET_TRIPLES covers the new set "
            "and update the expected count with it")
    return platforms


def main() -> int:
    try:
        mapping = mapped_platforms()
        published = published_platforms()
    except Refusal as exc:
        print(f"REFUSED: {exc}", file=sys.stderr)
        return 2

    unmapped = sorted(published - mapping.keys())
    if unmapped:
        print(f"{SHELL_RELEASE}'s {BUILD_JOB} publishes {len(unmapped)} "
              f"platform(s) that {MAPPING_SOURCE} cannot map to an engine "
              "target:", file=sys.stderr)
        for os_name, arch in unmapped:
            print(f"  {os_name}-{arch}", file=sys.stderr)
        print("A desktop published for a platform with no SERVER_TARGET_TRIPLES "
              "entry cannot download an engine. Add the entry, or stop "
              "publishing the platform.", file=sys.stderr)
        return 1

    print(f"shell platform mapping OK: {len(published)} published platform(s) "
          f"({', '.join(f'{o}-{a}' for o, a in sorted(published))}) all mapped "
          f"by SERVER_TARGET_TRIPLES ({len(mapping)} entr(y/ies))")
    return 0


if __name__ == "__main__":
    sys.exit(main())
