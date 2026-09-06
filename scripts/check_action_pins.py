#!/usr/bin/env python3
"""Every external action reachable from a privileged workflow must be pinned.

Pinning only the workflow file being edited is not enough: a reusable workflow
or composite action it calls runs with the caller's secrets, so a mutable ref
one call away is the same exposure with less visibility. This walks those call
chains.

The walk parses YAML rather than matching `uses:` textually. A pattern misses
every form the parser accepts but the pattern does not -- `"uses":`, `'uses':`,
a flow mapping `{uses: ...}` -- and each of those is a way to put a mutable
action into a privileged workflow while the gate reports success.

PIN_CHECK_ROOT points the walk at a tree other than this checkout, so a trusted
copy of this script can audit an untrusted one's workflow files without running
anything from it.

Usage: check_action_pins.py [entry-workflow ...]
"""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path

try:
    import yaml
except ModuleNotFoundError:  # pragma: no cover - exercised by the runner, not tests
    # Falling back to a pattern would silently weaken the gate, which is worse
    # than not running: the result would look identical to a clean pass.
    sys.exit("check_action_pins: PyYAML is required and was not found; refusing "
             "to check with a weaker method")

DEFAULT_ENTRIES = (".github/workflows/release.yml", ".github/workflows/deploy.yml")
SHA = re.compile(r"@[0-9a-f]{40}$")
# A container action is pinned by image digest, not by a commit SHA. A tag such
# as docker://alpine:3.20 moves, so it is exactly what this gate exists to stop.
DOCKER_DIGEST = re.compile(r"^docker://[^@\s]+@sha256:[0-9a-f]{64}$")
REMOTE_REUSABLE = re.compile(r"/\.github/workflows/[^@]+\.ya?ml@")


def contained(target: Path, root: Path) -> bool:
    """Whether `target` stays inside `root` once symlinks are followed."""
    try:
        return target.resolve().is_relative_to(root)
    except OSError:
        return False


def uses_refs(node: object) -> list[str]:
    """Every `uses:` value anywhere in a parsed workflow or action."""
    found: list[str] = []
    if isinstance(node, dict):
        for key, value in node.items():
            if key == "uses" and isinstance(value, str):
                found.append(value)
            else:
                found.extend(uses_refs(value))
    elif isinstance(node, list):
        for item in node:
            found.extend(uses_refs(item))
    return found


def main(argv: list[str]) -> int:
    root = Path(os.environ.get("PIN_CHECK_ROOT") or Path(__file__).resolve().parent.parent).resolve()
    entries = argv[1:] or list(DEFAULT_ENTRIES)

    queue = list(entries)
    seen: set[str] = set()
    unpinned: list[str] = []
    pinned = 0
    walked = 0

    while queue:
        rel = queue.pop(0)
        if rel in seen:
            continue
        seen.add(rel)

        path = root / rel
        if not path.is_file():
            print(f"ERROR: {rel} is referenced but does not exist", file=sys.stderr)
            return 1
        try:
            document = yaml.safe_load(path.read_text(encoding="utf-8"))
        except yaml.YAMLError as exc:
            print(f"ERROR: {rel} is not parseable YAML: {exc}", file=sys.stderr)
            return 1
        walked += 1

        for ref in uses_refs(document):
            if ref.startswith("./"):
                target = root / ref[2:]
                if not contained(target, root):
                    # A tree that reads its own actions out of a directory it
                    # does not contain is not being audited: the audit walks the
                    # workspace above it, which holds the trusted checkout.
                    unpinned.append(
                        f"{rel}: {ref} (local reference resolves outside the "
                        "checked tree)"
                    )
                    continue
                if target.is_dir():
                    manifest = next(
                        (m for m in ("action.yml", "action.yaml") if (target / m).is_file()),
                        None,
                    )
                    if manifest is None:
                        # Skipping leaves the gate green while the runner fails
                        # to resolve this action.
                        unpinned.append(f"{rel}: {ref} (no action.yml or action.yaml)")
                    else:
                        queue.append(f"{ref[2:]}/{manifest}")
                else:
                    queue.append(ref[2:])
            elif ref.startswith("docker://"):
                if DOCKER_DIGEST.match(ref):
                    pinned += 1
                else:
                    unpinned.append(
                        f"{rel}: {ref} (container action: pin it by digest, "
                        "docker://image@sha256:<64 hex>)"
                    )
            elif REMOTE_REUSABLE.search(ref):
                unpinned.append(
                    f"{rel}: {ref} (remote reusable workflow: its own actions are "
                    "unverifiable here)"
                )
            elif SHA.search(ref):
                pinned += 1
            else:
                unpinned.append(f"{rel}: {ref}")

    if unpinned:
        print(f"Unverified actions reachable from: {' '.join(entries)}", file=sys.stderr)
        for item in unpinned:
            print(f"  {item}", file=sys.stderr)
        print("Pin each external action to a 40-hex commit SHA with a trailing "
              "'# vX.Y.Z' comment.", file=sys.stderr)
        return 1

    print(f"action pins OK: {walked} file(s) walked from {' '.join(entries)}, "
          f"{pinned} external ref(s), all SHA-pinned")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
