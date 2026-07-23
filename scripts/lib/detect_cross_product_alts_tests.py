#!/usr/bin/env python3
"""Tests for family (D) of the parser-combinator gate (detect-cross-product-alts.py).

The detector finds the END of an `alt((...))` block by counting parentheses. That
count is a LEX, and a lex that reads literals and comments as code is the ceiling
class that has now bitten this repo's other scanner three times (raw strings
#5704, lifetimes #5705, backslash-continued strings #5715). Here it has teeth in
BOTH directions, because the gate it feeds is diff-based and commit-blocking:

    a comment that OPENS a phantom block   -> FALSE HIT   -> blocks a good commit
    a comment that TRUNCATES a real block  -> FALSE MISS  -> a real cross-product
                                                             alt lands on main, i.e.
                                                             string-matching parsing
                                                             ships past the gate
                                                             built to stop it

So the tests drive `flagged_blocks` — the DECISION — not the block coordinates.
A test that can only see coordinates cannot pin what the gate actually does.

Run:  python3 scripts/lib/detect_cross_product_alts_tests.py
"""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

_SPEC = importlib.util.spec_from_file_location(
    "detect_cross_product_alts", Path(__file__).with_name("detect-cross-product-alts.py")
)
xp = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(xp)


def flags(src: str) -> list[str]:
    """Every flag the gate would raise on `src`, with the whole file treated as
    added (the gate only reports blocks a diff touches; here everything is new)."""
    lines = src.splitlines(keepends=True)
    added = set(range(1, len(lines) + 1))
    return xp.flagged_blocks(lines, added, "t.rs")


# A genuine 4-arm cross product: same prefix, same suffix, one varying interior
# word. This is the shape the gate exists to catch (PATTERNS.md 8b).
CROSS_PRODUCT_ARMS = (
    '        tag("in addition to its other types"),\n'
    '        tag("in addition to their other types"),\n'
    '        tag("in addition to his other types"),\n'
    '        tag("in addition to her other types"),\n'
)


class CommentLexingTests(unittest.TestCase):
    """A comment is not code. The paren counter must not read one."""

    def test_comment_mentioning_alt_does_not_open_a_phantom_block(self) -> None:
        # FALSE HIT. A comment that merely QUOTES `alt((` — e.g. explaining the
        # factoring that was already done — opens a block in a raw-text scan. The
        # block then swallows the ordinary `tag(...)` bindings below it, which
        # share a prefix and suffix because they are the same grammar axis, and
        # the gate blocks a commit over an `alt` that does not exist.
        src = (
            "// Historical note: this used to be a flat `alt((\n"
            "// ... which we since factored. Do not reintroduce.\n"
            "fn parse_axis(i: &str) -> IResult<&str, ()> {\n"
            '    let a = tag("in addition to its other types");\n'
            '    let b = tag("in addition to their other types");\n'
            '    let c = tag("in addition to his other types");\n'
            '    let d = tag("in addition to her other types");\n'
            "    Ok((i, ()))\n"
            "}\n"
        )
        self.assertEqual(flags(src), [])

    def test_stray_parens_in_a_comment_do_not_truncate_a_real_block(self) -> None:
        # FALSE MISS — the direction that matters. `alt((` gives the counter two
        # units of depth headroom, so ONE stray paren is survivable; two are not.
        # A comment inside the block carrying `))` closes it early, the arms below
        # are never collected, and a real cross product ships unflagged.
        src = (
            "fn f(i: &str) -> IResult<&str, ()> {\n"
            "    alt((\n"
            '        // handles the ")" and "))" cases))\n'
            f"{CROSS_PRODUCT_ARMS}"
            "    ))\n"
            "    .parse(i)\n"
            "}\n"
        )
        self.assertEqual(len(flags(src)), 5)  # 1 header + 4 arm lines
        self.assertIn("4 arms", flags(src)[0])

    def test_block_comment_inside_an_alt_does_not_desync(self) -> None:
        # The same failure with `/* */`, which does not stop at the end of a line.
        src = (
            "fn f(i: &str) -> IResult<&str, ()> {\n"
            "    alt((\n"
            "        /* the )) shape\n"
            "           spans lines )) */\n"
            f"{CROSS_PRODUCT_ARMS}"
            "    ))\n"
            "    .parse(i)\n"
            "}\n"
        )
        self.assertIn("4 arms", flags(src)[0])


