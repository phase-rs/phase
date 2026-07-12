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

Pattern families (a hit is classified into exactly one):

    mover        the five raw movers game/zones.rs exports
    container    direct membership mutation of an im::Vector zone container
    zone-assign  direct `GameObject::zone = ...`
    borrow       a `&mut <expr>.<zone>` handed to a callee that is not known to
                 preserve membership (a shuffle is; anything else must prove it)
    exempt       any of the above, annotated `// allow-raw-zone: <reason>`

`exempt` is ratcheted like the rest, deliberately. The annotation is the
cheapest possible way to add a raw zone mutation, so it must cost a review
rather than a keystroke -- an exemption no instrument counts is an exemption
nobody revisits. The reason string is mandatory.

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
from typing import NamedTuple

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

ALLOW_ANNOTATION = re.compile(r"allow-raw-zone\s*:\s*(?P<reason>\S.*?)\s*$")
ALLOW_ANNOTATION_BARE = "allow-raw-zone"

ZONES = r"library|hand|graveyard|exile|battlefield|command_zone"

# (A) The five raw movers `game/zones.rs` exports.
MOVERS = re.compile(
    r"\b(?:zones::)?"
    r"(move_to_zone|move_to_library_position|move_to_library_at_index"
    r"|remove_from_zone|add_to_zone)\s*\("
)

# (B) Direct mutation of a zone container. Hand-rolling the container write is
# the same bypass as calling a raw mover -- privacy on the movers alone does not
# close it. Every `im::Vector` method that can change MEMBERSHIP belongs here:
# omitting one does not make the bypass safe, it makes the census blind to it.
CONTAINERS = re.compile(
    rf"\.\s*({ZONES})\s*\.\s*"
    r"(push_back|push_front|push|insert|remove|retain|pop_back|pop_front|pop"
    r"|clear|truncate|split_off|append|extend|set)\s*\("
)

# (C) Direct `GameObject::zone` assignment -- relocating an object without
# moving it. `==` is a comparison, not an assignment.
ZONE_ASSIGN = re.compile(r"\.zone\s*=\s*[^=]")

# (D) A `&mut` borrow of a zone container handed to a callee, which can mutate
# membership out of sight of (B). Only callees that provably preserve membership
# are allowed: a shuffle reorders the library (CR 701.19) and is not a zone
# change. Anything else is a bypass until proven otherwise.
BORROW = re.compile(rf"&mut\s+[\w.\[\]()]+\.\s*({ZONES})\b")
BORROW_CALLEE = re.compile(r"(\w+)\s*\(\s*$")
MEMBERSHIP_PRESERVING_CALLEES = {"shuffle_vector"}

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


