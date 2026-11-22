#!/usr/bin/env python3
"""Re-apply a PR's edits of the old `integration_cards.json.gz` onto the shard layout.

A branch cut before the fixture was sharded changed card entries inside the single
gzip. After merging main its binary conflict has no meaning any more: the file is
gone and the entries live in `crates/engine/tests/fixtures/integration_cards/`.
This script carries the branch's entry edits over.

    python3 scripts/reapply-fixture-entries.py <pr-ref> --base REV [--write]

`--base` is required: it is the main commit the branch was cut from (the last one
that still had the gz), for example `git merge-base <pr-ref> <sharding-commit>^`.
It cannot be guessed from HEAD: once main is merged into the branch, the merge-base
of the branch tip and HEAD is the branch tip itself and every edit would look
like "no change".

For each key, the branch's change is the difference between its gz and the gz at
`--base`, both read with `git show`. The
change is applied to the shards in the working tree when the shards still hold the
base's record for that key (main did not touch it), is a no-op when they already
hold the branch's record, and is a CONFLICT otherwise (main changed the same card
differently, or removed it). Records are compared as canonical shard lines, not as
parsed JSON. Without `--write` this only reports. With `--write` nothing is written
if any conflict exists.

Typical use, on the branch after `git merge main` took main's side of the conflict:

    python3 scripts/reapply-fixture-entries.py ORIG_HEAD --base <fork-point> --write
    git rm -q --ignore-unmatch crates/engine/tests/fixtures/integration_cards.json.gz

where `<pr-ref>` is a ref whose tree still has the old gz (the branch tip before
the merge, e.g. `ORIG_HEAD` or `origin/<branch>`) and `<fork-point>` is the main
commit the branch was cut from.
"""
from __future__ import annotations

import argparse
import gzip
import json
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
import fixture_shards  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parent.parent
OLD_GZ = "crates/engine/tests/fixtures/integration_cards.json.gz"
FIXTURE_DIR = REPO_ROOT / "crates/engine/tests/fixtures/integration_cards"


def git(*args: str) -> bytes:
    return subprocess.run(["git", *args], cwd=REPO_ROOT, check=True, stdout=subprocess.PIPE).stdout


def old_entries(rev: str) -> dict[str, object]:
    try:
        blob = git("show", f"{rev}:{OLD_GZ}")
    except subprocess.CalledProcessError:
        sys.exit(f"error: {rev} has no {OLD_GZ} (already sharded, or not a fixture-editing ref)")
    entries = json.loads(gzip.decompress(blob).decode("utf-8"))
    if not isinstance(entries, dict):
        sys.exit(f"error: {rev}:{OLD_GZ} is not a JSON object")
    return entries


def line(key: str, entries: dict[str, object]) -> str | None:
    return fixture_shards.render_record(key, entries[key]) if key in entries else None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("pr_ref", help="ref whose tree still has the old integration_cards.json.gz")
    parser.add_argument("--base", required=True, help="main commit the branch was cut from (the gz at this commit is the baseline)")
    parser.add_argument("--write", action="store_true", help="write the shards (default: report only)")
    args = parser.parse_args()

    base_rev = args.base
    if not FIXTURE_DIR.is_dir():
        sys.exit(f"error: {FIXTURE_DIR.relative_to(REPO_ROOT)}/ not found: run this on a checkout that has the shards (merge main first)")
    base = old_entries(base_rev)
    theirs = old_entries(args.pr_ref)
    current = fixture_shards.parse_shards(fixture_shards.read_shards(FIXTURE_DIR))

    result = dict(current)
    applied: list[str] = []
    noops: list[str] = []
    conflicts: list[str] = []
    for key in sorted(set(base) | set(theirs)):
        was, want, now = line(key, base), line(key, theirs), line(key, current)
        if was == want:
            continue  # the branch did not touch this key
        if now == want:
            noops.append(key)  # main already holds the branch's record (or both removed it)
        elif now == was:
            applied.append(key)  # main left the base's record alone
            if want is None:
                del result[key]
            else:
                result[key] = theirs[key]
        else:
            conflicts.append(key)

    print(f"base {base_rev[:10]}: {len(applied)} to apply, {len(noops)} already present, {len(conflicts)} conflict(s)")
    for key in conflicts:
        print(f"  CONFLICT {key!r}: main's record differs from both the base and the branch")
    if conflicts:
        return 1
    if not args.write:
        for key in applied:
            print(f"  would {'remove' if key not in theirs else 'set'} {key!r}")
        return 0

    fixture_shards.write_shards(FIXTURE_DIR, fixture_shards.render_shards(dict(sorted(result.items()))))
    print(f"wrote {len(applied)} change(s) to {FIXTURE_DIR.relative_to(REPO_ROOT)}/")
    if (REPO_ROOT / OLD_GZ).exists():
        print(f"note: {OLD_GZ} is still in the tree; remove it with `git rm {OLD_GZ}`")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
