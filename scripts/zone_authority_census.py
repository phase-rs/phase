#!/usr/bin/env python3
"""Full-tree census of raw zone mutation in the engine.

Every gameplay zone change must go through the replacement-consulting pipeline
(`zone_pipeline::move_object` -> `ApprovedZoneChange` -> delivery). Code that
calls the raw movers in `game/zones.rs`, pokes the `im::Vector` zone containers
directly, or assigns `GameObject::zone` bypasses replacement consultation,
`ZoneChanged` events, triggers, and draw bookkeeping.

This census is the ratchet for that migration (Plan 03). It classifies every
production hit by (file, enclosing fn, pattern family) and compares the result
against `scripts/zone-authority-baseline.txt`:

  * a hit that is NOT in the baseline fails    -> new bypass, route it properly
  * a baseline row whose count DROPPED fails   -> stale baseline, tighten it

so the allowlist can only shrink. When the baseline reaches zero rows the
migration is complete and the gate is zero-tolerance by construction.

Known gap (deliberate): a `&mut <expr>.<zone>` borrow handed to a function that
mutates it is invisible to the container pattern below. Today every such site is
`im_ext::shuffle_vector(&mut player.library, rng)` -- a membership-preserving
library shuffle (CR 701.19), which is not a zone change and would be a permanent
exemption anyway. Adding a fourth pattern family for it would freeze five rows
that never migrate. Revisit if a borrow is ever passed to something that adds or
removes members.

Usage:
    scripts/zone_authority_census.py --check      # gate (used by CI)
    scripts/zone_authority_census.py --list       # report every classified hit
    scripts/zone_authority_census.py --write      # regenerate the baseline
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
BASELINE = REPO_ROOT / "scripts" / "zone-authority-baseline.txt"

SCOPES = ("crates/engine/src", "crates/engine-wasm/src")

# The authority modules themselves: raw delivery is their implementation.
AUTHORITY_FILES = {"zones.rs", "zone_pipeline.rs"}

# Test-support placement helpers. These construct pre-game state and are
# expected to bypass the pipeline loudly; Plan 03 step 5 gives them a named
# `test-support` API. Outlined test modules (`*_tests.rs`, `tests.rs`) carry no
# production dispatch and lose the inline `#[cfg(test)]` marker a line scan
# keys on, so they are excluded by name (same convention as
# check-parser-combinators.sh).
TEST_SUPPORT_FILES = {"scenario.rs", "scenario_db.rs", "testing.rs"}

ALLOW_ANNOTATION = "allow-raw-zone"

# (A) The five raw movers `game/zones.rs` exports.
MOVERS = re.compile(
    r"\b(?:zones::)?"
    r"(move_to_zone|move_to_library_position|move_to_library_at_index"
    r"|remove_from_zone|add_to_zone)\s*\("
)

# (B) Direct mutation of a zone container. Hand-rolling the container write is
# the same bypass as calling a raw mover -- privacy on the movers alone does not
# close it.
CONTAINERS = re.compile(
    r"\.\s*(library|hand|graveyard|exile|battlefield|command_zone)\s*\.\s*"
    r"(push_back|push_front|push|insert|remove|retain|pop_back|pop_front|pop"
    r"|clear|truncate|split_off)\s*\("
)

# (C) Direct `GameObject::zone` assignment -- relocating an object without
# moving it. `==` is a comparison, not an assignment.
ZONE_ASSIGN = re.compile(r"\.zone\s*=\s*[^=]")

FAMILIES = (("mover", MOVERS), ("container", CONTAINERS), ("zone-assign", ZONE_ASSIGN))

FN_DECL = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:const\s+)?(?:unsafe\s+)?fn\s+(\w+)")
INLINE_TEST_MOD = re.compile(r"^\s*(?:pub\s+)?mod\s+\w+\s*\{")

# `#[cfg(test)]`, but also compound predicates like
# `#[cfg(all(test, target_arch = "wasm32"))]` (engine-wasm gates its tests that
# way). `test` must appear as a bare token: `feature = "test-support"` is a
# production cfg, and `not(test)` is production-only code.
CFG_ATTR = re.compile(r"^\s*#\[cfg\((?P<pred>.*)\)\]\s*$")
BARE_TEST = re.compile(r'(?<![\w"-])test(?![\w"-])')


def is_cfg_test_attr(line: str) -> bool:
    m = CFG_ATTR.match(line)
    if not m:
        return False
    pred = m.group("pred")
    return bool(BARE_TEST.search(pred)) and "not(" not in pred


def strip_comment(line: str) -> str:
    """Return the code part of a line (naive but sufficient: no `//` in the
    zone patterns, and Rust string literals containing `//` do not appear in
    the mover/container call sites)."""
    idx = line.find("//")
    return line if idx == -1 else line[:idx]


def census_file(path: Path) -> list[tuple[str, str, str]]:
    """Classify every non-test, non-annotated hit in one file.

    Returns (rel_path, enclosing_fn, family) triples -- one per hit, so callers
    can count multiple distinct branches inside the same function.
    """
    rel = str(path.relative_to(REPO_ROOT))
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()

    hits: list[tuple[str, str, str]] = []
    current_fn = "<module>"
    skip_until_depth: int | None = None
    depth = 0
    pending_cfg_test = False

    for i, raw in enumerate(lines):
        code = strip_comment(raw)

        # Track an inline `#[cfg(test)] mod foo { .. }` body and skip it whole.
        # A naive "first #[cfg(test)] wins" is wrong: engine.rs has 10 and
        # synthesis.rs has 75, nearly all `#[cfg(test)] mod foo;` *declarations*
        # of outlined files, which are excluded by name instead.
        if skip_until_depth is None:
            if is_cfg_test_attr(raw):
                pending_cfg_test = True
            elif pending_cfg_test:
                if INLINE_TEST_MOD.match(code):
                    skip_until_depth = depth
                    depth += code.count("{") - code.count("}")
                    continue
                if code.strip():
                    pending_cfg_test = False

        opened = code.count("{")
        closed = code.count("}")

        if skip_until_depth is not None:
            depth += opened - closed
            if depth <= skip_until_depth:
                skip_until_depth = None
            continue

        m = FN_DECL.match(code)
        if m:
            current_fn = m.group(1)

        depth += opened - closed

        # Explicitly classified non-event operation.
        if ALLOW_ANNOTATION in raw:
            continue
        if i > 0 and ALLOW_ANNOTATION in lines[i - 1]:
            continue

        for family, pattern in FAMILIES:
            if pattern.search(code):
                hits.append((rel, current_fn, family))

    return hits


def collect() -> dict[tuple[str, str, str], int]:
    counts: dict[tuple[str, str, str], int] = {}
    for scope in SCOPES:
        for path in sorted((REPO_ROOT / scope).rglob("*.rs")):
            name = path.name
            if name in AUTHORITY_FILES or name in TEST_SUPPORT_FILES:
                continue
            if name == "tests.rs" or name.endswith("_tests.rs"):
                continue
            for key in census_file(path):
                counts[key] = counts.get(key, 0) + 1
    return counts


HEADER = """\
# Frozen census of pre-existing raw zone mutation (Plan 03 / CR 400.7).
#
# Generated by scripts/zone_authority_census.py --write. Do not hand-edit.
# Columns: file <TAB> enclosing fn <TAB> pattern family <TAB> count.
# Keyed on the enclosing function, not the line, so it survives line drift.
#
# This is MIGRATION DEBT, and it is a ratchet: rows may only shrink. Each row
# is a site that still mutates a zone without going through zone_pipeline. As
# the Plan 03 tranches migrate them onto ZoneMoveRequest, delete the rows
# (scripts/zone_authority_census.py --write). When this file is empty the gate
# is zero-tolerance by construction.
#
# A site that is genuinely NOT a replaceable zone event (CR 733 rollback,
# component absorption, in-library reorder, cease-to-exist, test setup) does
# not belong here -- it is a permanent, named exemption and is annotated at the
# call site instead:
#
#     // allow-raw-zone: <one-line reason>
#
"""


def render(counts: dict[tuple[str, str, str], int]) -> str:
    rows = [f"{f}\t{fn}\t{fam}\t{n}" for (f, fn, fam), n in sorted(counts.items())]
    return HEADER + "\n".join(rows) + ("\n" if rows else "")


def load_baseline() -> dict[tuple[str, str, str], int]:
    if not BASELINE.exists():
        return {}
    out: dict[tuple[str, str, str], int] = {}
    for line in BASELINE.read_text(encoding="utf-8").splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        f, fn, fam, n = line.split("\t")
        out[(f, fn, fam)] = int(n)
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--check", action="store_true", help="gate against the baseline")
    g.add_argument("--list", action="store_true", help="print every classified hit")
    g.add_argument("--write", action="store_true", help="regenerate the baseline")
    args = ap.parse_args()

    counts = collect()
    total = sum(counts.values())

    if args.list:
        sys.stdout.write(render(counts))
        print(f"\n{total} classified production hits in {len(counts)} (file, fn, family) rows", file=sys.stderr)
        return 0

    if args.write:
        BASELINE.write_text(render(counts), encoding="utf-8")
        print(f"wrote {BASELINE.relative_to(REPO_ROOT)}: {total} hits / {len(counts)} rows")
        return 0

    baseline = load_baseline()
    added = {k: n for k, n in counts.items() if k not in baseline}
    grown = {k: (baseline[k], n) for k, n in counts.items() if k in baseline and n > baseline[k]}
    shrunk = {k: (baseline[k], counts.get(k, 0)) for k in baseline if counts.get(k, 0) < baseline[k]}

    if added or grown:
        print("ERROR: new raw zone mutation bypasses the zone pipeline.\n", file=sys.stderr)
        for (f, fn, fam), n in sorted(added.items()):
            print(f"  NEW      {f}::{fn} ({fam} x{n})", file=sys.stderr)
        for (f, fn, fam), (was, now) in sorted(grown.items()):
            print(f"  GREW     {f}::{fn} ({fam}) {was} -> {now}", file=sys.stderr)
        print(
            "\nA gameplay zone change must be proposed through zone_pipeline so that\n"
            "replacement effects, ZoneChanged events, triggers, and draw bookkeeping\n"
            "all get their opportunity. Build a ZoneMoveRequest instead.\n\n"
            "If the operation is genuinely not a replaceable zone event (rollback,\n"
            "component absorption, in-library reorder, cease-to-exist, test setup),\n"
            "annotate it with:\n\n"
            "    // allow-raw-zone: <one-line reason>\n",
            file=sys.stderr,
        )
        return 1

    if shrunk:
        print("ERROR: the zone-authority baseline is stale -- migration progressed.\n", file=sys.stderr)
        for (f, fn, fam), (was, now) in sorted(shrunk.items()):
            print(f"  MIGRATED {f}::{fn} ({fam}) {was} -> {now}", file=sys.stderr)
        print(
            "\nThe baseline is a ratchet: it may only shrink. Tighten it with\n"
            "    scripts/zone_authority_census.py --write\n",
            file=sys.stderr,
        )
        return 1

    print(f"Gate B PASS: {total} raw zone hits, all classified ({len(counts)} rows, baseline frozen)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
