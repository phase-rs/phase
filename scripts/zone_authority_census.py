#!/usr/bin/env python3
"""Full-tree census of raw zone mutation in the engine.

Every gameplay zone change must go through the replacement-consulting pipeline
(`zone_pipeline::move_object` -> `ApprovedZoneChange` -> delivery). Code that
calls the raw movers in `game/zones.rs`, pokes the `im::Vector` zone containers
directly, or assigns `GameObject::zone` bypasses replacement consultation,
`ZoneChanged` events, triggers, and draw bookkeeping.

This census is the hard authority gate for that migration (Plan 03). It
classifies every production hit by (file, enclosing fn, pattern family), and
fails every hit that lacks a nonempty `allow-raw-zone:` annotation. An
annotation records a reviewed operation that is not a replaceable zone event;
it remains visible in `--list` rather than disappearing from the census.

Pattern families (a hit is classified into exactly one):

    mover        the five raw movers game/zones.rs exports
    container    direct membership mutation of an im::Vector zone container
    zone-assign  direct `GameObject::zone = ...`
    borrow       a `&mut <expr>.<zone>` handed to a callee that is not known to
                 preserve membership (a shuffle is; anything else must prove it)
    exempt       any of the above, annotated `// allow-raw-zone: <reason>`

The annotation reason is mandatory. A raw zone operation cannot be introduced
without naming why it is outside the replacement-consulting pipeline.

Usage:
    scripts/zone_authority_census.py --check      # gate (used by CI)
    scripts/zone_authority_census.py --list       # report every classified hit
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import NamedTuple

REPO_ROOT = Path(__file__).resolve().parent.parent

SCOPES = ("crates/engine/src", "crates/engine-wasm/src")
ENGINE_GAME_DIR = REPO_ROOT / "crates" / "engine" / "src" / "game"

# The authority modules themselves: raw delivery is their implementation.
AUTHORITY_FILES = {"zones.rs", "zone_pipeline.rs"}

# Test-support placement helpers are excluded only when game/mod.rs explicitly
# exposes them through the named test-support boundary. This guards against a
# future scenario.rs becoming a production module while retaining its basename.
TEST_SUPPORT_EXPORT = re.compile(
    r'^\s*#\[cfg\(any\(test,\s*feature\s*=\s*"test-support"\)\)\]\s*$'
    r"\n^\s*pub\s+mod\s+(?P<module>\w+)\s*;\s*$",
    re.MULTILINE,
)


def test_support_modules(game_mod_source: str) -> frozenset[str]:
    """Return game modules explicitly compiled only for tests/test-support."""
    return frozenset(match.group("module") for match in TEST_SUPPORT_EXPORT.finditer(game_mod_source))


TEST_SUPPORT_MODULES = test_support_modules((ENGINE_GAME_DIR / "mod.rs").read_text(encoding="utf-8"))

# Compatibility surface for sibling censuses that share this scanner. Scenario
# infrastructure deliberately does NOT appear here: its exclusion is derived
# from the feature gate above, not its basename.
TEST_SUPPORT_FILES = frozenset({"testing.rs"})


def is_feature_gated_test_support_module(path: Path) -> bool:
    """Whether `path` is a game module behind the explicit test-support gate."""
    return path.parent == ENGINE_GAME_DIR and path.stem in TEST_SUPPORT_MODULES

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
# are allowed: a shuffle reorders the library (CR 701.24) and is not a zone
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
#
# The char alternative is EXACTLY ONE char (or one escape) followed by the closing
# quote, and that precision is the whole point: in Rust a `'` also opens a LIFETIME
# (`&'a str`, `Foo<'_>`) and a loop LABEL (`'outer: loop`), neither of which is ever
# closed by a second `'`. A permissive `'(?:\\.|[^'\\])*'` cannot tell them apart --
# it runs from a lifetime tick to whatever quote comes next and eats the code in
# between, braces included:
#
#     char::<_, OracleError<'_>>('{'),   ->   char::<_, OracleError<{'),
#
# which does not merely lose a hit: it LEAKS a `{` into the code stream and desyncs
# brace tracking for the rest of the file -- the raw-string failure mode again.
# Rust's own rule is the one encoded here: `'x'`, `'\n'`, `'\x41'`, `'\u{1F600}'` are
# literals; a `'` that does not close after one char is a lifetime, and the scanner
# emits its tick as ordinary code. (The byte form `b'x'` needs no prefix alternative:
# CANDIDATE only stops on a `b` that a `"` follows, so a byte-char literal is always
# entered at its quote and its `b` is scanned as the ordinary code it lexes like.)
STRING_LIT = re.compile(
    r'(?:b|c)?"(?:\\.|[^"\\])*"'
    r"|'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]{1,6}\}|.)|[^'\\])'"
)

# The BODY of a non-raw string, from just past its opening quote through its
# closing quote. Escapes are honoured -- `\"` is content, `\\` is a spent backslash
# -- and a line that ends in a backslash matches NOTHING here, which is precisely
# the Rust rule: a `\` immediately before the newline escapes it and the literal
# RUNS ON to the next line (Reference, STRING_CONTINUE). So a non-match is not a
# syntax error to guess around; it is the signal to carry the string.
STRING_BODY = re.compile(r'(?:\\.|[^"\\])*"')

# A raw-string opener: `r"`, `r#"`, `r##"`, and the byte/C-string forms `br#"` /
# `cr#"`. Rust raw strings do NOT nest and honour NO escapes, so the `#` count is
# the only thing that closes one -- and the only thing we have to carry.
RAW_OPEN = re.compile(r'(?:b|c)?r(#*)"')

# Where a comment or a literal could START. Everything between two candidates is
# ordinary code and is appended in ONE slice.
#
# This exists for throughput, and it is load-bearing: the scanner sweeps ~7MB of
# Rust per census run, and a character-at-a-time loop that fires a regex per
# character does that at ~0.2 MB/s -- minutes per gate. Only these positions can
# open something:
#
#     /   a line or block comment            "  '   a string or char literal
#     b c r   a literal prefix, but ONLY when a `"` follows (through any `#`s),
#             which is what the lookahead checks -- otherwise every identifier
#             starting with b/c/r would be a false stop
#
# Over-inclusion here is free (the branch logic below rejects a false candidate
# and moves on). Under-inclusion is a BUG: a missed candidate is a literal
# scanned as code. Every construct the branches can consume starts at one of
# these characters.
#
# EVERY alternative starts with a character from one small set, deliberately:
# that lets the regex engine prefilter on the first character and skip runs of
# ordinary code at C speed. Do NOT add a lookbehind here to enforce the token
# boundary -- it defeats the prefilter and drags the scan back to per-character
# lookaround. `_at_token_boundary` already enforces it, in Python, at the few
# positions that survive this far.
# The `r?` in the lookahead is load-bearing: it lets the candidate fire on the
# FIRST letter of a two-letter prefix (`br#"`, `cr"`). Without it the scan stops
# on the `r` instead, where the boundary check correctly rejects it -- and the
# literal is then mis-lexed as an ordinary string. The test suite catches this.
CANDIDATE = re.compile(r"""[/"']|[bcr](?=r?#*")""")

IDENT_CHARS = frozenset("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_")

# Bound up front: these are called once per candidate across every line of the
# tree, and re-resolving the attribute each time is measurable at that volume.
_find_candidate = CANDIDATE.search
_match_raw_open = RAW_OPEN.match
_match_string_lit = STRING_LIT.match
_match_string_body = STRING_BODY.match


class ScanState(NamedTuple):
    """Lexer state carried BETWEEN lines.

    THREE constructs span a line boundary, and each needs its own carrier:

        block comments   nest, so they need a DEPTH (`/* /* */ */`)
        raw strings      close only on their own `#` count, so they need that COUNT
        non-raw strings  continue on a trailing `\\`, which is all-or-nothing: a FLAG

    They are mutually exclusive by construction. A raw string opened inside a block
    comment is comment text; a `/*` inside either kind of string is data; a `\\` is
    inert inside a raw string, which honours no escapes at all.
    """

    block_depth: int = 0
    raw_hashes: int | None = None  # `#` count of the raw string we are inside
    in_string: bool = False  # inside a non-raw string continued from the line above


class CensusError(Exception):
    """The scanner lost track of the source structure. Never guess -- a census
    that silently mis-scopes is worse than no census."""


def _at_token_boundary(line: str, i: int) -> bool:
    """True if `i` starts a token. `r`/`b`/`c` mean "literal prefix" only at a
    token boundary; elsewhere they are the tail of an identifier."""
    return i == 0 or line[i - 1] not in IDENT_CHARS


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


def _consume_string(line: str, i: int) -> tuple[int, bool]:
    """Consume non-raw string body from `i` (just past the opening `"`).

    Returns (next_index, closed_on_this_line). A non-raw string is closed by the
    first unescaped `"`; if there is none, the line ends in a `\\` that escapes the
    newline and the literal continues onto the next line -- Rust's STRING_CONTINUE,
    and the ONLY way a non-raw string spans a line break (an unescaped newline
    inside one is a compile error, so there is no third case to guess at).
    """
    m = _match_string_body(line, i)
    if m is None:
        return len(line), False
    return m.end(), True


def strip_noncode(line: str, state: ScanState) -> tuple[str, ScanState]:
    """Return (code, state_for_the_next_line).

    Strings and comments are removed before ANY brace counting or pattern
    matching. Brace counting in particular must not see a stray `{` inside a
    literal: inside a skipped `#[cfg(test)]` mod that would extend the skip past
    the mod's closing brace and silently swallow the production code that
    follows.

    Multi-line literals are the reason this is a state machine rather than a regex.
    A raw string's contents are arbitrary bytes -- `//`, `/*`, `{`, `"`, and
    `#[cfg(test)]` all appear inside them as DATA (see any `format!(r#"{{...}}"#)`
    JSON fixture) -- and an ORDINARY string carries the same data across the same
    line break whenever it ends in a `\\` (Rust's STRING_CONTINUE), which is how
    every wrapped `assert!` message in the tree is written. A scanner that
    mishandles either does not just miss a hit: it starts a comment that eats the
    file, leaks a brace into the counter, or opens a skip region that eats the
    production code after it.
    """
    # Fast path. 4 lines in 5 hold no comment and no literal, and for those the
    # scanner has nothing to do -- so it should ALLOCATE nothing: no char list,
    # no join, no fresh ScanState. Doing the work anyway is most of what made the
    # per-character version too slow to run on the tree it was written to sweep.
    if (
        state.block_depth == 0
        and state.raw_hashes is None
        and not state.in_string
        and _find_candidate(line) is None
    ):
        return line, state

    out: list[str] = []
    i = 0
    n = len(line)
    block_depth = state.block_depth
    raw_hashes = state.raw_hashes
    in_string = state.in_string
    append = out.append

    while i < n:
        if raw_hashes is not None:
            i, closed = _consume_raw(line, i, raw_hashes)
            if closed:
                raw_hashes = None
            continue

        if in_string:
            # A continued non-raw string. Its body is data exactly like a raw
            # string's -- and unlike a raw string's, it honours escapes, so the
            # closing quote is the first UNESCAPED one.
            i, closed = _consume_string(line, i)
            in_string = not closed
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

        # Skip straight to the next position that could open a comment or a
        # literal, taking everything before it as code in one slice.
        m = _find_candidate(line, i)
        if m is None:
            append(line[i:])
            break
        start = m.start()
        if start > i:
            append(line[i:start])
            i = start

        ch = line[i]
        if ch == "/":
            nxt = line[i + 1 : i + 2]
            if nxt == "/":
                break
            if nxt == "*":
                block_depth += 1
                i += 2
                continue
            append("/")  # division, not a comment
            i += 1
            continue

        # A literal may open here. The `r`/`b`/`c` prefixes only mean "literal"
        # at a token boundary -- mid-identifier they are ordinary letters. A `"`
        # needs no boundary: it always opens a string. A `'` is the one candidate
        # that is genuinely ambiguous, and STRING_LIT is what resolves it: it opens
        # a char literal only if a single char (or escape) then a closing quote
        # follows -- otherwise it is a lifetime or a loop label and falls through.
        boundary = i == 0 or line[i - 1] not in IDENT_CHARS
        if boundary:
            m = _match_raw_open(line, i)
            if m:
                hashes = len(m.group(1))
                i, closed = _consume_raw(line, m.end(), hashes)
                if not closed:
                    raw_hashes = hashes
                continue
        if boundary or ch == '"' or ch == "'":
            m = _match_string_lit(line, i)
            if m:
                i = m.end()
                continue

            # No closing quote on this line. For a `"` -- with or without its `b`/`c`
            # prefix -- that is not a syntax error and not a false candidate: Rust
            # closes a non-raw string with an unescaped quote or CONTINUES it past a
            # trailing `\`, and nothing else is legal. So the literal is open, and it
            # is open ACROSS the line boundary. Carry it, and the body it holds --
            # braces, `//`, `add_to_zone(` and all -- is data on every line it spans.
            # (A `'` that reaches here is a lifetime or a loop label, which no second
            # quote closes; it opens nothing and falls through to the code stream.)
            if ch == '"':
                quote = i + 1
            elif ch in ("b", "c") and line[i + 1 : i + 2] == '"':
                quote = i + 2
            else:
                quote = None
            if quote is not None:
                i, closed = _consume_string(line, quote)
                in_string = not closed
                continue

        # A false candidate: an `r`/`b`/`c` that opens nothing, or a `'` that opens
        # a lifetime rather than a literal. Either way it is ordinary code: emit the
        # one character and let the next slice pick up the rest of the token.
        append(ch)
        i += 1

    return "".join(out), ScanState(block_depth, raw_hashes, in_string)


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
        # exemption -- so `--list` remains a complete review surface.
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
            if name in AUTHORITY_FILES or is_feature_gated_test_support_module(path):
                continue
            if name == "tests.rs" or name.endswith("_tests.rs"):
                continue
            for key in census_file(path):
                counts[key] = counts.get(key, 0) + 1
    return counts


def render(counts: dict[tuple[str, str, str], int]) -> str:
    rows = [f"{f}\t{fn}\t{fam}\t{n}" for (f, fn, fam), n in sorted(counts.items())]
    return "\n".join(rows) + ("\n" if rows else "")


def unannotated_hits(counts: dict[tuple[str, str, str], int]) -> dict[tuple[str, str, str], int]:
    """Return raw zone operations not authorized by `allow-raw-zone:`."""
    return {key: count for key, count in counts.items() if key[2] != "exempt"}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--check", action="store_true", help="require an annotation on every classified hit")
    g.add_argument("--list", action="store_true", help="print every classified hit")
    args = ap.parse_args()

    try:
        counts = collect()
    except CensusError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 1
    total = sum(counts.values())

    if args.list:
        sys.stdout.write(render(counts))
        print(f"\n{total} classified production hits in {len(counts)} (file, fn, family) rows", file=sys.stderr)
        return 0

    bypasses = unannotated_hits(counts)
    if bypasses:
        print("ERROR: raw zone mutation lacks allow-raw-zone annotation.\n", file=sys.stderr)
        for (f, fn, fam), count in sorted(bypasses.items()):
            print(f"  UNANNOTATED {f}::{fn} ({fam} x{count})", file=sys.stderr)
        print(
            "\nA gameplay zone change must go through zone_pipeline so replacement effects,\n"
            "ZoneChanged events, triggers, and draw bookkeeping all run. If an operation is\n"
            "genuinely not a replaceable zone event, annotate it with:\n\n"
            "    // allow-raw-zone: <one-line reason>\n",
            file=sys.stderr,
        )
        return 1

    print(f"Gate B PASS: {total} classified production hits carry allow-raw-zone annotations ({len(counts)} rows)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
