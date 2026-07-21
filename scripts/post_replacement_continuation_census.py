#!/usr/bin/env python3
"""Freeze the `PostReplacementContinuation` producer/consumer surface (Plan 03).

`PostReplacementContinuation` carries work to run only after a replacement
effect's delivery settles (CR 616.1g / CR 615.5). Its `Template` arm holds an
arbitrary parsed `AbilityDefinition` AST; its `Resolved` arm holds an arbitrary
`ResolvedAbility` with bound targets. Either can contain `Effect::Draw`, so a
source scan cannot prove that a particular producer cannot reach Draw. Trying
to derive that semantic reachability here would turn a structural gate into an
incomplete second resolver.

The durable state machine (`PostReplacementDrain`, `DrainStatus`, and
`ResidentDrainPolicy`) already shipped in PRs #5686/#5690. Production behavior
is pinned separately by `draw_from_general_post_replacement.rs`: the `Template`
arm through Swans of Bryn Argoll and Nefarious Lich, the `Resolved` arm through
New Way Forward, and the nested-continuation case
`nested_mandatory_post_effect_runs_when_a_dispatching_continuation_draws`.
This census deliberately does none of that behavioral work again. It freezes
only the syntactic producer/consumer surface those tests sit on top of.

Every classified production hit is keyed by `(file, enclosing fn, family)` and
compared against `scripts/post-replacement-continuation-baseline.txt`:

  * a hit that is NOT in the baseline fails    -> a reviewed new surface site
  * a row whose count GREW fails               -> a reviewed new use at that site
  * a baseline row whose count DROPPED fails   -> stale baseline, tighten it

This is a RATCHET: rows may only shrink or stay level. There is no exemption
annotation. A continuation can carry arbitrary work, so no syntactic use is
safe to waive by local inspection; a new use is a design decision that must be
reviewed and frozen with `--write`.

Usage:
    scripts/post_replacement_continuation_census.py --check  # gate (used by CI)
    scripts/post_replacement_continuation_census.py --list   # report every hit
    scripts/post_replacement_continuation_census.py --write  # regenerate baseline
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# "Production engine code" (inline `#[cfg(test)]` mod bodies skipped by brace
# depth, compound cfg predicates, strings/comments stripped, loud failure on
# brace desync) is defined once, by the zone-authority census's scanner. Reused
# rather than copied: two copies would be two definitions of production code,
# free to drift, and their disagreements would be silent in both directions.
from zone_authority_census import (
    CensusError,
    REPO_ROOT,
    TEST_SUPPORT_FILES,
    iter_production_lines,
)

BASELINE = REPO_ROOT / "scripts" / "post-replacement-continuation-baseline.txt"

# The continuation itself is engine state, but engine-wasm is included exactly
# as it is for the zone-authority census: a future adapter-side producer or
# consumer must not become invisible merely because it does not exist today.
SCOPES = ("crates/engine/src", "crates/engine-wasm/src")

# (A) Direct construction of either binding state. The parentheses matter: a
# function-pointer use such as `.map(PostReplacementContinuation::Template)`
# is not one of the direct-construction shapes this structural census freezes.
TEMPLATE = re.compile(r"PostReplacementContinuation::Template\s*\(")
RESOLVED = re.compile(r"PostReplacementContinuation::Resolved\s*\(")

# (B) GameState's installation authority. The leading dot deliberately excludes
# the sole `pub fn install_ready_continuation(` declaration: it has no receiver
# expression, and therefore no leading dot.
INSTALL = re.compile(r"\.install_ready_continuation\s*\(")

# (C) The replacement-pipeline stash authority and the post-delivery dispatcher.
# The lookbehind is exactly three characters because the declarations are
# verified as `fn stash_post_replacement_continuation(` and `pub(crate) fn
# apply_pending_post_replacement_effect(`. At each function name the immediate
# preceding text is therefore `fn `, even when visibility appears earlier. This
# must stay fixed-width: Python `re` rejects variable-width lookbehinds.
STASH = re.compile(r"(?<!fn )stash_post_replacement_continuation\s*\(")
CONSUMER = re.compile(r"(?<!fn )apply_pending_post_replacement_effect\s*\(")

FAMILIES = (
    ("template", TEMPLATE),
    ("resolved", RESOLVED),
    ("install", INSTALL),
    ("stash", STASH),
    ("consumer", CONSUMER),
)


def census_file(path: Path) -> list[tuple[str, str, str]]:
    """Classify every non-test producer or consumer hit in one file.

    Returns `(rel_path, enclosing_fn, family)` triples -- one per matching
    family on a line. Several families may match a single line and each is
    ratcheted independently, matching the zone census's sum-per-line contract.
    Patterns run only on the shared scanner's stripped `code`, never `raw`, so
    a comment or string that names a continuation cannot create a fake site.
    """
    rel = str(path.relative_to(REPO_ROOT))
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    hits: list[tuple[str, str, str]] = []

    for _i, _raw, code, current_fn in iter_production_lines(rel, lines):
        for family, pattern in FAMILIES:
            if pattern.search(code):
                hits.append((rel, current_fn, family))

    return hits


def collect() -> dict[tuple[str, str, str], int]:
    counts: dict[tuple[str, str, str], int] = {}
    for scope in SCOPES:
        for path in sorted((REPO_ROOT / scope).rglob("*.rs")):
            name = path.name
            # Unlike raw zone mutation, this surface has no implementation file
            # that is categorically outside the census. Test-support helpers and
            # outlined test modules follow the shared scanner's sibling-census
            # convention, though: they are not production dispatch sites.
            if name in TEST_SUPPORT_FILES:
                continue
            if name == "tests.rs" or name.endswith("_tests.rs"):
                continue
            for key in census_file(path):
                counts[key] = counts.get(key, 0) + 1
    return counts


HEADER = """\
# Frozen census of the `PostReplacementContinuation` producer/consumer surface
# (Plan 03 / CR 121.2 / CR 616.1g / CR 615.5).
#
# Generated by scripts/post_replacement_continuation_census.py --write. Do not
# hand-edit.
# Columns: file <TAB> enclosing fn <TAB> family <TAB> count.
# Keyed on the enclosing function, not the line, so it survives line drift.
#
# This is a RATCHET: rows may only shrink or stay level. A new row or a row
# whose count grows fails the gate until its syntactic use of the continuation
# surface is reviewed. A row whose count shrinks also fails: migration progressed
# and this frozen baseline must be tightened with --write.
#
# `Template` and `Resolved` hold arbitrary ability ASTs / resolved abilities, so
# either can contain `Effect::Draw`. This is intentionally a structural freeze,
# not an attempted semantic Draw-reachability classifier; behavioral witnesses
# live in tests/integration/draw_from_general_post_replacement.rs.
#
# family=template  direct `PostReplacementContinuation::Template(...)`
# family=resolved  direct `PostReplacementContinuation::Resolved(...)`
# family=install   `.install_ready_continuation(...)`
# family=stash     `stash_post_replacement_continuation(...)` call
# family=consumer  `apply_pending_post_replacement_effect(...)` call
#
"""


def render(counts: dict[tuple[str, str, str], int], header: bool = True) -> str:
    rows = [f"{f}\t{fn}\t{fam}\t{n}" for (f, fn, fam), n in sorted(counts.items())]
    body = "\n".join(rows) + ("\n" if rows else "")
    return HEADER + body if header else body


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

    try:
        counts = collect()
    except CensusError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 1
    total = sum(counts.values())

    if args.list:
        sys.stdout.write(render(counts, header=False))
        print(
            f"\n{total} classified production hits in {len(counts)} "
            "(file, fn, family) rows",
            file=sys.stderr,
        )
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
        print("ERROR: post-replacement continuation surface grew.\n", file=sys.stderr)
        for (f, fn, fam), n in sorted(added.items()):
            print(f"  NEW      {f}::{fn} ({fam} x{n})", file=sys.stderr)
        for (f, fn, fam), (was, now) in sorted(grown.items()):
            print(f"  GREW     {f}::{fn} ({fam}) {was} -> {now}", file=sys.stderr)
        print(
            "\nA continuation can hold arbitrary delayed ability work, including Draw.\n"
            "Review every new construction, installation, stash, or dispatch site, then\n"
            "freeze the reviewed structural surface with:\n"
            "    scripts/post_replacement_continuation_census.py --write\n",
            file=sys.stderr,
        )
        return 1

    if shrunk:
        print(
            "ERROR: the post-replacement-continuation baseline is stale -- "
            "migration progressed.\n",
            file=sys.stderr,
        )
        for (f, fn, fam), (was, now) in sorted(shrunk.items()):
            print(f"  MIGRATED {f}::{fn} ({fam}) {was} -> {now}", file=sys.stderr)
        print(
            "\nThe baseline is a ratchet: it may only shrink. Tighten it with\n"
            "    scripts/post_replacement_continuation_census.py --write\n",
            file=sys.stderr,
        )
        return 1

    print(
        f"Gate D PASS: {total} post-replacement continuation hits, all classified "
        f"({len(counts)} rows, baseline frozen)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
