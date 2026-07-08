#!/usr/bin/env python3
"""Generate the curated card-data fixture for the engine integration tests.

The engine integration suite runs under `cargo nextest`, which executes every
test in its own process. Each process otherwise deserializes the full ~90 MB
`client/public/card-data.json`, costing tens of seconds *per test* in a debug
build. The suite only references a few dozen distinct cards, so this script
extracts just those (plus any faces sharing their `scryfall_oracle_id`, so
multi-face cards keep their back faces) into a small committed fixture that
`tests::integration::support::shared_card_db` loads instead.

Scans the integration tests under `crates/engine/tests`, source-side test
modules under `crates/engine/src`, and source files that load the same fixture
through `crate::test_support::shared_card_db`.

Re-run after adding a test that references a new card:

    python3 scripts/gen-test-fixture.py

The fixture is a strict subset of the export (same key/value shape), so it
loads through the identical `CardDatabase::from_export` path.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
EXPORT_PATH = REPO_ROOT / "client/public/card-data.json"
TESTS_DIR = REPO_ROOT / "crates/engine/tests"
SRC_DIR = REPO_ROOT / "crates/engine/src"
FIXTURE_PATH = REPO_ROOT / "crates/engine/tests/fixtures/integration_cards.json"

# A few non-test-named source files contain test-only card references or corpus
# rows consumed by tests that load the curated fixture.
ALWAYS_SCAN_SRC_FILES = [
    REPO_ROOT / "crates/engine/src/analysis/corpus_tests.rs",
    REPO_ROOT / "crates/engine/src/analysis/corpus.rs",
    REPO_ROOT / "crates/engine/src/database/synthesis.rs",
    REPO_ROOT / "crates/engine/src/game/engine.rs",
    REPO_ROOT / "crates/engine/src/game/meld_tests.rs",
]

# Double-quoted Rust string literal contents (handles \" escapes).
STRING_LITERAL = re.compile(r'"((?:[^"\\]|\\.)*)"')


def src_fixture_files() -> list[Path]:
    """Source files whose test card-name literals should be fixture-backed."""
    files = {path for path in ALWAYS_SCAN_SRC_FILES if path.exists()}
    files.update(SRC_DIR.rglob("*tests.rs"))

    for rs in SRC_DIR.rglob("*.rs"):
        text = rs.read_text(encoding="utf-8", errors="ignore")
        if "shared_card_db" in text:
            files.add(rs)

    return sorted(files)


def referenced_card_keys(export: dict[str, object]) -> set[str]:
    """Every quoted string in the test sources that is a real card key.

    Scans line-by-line so one desyncing quote (a `'"'` char literal, an odd
    quote in a doc-comment) can't shift quote-pairing across the rest of the
    file. Card-name literals are single-line, so this loses nothing.
    """
    keys: set[str] = set()
    for rs in [*TESTS_DIR.rglob("*.rs"), *src_fixture_files()]:
        text = rs.read_text(encoding="utf-8", errors="ignore")
        for line in text.splitlines():
            for raw in STRING_LITERAL.findall(line):
                # Undo the common \" / \\ escapes so quoted card names resolve.
                literal = raw.replace('\\"', '"').replace("\\\\", "\\")
                key = literal.lower()
                if key in export:
                    keys.add(key)
    return keys


def main() -> int:
    if not EXPORT_PATH.exists():
        sys.exit(
            f"error: {EXPORT_PATH.relative_to(REPO_ROOT)} not found — run the "
            "card-data pipeline first (Tilt `card-data` resource)."
        )

    export: dict[str, object] = json.loads(EXPORT_PATH.read_text(encoding="utf-8"))

    referenced = referenced_card_keys(export)

    # Group keys by oracle id so a referenced front face pulls in its siblings.
    by_oracle: dict[str, list[str]] = {}
    for key, value in export.items():
        oid = value.get("scryfall_oracle_id") if isinstance(value, dict) else None
        if oid:
            by_oracle.setdefault(oid, []).append(key)

    selected: set[str] = set(referenced)
    for key in referenced:
        value = export[key]
        oid = value.get("scryfall_oracle_id") if isinstance(value, dict) else None
        if oid:
            selected.update(by_oracle.get(oid, ()))

    # `--check`: verify the committed fixture still covers every referenced card,
    # without rewriting it. Exits non-zero (for CI / pre-commit) when stale.
    if "--check" in sys.argv:
        if not FIXTURE_PATH.exists():
            sys.exit("error: fixture missing — run `python3 scripts/gen-test-fixture.py`")
        current = set(json.loads(FIXTURE_PATH.read_text(encoding="utf-8")))
        missing = selected - current
        if missing:
            listed = "\n  ".join(sorted(missing))
            sys.exit(
                f"error: fixture is stale — {len(missing)} card(s) not covered:\n  "
                f"{listed}\nregenerate with `python3 scripts/gen-test-fixture.py`"
            )
        print(f"ok: fixture covers all {len(selected)} referenced cards")
        return 0

    fixture = {key: export[key] for key in sorted(selected)}
    FIXTURE_PATH.parent.mkdir(parents=True, exist_ok=True)
    # Compact separators keep the committed fixture small.
    serialized = json.dumps(fixture, separators=(",", ":"), ensure_ascii=False)
    FIXTURE_PATH.write_text(serialized + "\n", encoding="utf-8")

    siblings = len(selected) - len(referenced)
    print(
        f"wrote {len(selected)} cards "
        f"({len(referenced)} referenced + {siblings} sibling faces) to "
        f"{FIXTURE_PATH.relative_to(REPO_ROOT)} "
        f"({len(serialized) / 1024:.0f} KB)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