# A non-raw literal: `"..."` (with a `b`/`c` prefix, which is part of the token),
# or a char literal. The char alternative earns its place: `'"'` must be consumed
# whole, or the leaked `"` opens a phantom string that swallows the line.
STRING_LIT = re.compile(r'(?:b|c)?"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'')

# A raw-string opener: `r"`, `r#"`, `r##"`, and the byte/C-string forms `br#"` /
# `cr#"`. Rust raw strings do NOT nest and honour NO escapes, so the `#` count is
# the only thing that closes one -- and the only thing we have to carry.
RAW_OPEN = re.compile(r'(?:b|c)?r(#*)"')

IDENT_CHAR = re.compile(r"[A-Za-z0-9_]")


class ScanState(NamedTuple):
    """Lexer state carried BETWEEN lines.

    Both constructs that can span a line boundary need a count, not a flag:
    block comments nest (`/* /* */ */`) and a raw string is closed only by its
    own `#` count. The two are mutually exclusive -- a raw string opened inside a
    block comment is comment text, and a `/*` inside a raw string is data.
    """

    block_depth: int = 0
    raw_hashes: int | None = None  # `#` count of the raw string we are inside


class CensusError(Exception):
    """The scanner lost track of the source structure. Never guess -- a census
    that silently mis-scopes is worse than no census."""


def _at_token_boundary(line: str, i: int) -> bool:
    """True if `i` starts a token. `r`/`b`/`c` mean "literal prefix" only at a
    token boundary; elsewhere they are the tail of an identifier."""
    return i == 0 or not IDENT_CHAR.match(line[i - 1])


def _consume_raw(line: str, i: int, hashes: int) -> tuple[int, bool]:
    """Consume raw-string body from `i`. Returns (next_index, closed_on_this_line).

    The terminator is `"` followed by exactly the opening `#` count, so a bare `"`
    -- or a `"` with too few `#` -- is content. Nothing else terminates a raw
    string: not a backslash, not a newline.
    """
    close = '"' + "#" * hashes
    end = line.find(close, i)
    if end == -1:
        return len(line), False
    return end + len(close), True


def strip_noncode(line: str, state: ScanState) -> tuple[str, ScanState]:
    """Return (code, state_for_the_next_line).

    Strings and comments are removed before ANY brace counting or pattern
    matching. Brace counting in particular must not see a stray `{` inside a
    literal: inside a skipped `#[cfg(test)]` mod that would extend the skip past
    the mod's closing brace and silently swallow the production code that
    follows.

    Raw strings are the reason this is a state machine rather than a regex. Their
    contents are arbitrary bytes -- `//`, `/*`, `{`, `"`, and `#[cfg(test)]` all
    appear inside them as DATA (see any `format!(r#"{{...}}"#)` JSON fixture) --
    and they run across lines. A scanner that mishandles one does not just miss a
    hit: it starts a comment that eats the file, or a skip region that eats the
    production code after it.
    """
    out: list[str] = []
    i = 0
    block_depth = state.block_depth
    raw_hashes = state.raw_hashes

    while i < len(line):
        if raw_hashes is not None:
            i, closed = _consume_raw(line, i, raw_hashes)
            if closed:
                raw_hashes = None
            continue

        if block_depth:
            # Rust block comments nest: `/* a /* b */ still a comment */`. Closing
            # at the first `*/` leaves the comment's tail behind as "code", and a
            # stray brace in that tail desyncs exactly like a raw string does.
            opened_at = line.find("/*", i)
            closed_at = line.find("*/", i)
            if opened_at == -1 and closed_at == -1:
                break
            if opened_at != -1 and (closed_at == -1 or opened_at < closed_at):
                block_depth += 1
                i = opened_at + 2
            else:
                block_depth -= 1
                i = closed_at + 2
            continue

        if line.startswith("//", i):
            break
        if line.startswith("/*", i):
            block_depth += 1
            i += 2
            continue

        # A literal may open here. The `r`/`b`/`c` prefixes only mean "literal"
        # at a token boundary -- mid-identifier they are ordinary letters. A bare
        # quote needs no boundary: it always opens one.
        boundary = _at_token_boundary(line, i)
        if boundary:
            m = RAW_OPEN.match(line, i)
            if m:
                hashes = len(m.group(1))
                i, closed = _consume_raw(line, m.end(), hashes)
                if not closed:
                    raw_hashes = hashes
                continue
        if boundary or line[i] in "\"'":
            m = STRING_LIT.match(line, i)
            if m:
                i = m.end()
                continue

        out.append(line[i])
        i += 1

    return "".join(out), ScanState(block_depth, raw_hashes)


def annotation_reason(line: str) -> str | None:
    """The reason text of an `// allow-raw-zone: <reason>` annotation, if any."""
    m = ALLOW_ANNOTATION.search(line)
    return m.group("reason") if m else None


def borrow_hits(code: str) -> int:
    """Count `&mut <expr>.<zone>` borrows handed to a callee that is not known
    to preserve membership."""
    n = 0
    for m in BORROW.finditer(code):
        callee = BORROW_CALLEE.search(code[: m.start()])
        if callee and callee.group(1) in MEMBERSHIP_PRESERVING_CALLEES:
            continue
        n += 1
    return n


def iter_production_lines(rel: str, lines: list[str]):
    """Yield `(index, raw, code, enclosing_fn)` for every PRODUCTION line.

    This is the scanner, split out from the classifier: strings and comments
    stripped, inline `#[cfg(test)]` mod bodies skipped by brace depth, enclosing
    fn tracked, `CensusError` on desync. WHAT counts as a hit is the caller's
    business; WHERE production code lives is this function's.

    Split out because the Draw-replacement census
    (`draw_replacement_census.py`) needs exactly this notion of "production
    engine code" and nothing else. A second copy of it would be a second
    definition of production code, free to drift -- and its bugs would be
    silent in both directions (test code scanned as production, production
    skipped as test).

    NOTE for callers: `code` has string literals REMOVED (raw strings included),
    because brace counting must not see a `{` inside a literal. A pattern that
    needs a literal (e.g. matching `"Draw" => ReplacementEvent::Draw`) will never
    fire against `code`. Rewrite the pattern to key on the code around the
    literal, or match `raw` and accept that comments are then in scope.
    """
    current_fn = "<module>"
    skip_until_depth: int | None = None
    depth = 0
    pending_cfg_test = False
    state = ScanState()

    for i, raw in enumerate(lines):
        code, state = strip_noncode(raw, state)

        # Track an inline `#[cfg(test)] mod foo { .. }` body and skip it whole.
        # A naive "first #[cfg(test)] wins" is wrong: engine.rs has 10 and
        # synthesis.rs has 75, nearly all `#[cfg(test)] mod foo;` *declarations*
        # of outlined files, which are excluded by name instead.
        #
        # Keyed on `code`, never `raw`, so a `#[cfg(test)]` QUOTED inside a raw
        # string is text and cannot arm the skip. Belt-and-braces: the `code.strip()`
        # clear below already saves this today, because a raw-string terminator
        # always leaves at least a `;` behind. That is a coincidence of the
        # terminator's punctuation, not a property anyone declared -- and a
        # structural decision taken on unstripped text is exactly the bug this
        # function exists to prevent.
        if skip_until_depth is None:
            if is_cfg_test_attr(code):
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
            if depth < skip_until_depth:
                # The mod closed more braces than it opened: brace tracking has
                # desynced, so every downstream classification in this file is
                # untrustworthy. Fail loudly rather than under-report.
                raise CensusError(
                    f"{rel}:{i + 1}: brace tracking desynced leaving a "
                    f"#[cfg(test)] mod (depth {depth} < {skip_until_depth})"
                )
            if depth == skip_until_depth:
                skip_until_depth = None
            continue

        m = FN_DECL.match(code)
        if m:
            current_fn = m.group(1)

        depth += opened - closed

        yield i, raw, code, current_fn


def census_file(path: Path) -> list[tuple[str, str, str]]:
    """Classify every non-test hit in one file.

    Returns (rel_path, enclosing_fn, family) triples -- one per hit, so callers
    can count multiple distinct branches inside the same function. An annotated
    hit is classified into the `exempt` family rather than dropped: an exemption
    no instrument counts is an exemption nobody revisits.
    """
    rel = str(path.relative_to(REPO_ROOT))
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()

    hits: list[tuple[str, str, str]] = []

    for i, raw, code, current_fn in iter_production_lines(rel, lines):
        n = sum(bool(pattern.search(code)) for _, pattern in FAMILIES)
        n += borrow_hits(code)
        if not n:
            continue

        # An explicitly classified non-event operation is still counted -- as an
        # exemption. It is capped by the same ratchet, so the annotation cannot
        # become the cheap way to add a raw zone mutation.
        reason = annotation_reason(raw) or (annotation_reason(lines[i - 1]) if i > 0 else None)
        if reason:
            hits.extend([(rel, current_fn, "exempt")] * n)
            continue
        if ALLOW_ANNOTATION_BARE in raw or (i > 0 and ALLOW_ANNOTATION_BARE in lines[i - 1]):
            raise CensusError(
                f"{rel}:{i + 1}: `{ALLOW_ANNOTATION_BARE}` needs a reason: "
                f"`// {ALLOW_ANNOTATION_BARE}: <why this is not a zone event>`"
            )

        for family, pattern in FAMILIES:
            if pattern.search(code):
                hits.append((rel, current_fn, family))
        hits.extend([(rel, current_fn, "borrow")] * borrow_hits(code))

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
        bypass = {k: v for k, v in added.items() if k[2] != "exempt"} or {
            k: v for k, v in grown.items() if k[2] != "exempt"
        }
        print("ERROR: raw zone mutation grew.\n", file=sys.stderr)
        for (f, fn, fam), n in sorted(added.items()):
            print(f"  NEW      {f}::{fn} ({fam} x{n})", file=sys.stderr)
        for (f, fn, fam), (was, now) in sorted(grown.items()):
            print(f"  GREW     {f}::{fn} ({fam}) {was} -> {now}", file=sys.stderr)
        if bypass:
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
        print(
            "An `exempt` row means an ANNOTATED site. Exemptions are ratcheted too:\n"
            "the annotation is the cheapest way to add a raw zone mutation, so a new\n"
            "one is a reviewed decision, not a local one. If it is genuinely not a\n"
            "zone event, get the exemption reviewed and run --write.\n",
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