class LiteralLexingTests(unittest.TestCase):
    """A string literal is not code either. Its parens are data."""

    def test_paren_inside_a_tag_literal_does_not_end_the_block(self) -> None:
        # `take_until(")")` is real parser code (oracle.rs and static_helpers.rs
        # both carry one). The `)` inside the literal is DATA; counted, it eats
        # the block's depth and can close it before the arms are read.
        src = (
            "fn f(i: &str) -> IResult<&str, ()> {\n"
            "    alt((\n"
            '        preceded(take_until("))"), tag("in addition to its other types")),\n'
            '        tag("in addition to their other types"),\n'
            '        tag("in addition to his other types"),\n'
            '        tag("in addition to her other types"),\n'
            "    ))\n"
            "    .parse(i)\n"
            "}\n"
        )
        self.assertIn("4 arms", flags(src)[0])

    def test_raw_string_parens_are_data(self) -> None:
        # A raw string honours no escapes and can hold anything, parens included.
        src = (
            "fn f(i: &str) -> IResult<&str, ()> {\n"
            "    alt((\n"
            '        preceded(tag(r#"))"#), tag("in addition to its other types")),\n'
            '        tag("in addition to their other types"),\n'
            '        tag("in addition to his other types"),\n'
            '        tag("in addition to her other types"),\n'
            "    ))\n"
            "    .parse(i)\n"
            "}\n"
        )
        self.assertIn("4 arms", flags(src)[0])


class PreservedBehaviourTests(unittest.TestCase):
    """What the gate already does correctly, and must keep doing. The lexer
    delegation must not cost any of it."""

    def test_real_cross_product_is_flagged(self) -> None:
        src = (
            "fn f(i: &str) -> IResult<&str, ()> {\n"
            "    alt((\n"
            f"{CROSS_PRODUCT_ARMS}"
            "    ))\n"
            "    .parse(i)\n"
            "}\n"
        )
        out = flags(src)
        self.assertIn("4 arms", out[0])
        self.assertIn("in addition to ", out[0])

    def test_distinct_word_dispatch_is_not_flagged(self) -> None:
        # destroy/exile/sacrifice share neither prefix nor suffix: a legitimate
        # alt, and never the gate's business.
        src = (
            "fn f(i: &str) -> IResult<&str, ()> {\n"
            "    alt((\n"
            '        tag("destroy"),\n'
            '        tag("exile"),\n'
            '        tag("sacrifice"),\n'
            '        tag("counter"),\n'
            "    ))\n"
            "    .parse(i)\n"
            "}\n"
        )
        self.assertEqual(flags(src), [])

    def test_allow_noncombinator_annotation_still_exempts(self) -> None:
        # The escape hatch lives in a COMMENT. Structure is read off the code
        # stream, but the annotation's CONTENT is the point — so it is read off the
        # RAW line. A scan that consulted stripped code here would strip the hatch
        # away and re-flag every annotated block in the tree.
        src = (
            "fn f(i: &str) -> IResult<&str, ()> {\n"
            "    // allow-noncombinator: the axes are genuinely independent here\n"
            "    alt((\n"
            f"{CROSS_PRODUCT_ARMS}"
            "    ))\n"
            "    .parse(i)\n"
            "}\n"
        )
        self.assertEqual(flags(src), [])

    def test_tag_literals_are_read_from_raw_text_not_stripped_code(self) -> None:
        # The other half of the two-API split, pinned: the block's STRUCTURE comes
        # from the code stream, but the arms ARE string literals. A detector that
        # collected its tags from stripped code would find zero arms and flag
        # nothing, ever — blind, not hardened. (#76)
        src = (
            "fn f(i: &str) -> IResult<&str, ()> {\n"
            "    alt((\n"
            f"{CROSS_PRODUCT_ARMS}"
            "    ))\n"
            "    .parse(i)\n"
            "}\n"
        )
        self.assertIn('tag("in addition to its other types")', "\n".join(flags(src)))

    def test_untouched_blocks_are_frozen(self) -> None:
        # The gate only reports blocks a diff TOUCHES. A pre-existing offender that
        # no added line intersects stays frozen in amber.
        src = (
            "fn f(i: &str) -> IResult<&str, ()> {\n"
            "    alt((\n"
            f"{CROSS_PRODUCT_ARMS}"
            "    ))\n"
            "}\n"
        )
        lines = src.splitlines(keepends=True)
        self.assertEqual(xp.flagged_blocks(lines, set(), "t.rs"), [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
