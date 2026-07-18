#!/usr/bin/env python3
"""Detect un-factored cross-product `alt((...))` blocks in parser code.

Family (D) of the parser combinator gate (scripts/check-parser-combinators.sh).
A flat `alt` whose `tag("...")` arms share a long common *prefix AND suffix* is
a cross product that should be factored into per-axis `alt`/`opt` calls — see
crates/engine/src/parser/oracle_nom/PATTERNS.md section 8b.

This is a multi-line structural check (the bash gate's line-regex families can't
see across an `alt` block), so it lives here and is invoked per changed file.

Usage:
    git diff --unified=0 <base> -- <file> | \
        python3 scripts/lib/detect-cross-product-alts.py <file>

Reads the unified=0 diff on stdin (to recover the set of added post-image line
numbers) and the post-image <file> from disk. Prints one report block per
flagged cross product that *intersects an added line* — pre-existing blocks are
frozen in amber, matching the rest of the gate. A block is exempt if any of its
lines (or the line directly above the `alt((`) carries `allow-noncombinator`.

Exit code is always 0; the caller decides pass/fail from whether output is empty.
"""

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from zone_authority_census import ScanState, strip_noncode  # noqa: E402

# Conservative thresholds (see check-parser-combinators.sh family D). A genuine
# cross product has many arms that are nearly identical — long shared prefix AND
# suffix, with only a short interior span varying. Distinct-word dispatch
# (destroy/exile/sacrifice) shares neither and is never flagged.
MIN_ARMS = 4
MIN_PREFIX = 6
MIN_SUFFIX = 5

TAG_RE = re.compile(r'\btag(?:_no_case)?\s*(?:::<[^>]*>)?\s*\(\s*"((?:[^"\\]|\\.)*)"\s*\)')
ALT_OPEN_RE = re.compile(r'\balt\s*\(\s*\(')
HUNK_RE = re.compile(r'^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@')


def added_lines_from_diff(diff_text):
    """Post-image line numbers added in a unified=0 diff."""
    added = set()
    for line in diff_text.splitlines():
        m = HUNK_RE.match(line)
        if m:
            start = int(m.group(1))
            count = int(m.group(2)) if m.group(2) is not None else 1
            added.update(range(start, start + count))
    return added


def common_prefix(strings):
    lo, hi = min(strings), max(strings)
    i = 0
    while i < len(lo) and i < len(hi) and lo[i] == hi[i]:
        i += 1
    return lo[:i]


def common_suffix(strings):
    return common_prefix([s[::-1] for s in strings])[::-1]


def code_stream(lines):
    """`lines` with comments and string/char literals removed.

    Delegated to the census lexer (scripts/zone_authority_census.py), which is the
    single authority for CODE-STREAM lexing in this repo. It already knows raw
    strings (#5704), char-vs-lifetime ticks (#5705) and backslash-continued
    strings (#5715) — three bugs this detector would otherwise have had to
    rediscover one at a time, each of them a literal scanned as code.
    """
    out = []
    state = ScanState()
    for line in lines:
        code, state = strip_noncode(line.rstrip("\n"), state)
        out.append(code)
    return out


def alt_blocks(lines):
    """Yield (start_idx, end_idx, [tag_literals]) for each `alt((...))`, 0-based
    inclusive line indices. Nested alts are yielded independently.

    TWO views of the source, and the split is the whole design (#76):

        STRUCTURE (does an `alt((` open here? where do its parens balance?)
            read off the CODE STREAM. A `)` inside `take_until(")")` is data, and
            counted as code it closes the block early — the arms below are never
            collected and a real cross product ships unflagged, past the gate
            built to stop it. An `alt((` inside a COMMENT is likewise not an alt.

        CONTENT (which literals are the arms?)
            read off the RAW text. The arms ARE string literals: a detector that
            looked for them in the stripped stream would find none, ever, and
            flag nothing — blind, not hardened. This is exactly why the gate's
            sibling families (A/B/E/F, in check-parser-combinators.sh) must NOT
            be routed through strip_noncode: they match ON the literal.
    """
    codes = code_stream(lines)
    n = len(lines)
    for idx, code in enumerate(codes):
        if not ALT_OPEN_RE.search(code):
            continue
        depth = 0
        started = False
        end = idx
        for j in range(idx, min(idx + 60, n)):
            for ch in codes[j]:
                if ch == '(':
                    depth += 1
                    started = True
                elif ch == ')':
                    depth -= 1
            end = j
            if started and depth <= 0:
                break
        tags = TAG_RE.findall(''.join(lines[idx:end + 1]))
        yield idx, end, tags


def flagged_blocks(lines, added, path):
    """Report lines for every flagged cross-product block. Pure: no I/O.

    Split out from `main` so the gate's DECISION is testable at the seam it is
    enforced on. The witnesses that matter are flag decisions ("does this block
    the commit?"), not block coordinates, and a test that can only reach the
    latter cannot pin the former.
    """
    out = []
    for start, end, tags in alt_blocks(lines):
        uniq = []
        for t in tags:
            if t not in uniq:
                uniq.append(t)
        if len(uniq) < MIN_ARMS:
            continue
        cp, cs = common_prefix(uniq), common_suffix(uniq)
        minlen = min(len(t) for t in uniq)
        # Don't let prefix and suffix overlap on short arms (would double-count).
        if len(cp) + len(cs) > minlen:
            cs = cs[: max(0, minlen - len(cp))]
        if len(cp) < MIN_PREFIX or len(cs) < MIN_SUFFIX:
            continue
        # Only flag blocks touched by this diff (freeze existing offenders).
        block_range = range(start + 1, end + 2)  # 1-based, inclusive
        if not added.intersection(block_range):
            continue
        # Escape hatch: allow-noncombinator anywhere in the block or directly above.
        # Read off the RAW lines, deliberately: the annotation IS a comment, so a
        # scan that consulted stripped code here would strip the escape hatch away
        # and re-flag every annotated block. Structure comes from the code stream;
        # anything whose content is the point (annotations, tag literals) is read
        # from the raw text. See #76.
        window = lines[max(0, start - 1): end + 1]
        if any("allow-noncombinator" in w for w in window):
            continue
        out.append(f"  {path}:{start + 1}  ({len(uniq)} arms, shared prefix {cp!r} + suffix {cs!r})")
        for t in uniq[:6]:
            out.append(f'    tag("{t}")')
        if len(uniq) > 6:
            out.append(f"    ... +{len(uniq) - 6} more")
    return out


def main():
    if len(sys.argv) != 2:
        sys.stderr.write("usage: detect-cross-product-alts.py <file> (diff on stdin)\n")
        return 0
    path = sys.argv[1]
    added = added_lines_from_diff(sys.stdin.read())
    if not added:
        return 0
    try:
        with open(path, encoding="utf-8") as f:
            lines = f.readlines()
    except OSError:
        return 0

    for report in flagged_blocks(lines, added, path):
        print(report)

    return 0


if __name__ == "__main__":
    sys.exit(main())
