#!/usr/bin/env python3
"""One-time migration: `integration_cards.json.gz` -> `integration_cards/*.jsonl`.

    git show <rev>:crates/engine/tests/fixtures/integration_cards.json.gz > old.gz
    python3 scripts/migrate-fixture-to-shards.py --gz old.gz [--out DIR]

Deterministic: the shard bytes depend only on the gz's decoded entries. After
writing, the script reads the shards back and asserts they decode to exactly
the gz's entries. The gz no longer exists in the tree once the shards land (it
is read from history with `git show`).

Re-applying a PR that edited the old gz onto the shard layout: see
`scripts/reapply-fixture-entries.py`.
"""
from __future__ import annotations

import argparse
import gzip
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
import fixture_shards  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parent.parent
FIXTURES = REPO_ROOT / "crates/engine/tests/fixtures"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--gz", type=Path, required=True)
    parser.add_argument("--out", type=Path, default=FIXTURES / "integration_cards")
    args = parser.parse_args()

    fixture = json.loads(gzip.decompress(args.gz.read_bytes()).decode("utf-8"))
    if not isinstance(fixture, dict):
        sys.exit("error: gz must decode to a JSON object")
    shards = fixture_shards.render_shards(fixture)
    fixture_shards.write_shards(args.out, shards)

    back = fixture_shards.parse_shards(fixture_shards.read_shards(args.out))
    if back != dict(sorted(fixture.items())):
        sys.exit("error: shards do not decode to the gz's entries")
    print(f"wrote {len(fixture)} entries to {len(shards)} shards in {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
