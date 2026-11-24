#!/usr/bin/env python3
"""Generate the curated card-data fixture for the engine integration tests.

The engine integration suite runs under `cargo nextest`, which executes every
test in its own process. Each process otherwise deserializes the full ~90 MB
`client/public/card-data.json`, costing tens of seconds *per test* in a debug
build. The suite only references a few dozen distinct cards, so this script
extracts just those (plus any faces sharing their `scryfall_oracle_id`, so
multi-face cards keep their back faces) into a small committed fixture, a directory of
JSON Lines shards by first letter (format: `scripts/lib/fixture_shards.py`), that
`tests::integration::support::shared_card_db` loads instead.

Scans the integration tests under `crates/engine/tests`, source-side test
modules under `crates/engine/src` and `crates/phase-parser/src`, source files
that load the same fixture through `crate::test_support::shared_card_db`, and
Phase AI tests that load the fixture directly.

Re-run after adding a test that references a new card:

    python3 scripts/gen-test-fixture.py

The fixture is a strict subset of the export (same key/value shape). The Rust loader
(`database::integration_fixture`) joins the shards back into one JSON object, so it
loads through the identical `CardDatabase::from_export` path.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
import fixture_shards  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parent.parent
EXPORT_PATH = REPO_ROOT / "client/public/card-data.json"
TESTS_DIR = REPO_ROOT / "crates/engine/tests"
SRC_DIR = REPO_ROOT / "crates/engine/src"
PARSER_SRC_DIR = REPO_ROOT / "crates/phase-parser/src"
FIXTURE_DIR = REPO_ROOT / "crates/engine/tests/fixtures/integration_cards"
CORPUS_FIXTURE_DIR = REPO_ROOT / "crates/engine/tests/fixtures/bracket_corpus"

# This fixture must exercise Witherbloom Apprentice and Sakashima of a
# Thousand Faces through the raw-MTGJSON parser, not a hand-maintained
# card-data export entry. Keep the set narrow: every other referenced card is
# selected from the production export below.
PARSER_BACKED_FIXTURE_CARDS = {"witherbloom apprentice", "sakashima of a thousand faces"}

# A few non-test-named source files contain test-only card references or corpus
# rows consumed by tests that load the curated fixture.
ALWAYS_SCAN_SRC_FILES = [
    REPO_ROOT / "crates/engine/src/analysis/corpus_tests.rs",
    REPO_ROOT / "crates/engine/src/analysis/corpus.rs",
    REPO_ROOT / "crates/engine/src/database/synthesis.rs",
    REPO_ROOT / "crates/engine/src/game/engine.rs",
    REPO_ROOT / "crates/engine/src/game/meld_tests.rs",
]

# Phase AI owns a small number of real-card scenarios that load the engine
# fixture directly, rather than through the engine test-support wrapper.
FIXTURE_CONSUMER_TEST_FILES = [REPO_ROOT / "crates/phase-ai/src/search.rs"]

# Double-quoted Rust string literal contents (handles \" escapes).
STRING_LITERAL = re.compile(r'"((?:[^"\\]|\\.)*)"')


def src_fixture_files() -> list[Path]:
    """Source files whose test card-name literals should be fixture-backed."""
    files = {path for path in ALWAYS_SCAN_SRC_FILES if path.exists()}
    for root in (SRC_DIR, PARSER_SRC_DIR):
        files.update(root.rglob("*tests.rs"))

        for rs in root.rglob("*.rs"):
            text = rs.read_text(encoding="utf-8", errors="ignore")
            if "shared_card_db" in text:
                files.add(rs)
    # The Horde challenge-deck lists are corpus rows consumed by
    # horde_deck_card_names.rs through shared_card_db(); without them a
    # regeneration would silently drop every Horde deck card from the fixture.
    files.update((SRC_DIR / "game" / "decks").rglob("*.rs"))

    return sorted(files)


def referenced_card_keys(export: dict[str, object]) -> set[str]:
    """Every quoted string in the test sources that is a real card key.

    Scans line-by-line so one desyncing quote (a `'"'` char literal, an odd
    quote in a doc-comment) can't shift quote-pairing across the rest of the
    file. Card-name literals are single-line, so this loses nothing.
    """
    keys: set[str] = set()
    for rs in [
        *TESTS_DIR.rglob("*.rs"),
        *src_fixture_files(),
        *FIXTURE_CONSUMER_TEST_FILES,
    ]:
        text = rs.read_text(encoding="utf-8", errors="ignore")
        for line in text.splitlines():
            for raw in STRING_LITERAL.findall(line):
                # Undo the common \" / \\ escapes so quoted card names resolve.
                literal = raw.replace('\\"', '"').replace("\\\\", "\\")
                key = literal.lower()
                if key in export:
                    keys.add(key)
    return keys


def corpus_card_keys(export: dict[str, object]) -> set[str]:
    """Card names declared by the bracket-corpus fixtures.

    These are JSON, not Rust, so `referenced_card_keys`' string-literal scan
    cannot see them. A corpus card missing from the fixture resolves to
    BracketSignals::default() (card_db.rs:522) -- indistinguishable from a clean
    card -- so the corpus test would pass while measuring nothing.
    """
    keys: set[str] = set()
    if not CORPUS_FIXTURE_DIR.is_dir():
        return keys
    for path in sorted(CORPUS_FIXTURE_DIR.glob("*.json")):
        if path.name == "expectations.json":
            continue
        data = json.loads(path.read_text(encoding="utf-8"))
        deck = data.get("decklist", {})
        for section in ("commander", "main_deck"):
            for name in deck.get(section, []):
                key = name.lower()
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

    referenced = referenced_card_keys(export) | corpus_card_keys(export)
    export_referenced = referenced - PARSER_BACKED_FIXTURE_CARDS

    # Group keys by oracle id so a referenced front face pulls in its siblings.
    by_oracle: dict[str, list[str]] = {}
    for key, value in export.items():
        oid = value.get("scryfall_oracle_id") if isinstance(value, dict) else None
        if oid:
            by_oracle.setdefault(oid, []).append(key)

    selected: set[str] = set(export_referenced)
    for key in export_referenced:
        value = export[key]
        oid = value.get("scryfall_oracle_id") if isinstance(value, dict) else None
        if oid:
            selected.update(by_oracle.get(oid, ()))

    fixture = {key: export[key] for key in sorted(selected)}
    shards = fixture_shards.render_shards(fixture)

    # `--check`: parse the committed shards and validate the semantic subset, then
    # prove the shard set is exactly what the writer produces (same files, same bytes).
    if "--check" in sys.argv:
        if not FIXTURE_DIR.is_dir():
            sys.exit("error: fixture missing — run `python3 scripts/gen-test-fixture.py`")
        current_shards = fixture_shards.read_shards(FIXTURE_DIR)
        try:
            current = fixture_shards.parse_shards(current_shards)
        except (ValueError, KeyError, TypeError) as err:
            sys.exit(f"error: fixture shards do not parse as JSON Lines records: {err}")
        current_keys = set(current)
        missing = selected - current_keys
        if missing:
            listed = "\n  ".join(sorted(missing))
            sys.exit(
                f"error: fixture is stale — {len(missing)} card(s) not covered:\n  "
                f"{listed}\nregenerate with `python3 scripts/gen-test-fixture.py`"
            )
        parser_backed = current_keys & PARSER_BACKED_FIXTURE_CARDS
        if parser_backed:
            listed = "\n  ".join(sorted(parser_backed))
            sys.exit(
                "error: parser-backed cards leaked into the export fixture:\n  "
                f"{listed}\nregenerate with `python3 scripts/gen-test-fixture.py`"
            )
        if current != fixture:
            sys.exit(
                "error: fixture semantic values are stale — regenerate with "
                "`python3 scripts/gen-test-fixture.py`"
            )
        extra = sorted(set(current_shards) - set(shards))
        absent = sorted(set(shards) - set(current_shards))
        differing = sorted(
            name for name in set(shards) & set(current_shards) if shards[name] != current_shards[name]
        )
        if extra or absent or differing:
            sys.exit(
                "error: fixture shards are not the writer's output "
                f"(unexpected files: {extra}, missing files: {absent}, differing bytes: {differing}) — "
                "regenerate with `python3 scripts/gen-test-fixture.py`"
            )
        print(
            f"ok: fixture covers all {len(selected)} referenced cards in "
            f"{len(shards)} canonical shards"
        )
        return 0

    fixture_shards.write_shards(FIXTURE_DIR, shards)
    size = sum(len(data) for data in shards.values())

    siblings = len(selected) - len(export_referenced)
    print(
        f"wrote {len(selected)} cards "
        f"({len(export_referenced)} export-referenced + {siblings} sibling faces) to "
        f"{FIXTURE_DIR.relative_to(REPO_ROOT)}/ "
        f"({len(shards)} shards, {size / 1024:.0f} KB)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
