#!/usr/bin/env python3
"""Tests for the census scanner seam (`iter_production_lines` / `strip_noncode`).

The scanner is shared: the zone-authority census AND the Draw-replacement census
classify hits against the `code` it yields, and both rely on it to skip
`#[cfg(test)]` bodies. A scanner that loses track of the source structure does
not merely miss a hit -- it mis-scopes, and a mis-scope is silent in BOTH
directions (test code scanned as production, production skipped as test). So the
seam is tested at the seam, not through either census's classifier.

Everything here drives `iter_production_lines`, because that is what the censuses
consume and it is the level at which the failure modes are observable:

    code            what a pattern is matched against  -> a leak here is a FALSE HIT
    yielded lines   what counts as production          -> a leak here is a MIS-SCOPE
    CensusError     the loud failure                   -> better than a silent lie

Run:  python3 scripts/zone_authority_census_tests.py
"""

from __future__ import annotations

import unittest

from zone_authority_census import CensusError, iter_production_lines


def scan(src: str) -> list[tuple[int, str, str, str]]:
    """Every production line of `src`, as the censuses see it."""
    return list(iter_production_lines("t.rs", src.splitlines()))


def code_of(src: str) -> str:
    """The scanned code of `src`, one line per source line, comments/strings gone.

    Non-production (skipped `#[cfg(test)]`) lines are absent -- that is the
    point: what is not here cannot be classified.
    """
    return "\n".join(code for _i, _raw, code, _fn in scan(src))


class RawStringTests(unittest.TestCase):
    """Rust raw strings: `r"..."`, `r#"..."#` at any `#` depth, and the `b`/`c`
    prefixed forms. Their contents are DATA and must never reach a classifier or
    the brace counter."""

    def test_plain_raw_string_content_is_not_code(self) -> None:
        code = code_of('let s = r"add_to_zone(x)"; let y = 1;')
        self.assertNotIn("add_to_zone", code)
        self.assertIn("let y = 1;", code)

    def test_hashed_raw_string_content_is_not_code(self) -> None:
        # The `#` delimiters are DELIMITERS -- they are not code either.
        code = code_of('let s = r#"add_to_zone(x)"#; let y = 1;')
        self.assertNotIn("add_to_zone", code)
        self.assertNotIn("#", code)
        self.assertIn("let y = 1;", code)

    def test_arbitrary_hash_depth(self) -> None:
        # Only a quote followed by the SAME number of `#` closes the literal, so
        # `"##` and a bare `"` inside `r###"..."###` are content, not the end.
        code = code_of('let s = r###"a"## b" c"###; let y = 1;')
        self.assertEqual(code, "let s = ; let y = 1;")

    def test_byte_and_c_string_raw_forms(self) -> None:
        for lit in ('br"x{"', 'br#"x{"#', 'cr"x{"', 'cr#"x{"#', 'br##"x{"##'):
            with self.subTest(lit=lit):
                code = code_of(f"let s = {lit}; let y = 1;")
                self.assertEqual(code, "let s = ; let y = 1;")

    def test_non_raw_byte_and_c_strings_still_stripped(self) -> None:
        for lit in ('b"x{"', 'c"x{"', '"x{"'):
            with self.subTest(lit=lit):
                code = code_of(f"let s = {lit}; let y = 1;")
                self.assertEqual(code, "let s = ; let y = 1;")

    def test_raw_string_prefix_must_start_at_a_token_boundary(self) -> None:
        # An identifier ending in `r`/`b`/`c` is not a raw-string prefix. `for` is
        # not `f` + `or`, and `r` here is the tail of a name, not a literal.
        code = code_of('let bar = 1; let s = "x"; let cr = 2;')
        self.assertIn("let bar = 1;", code)
        self.assertIn("let cr = 2;", code)

    # ---- failure shape (a): comment markers inside a raw string ----

    def test_raw_string_comment_markers_do_not_start_a_comment(self) -> None:
        # `//` inside a raw string is DATA. If it opens a line comment, the rest
        # of the line is eaten.
        #
        # The embedded `"` is load-bearing: it is the whole REASON to reach for
        # `r#"..."#`, and it is what breaks a scanner that treats the raw
        # string's inner quotes as an ordinary string literal. Without it the
        # inner `"..."` pairs up by luck and the bug hides.
        code = code_of('let s = r#"a" // b"#; add_to_zone(z);')
        self.assertNotIn("//", code)
        self.assertIn("add_to_zone(z);", code)

    def test_raw_string_block_comment_open_does_not_eat_the_file(self) -> None:
        # Same shape with `/*`, which does not stop at the end of the line: it
        # runs to the next `*/` ANYWHERE, i.e. it can eat the rest of the file.
        src = 'let s = r#"a" /* b"#;\nadd_to_zone(a);\nlet z = 2;\n'
        code = code_of(src)
        self.assertIn("add_to_zone(a);", code)
        self.assertIn("let z = 2;", code)

    # ---- failure shape (b): the multi-line remainder ----

    def test_multiline_raw_string_remainder_survives(self) -> None:
        # The historical bite: the remainder after a multi-line raw string was
        # dropped, so the production code that followed was never scanned.
        src = 'let s = r#"\nadd_to_zone(inside);\n"#;\nadd_to_zone(after);\n'
        code = code_of(src)
        self.assertNotIn("inside", code)
        self.assertIn("add_to_zone(after);", code)

    def test_multiline_raw_string_braces_do_not_desync(self) -> None:
        # crates/draft-wasm/src/suggest.rs:437 in miniature: a brace-bearing
        # multi-line raw string inside a `#[cfg(test)]` mod. Unbalanced braces
        # leaking out of it desync brace tracking and the mod's closing brace
        # arrives "early" -> CensusError, or worse, a swallowed production tail.
        src = (
            "fn prod_before() { add_to_zone(a); }\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    fn fixture() -> String {\n"
            '        format!(r#"{{ "name": "{n}", "t": {{ "x": [] }}"#)\n'
            "    }\n"
            "}\n"
            "fn prod_after() { add_to_zone(b); }\n"
        )
        code = code_of(src)  # must not raise CensusError
        self.assertIn("add_to_zone(a);", code)
        self.assertIn("add_to_zone(b);", code)
        self.assertNotIn("name", code)

    # ---- failure shape (c): a fake region marker inside a raw string ----

    def test_raw_string_cannot_toggle_the_cfg_test_region(self) -> None:
        # A raw string that QUOTES `#[cfg(test)] mod ... {` must not start a skip
        # region. If it does, the production code after it silently disappears --
        # the census reports zero hits and calls that a pass.
        src = (
            "fn prod_before() { add_to_zone(a); }\n"
            "const DOC: &str = r#\"\n"
            "#[cfg(test)]\n"
            "mod fake {\n"
            '"#;\n'
            "fn prod_after() { add_to_zone(b); }\n"
        )
        code = code_of(src)
        self.assertIn("add_to_zone(a);", code)
        self.assertIn("add_to_zone(b);", code)

    def test_quoted_cfg_test_does_not_arm_the_next_real_mod(self) -> None:
        # A GUARD, not a regression witness: this passes both before and after
        # the raw-string branch, and it is recorded as green-both-ways rather
        # than dressed up as a catch.
        #
        # It pins the belt to the braces. The region marker is read off `code`,
        # so a quoted `#[cfg(test)]` cannot arm a skip. Today the BRACES alone
        # would also save it -- `pending_cfg_test` is cleared by any line with
        # non-empty code, and a raw-string terminator always leaves at least a
        # `;` behind. That is a coincidence of the terminator's punctuation, not
        # a property anyone declared, so the marker keys on `code` too.
        src = (
            'const DOC: &str = r#"#[cfg(test)]"#;\n'
            "mod real_production {\n"
            "    fn f() { add_to_zone(a); }\n"
            "}\n"
        )
        self.assertIn("add_to_zone(a);", code_of(src))


class LifetimeTests(unittest.TestCase):
    """A `'` opens a char literal ONLY when a single char (or escape) closes it.
    Everywhere else in Rust it opens a LIFETIME (`&'a str`, `Foo<'_>`) or a loop
    LABEL (`'outer:`), which no second `'` ever closes.

    Read as a permissive `'...'` literal, a lifetime tick runs to whatever quote
    comes next and eats the code in between -- the same ceiling as raw strings, and
    with the same two failure modes: a swallowed HIT, and a leaked BRACE that
    desyncs the scanner for the rest of the file.
    """

    def test_lifetime_does_not_swallow_the_hit_after_it(self) -> None:
        # The census-visible loss. An ODD number of ticks is what bites: the lifetime
        # tick pairs with the OPENING quote of a later char literal, and everything
        # between them -- here a `container`-family hit AND the fn's `{` -- is eaten
        # as literal content:
        #
        #     fn drain(&'a mut self) { self.hand.push_back(c); assert!(c != 'x'); }
        #       ->  fn drain(&x'); }
        #
        # The census does not report a mis-scan; it reports one fewer hit, which
        # reads exactly like migration progress.
        code = code_of("    fn drain(&'a mut self) { self.hand.push_back(c); assert!(c != 'x'); }")
        self.assertIn(".hand.push_back(c);", code)
        self.assertEqual(code.count("{"), 1)

    def test_lifetime_does_not_eat_a_brace(self) -> None:
        # `struct S<'a> { x: &'a str }` -- the `{` lives BETWEEN the two ticks, so a
        # permissive char literal eats it and the line closes a brace it never opened.
        code = code_of("struct S<'a> { x: &'a str }")
        self.assertEqual(code.count("{"), 1)
        self.assertEqual(code.count("}"), 1)

    def test_lifetime_before_a_brace_char_literal_does_not_leak_the_brace(self) -> None:
        # The witness, verbatim from crates/engine/src/parser/oracle_replacement.rs:
        # the lifetime tick in `<'_>` pairs with the OPENING quote of the `'{'` char
        # literal, so the `{` is left behind as code. This is the strictly worse
        # direction: not a missed hit but an INVENTED brace, and brace depth then
        # drifts for every line that follows.
        code = code_of("char::<_, OracleError<'_>>('{'),")
        self.assertNotIn("{", code)
        self.assertNotIn("}", code)  # the line is brace-neutral; the scan must agree

    def test_lifetime_does_not_desync_a_cfg_test_skip_region(self) -> None:
        # The mis-scope, end to end. A lifetime + a `'}'` char literal inside a
        # `#[cfg(test)]` body leaves the eaten line closing braces it never opened;
        # brace tracking walks out through the mod's floor and the scanner either
        # raises CensusError or swallows the production code that follows.
        src = (
            "fn prod_before() { add_to_zone(a); }\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    fn t(c: &'a char) -> bool { *c == '}' }\n"
            "}\n"
            "fn prod_after() { add_to_zone(b); }\n"
        )
        code = code_of(src)  # must not raise CensusError
        self.assertIn("add_to_zone(a);", code)
        self.assertIn("add_to_zone(b);", code)

    def test_loop_label_is_not_a_char_literal(self) -> None:
        # `'outer:` is the same shape as a lifetime and fails the same way: the label
        # tick pairs with the char literal's quote and the loop's `{` goes with it.
        code = code_of("'outer: loop { if c == 'x' { add_to_zone(a); } }")
        self.assertIn("add_to_zone(a);", code)
        self.assertEqual(code.count("{"), code.count("}"))

    def test_char_a_is_a_literal_but_lifetime_a_is_not(self) -> None:
        # The minimal pair, on one line: `'a'` IS a char literal, `'a` is NOT. Rust
        # resolves this the same way -- a quote closing after exactly one char is a
        # literal -- and the scanner must resolve it identically or the generic
        # parameter list is eaten.
        code = code_of("fn f<'a>(x: &'a str) -> char { 'a' }")
        self.assertIn("(x: &'a str)", code)  # the lifetimes survive as ordinary code
        self.assertNotIn("'a'", code)  # the char literal does not
        self.assertEqual(code.count("{"), 1)

    def test_multiple_lifetimes_on_one_line(self) -> None:
        # Four ticks pair up 1-2 and 3-4, so the braces happen to balance and the hit
        # happens to survive -- but the parameter list in between is still eaten. The
        # damage is only invisible to the brace counter, not absent.
        code = code_of("fn f<'a, 'b>(x: &'a str, y: &'b str) { self.hand.push_back(x); }")
        self.assertIn("&'a str", code)
        self.assertIn("&'b str", code)

    def test_static_lifetime(self) -> None:
        # `'static` is the lifetime the tree has most of, and it bites like any other:
        # its tick pairs with the `'x'` literal's opening quote and takes the fn's `{`.
        code = code_of("const S: &'static str = \"x\"; fn g() { assert_eq!(S.next(), Some('x')); }")
        self.assertIn("&'static str", code)
        self.assertEqual(code.count("{"), 1)

    def test_anonymous_lifetime_vs_underscore_char(self) -> None:
        # The other minimal pair: `'_` is the anonymous LIFETIME, `'_'` is the
        # underscore CHAR. One char then a quote is the only thing that separates them.
        code = code_of("fn f(x: &'_ str) -> char { '_' }")
        self.assertIn("&'_ str", code)
        self.assertEqual(code.count("{"), 1)
        self.assertEqual(code.count("}"), 1)

    # ---- the char literals themselves must still be consumed whole ----

    def test_byte_char_literal_is_stripped_despite_its_prefix(self) -> None:
        # `b'x'` is the two-letter-prefix shape that bit the raw-string branch (`br#"`),
        # so it gets its own test rather than a line in a list. It is SAFE here, and
        # for a reason worth pinning: CANDIDATE only stops on a `b` that a `"` follows
        # (`[bcr](?=r?#*")`), so a byte-char literal is never entered at its `b` -- the
        # candidate fires on the QUOTE, the char regex matches `'x'` there, and the `b`
        # is emitted as the ordinary identifier character it lexes like. A `b` prefix
        # alternative in the char regex would therefore be dead code, and is absent.
        for lit in ("b'x'", "b'\\''", "b'\"'", "b'{'"):
            with self.subTest(lit=lit):
                code = code_of(f"let c = {lit}; add_to_zone(a);")
                self.assertNotIn("{", code)
                self.assertIn("add_to_zone(a);", code)

    def test_char_literal_escapes_are_still_stripped(self) -> None:
        # Each is ONE char once escapes are honoured, so each must be consumed whole.
        # `'\''` is the sharp one: its escaped quote must not be read as the closer.
        for lit in ("'x'", "'{'", "'}'", "'\\n'", "'\\''", "'\\\\'", "'\\x41'", "'\\u{1F600}'", "'\\u{10FFFF}'"):
            with self.subTest(lit=lit):
                code = code_of(f"let c = {lit}; add_to_zone(a);")
                self.assertNotIn("{", code)
                self.assertNotIn("}", code)
                self.assertIn("add_to_zone(a);", code)


class PreservedBehaviourTests(unittest.TestCase):
    """The scanner's existing contracts. The raw-string branch must not cost any
    of them."""

    def test_line_comment_is_stripped(self) -> None:
        self.assertEqual(code_of("let x = 1; // add_to_zone(a)"), "let x = 1; ")

    def test_block_comment_is_stripped_across_lines(self) -> None:
        code = code_of("let a = 1; /* add_to_zone(x)\nstill add_to_zone(y) */ let b = 2;")
        self.assertNotIn("add_to_zone", code)
        self.assertIn("let b = 2;", code)

    def test_nested_block_comment_is_stripped_to_its_true_end(self) -> None:
        # Rust block comments NEST. Closing at the first `*/` leaves the tail of
        # the comment behind as "code" -- and a stray brace in that tail desyncs
        # brace tracking exactly like a raw string does.
        code = code_of("/* a /* b */ add_to_zone(x) */ let y = 1;")
        self.assertNotIn("add_to_zone", code)
        self.assertIn("let y = 1;", code)

    def test_normal_string_content_is_not_code(self) -> None:
        code = code_of('let s = "add_to_zone(x) {"; let y = 1;')
        self.assertNotIn("add_to_zone", code)
        self.assertIn("let y = 1;", code)

    def test_escaped_quote_in_string(self) -> None:
        code = code_of('let s = "a\\"add_to_zone(x)"; let y = 1;')
        self.assertNotIn("add_to_zone", code)
        self.assertIn("let y = 1;", code)

    def test_double_quote_char_literal(self) -> None:
        # `'"'` must be consumed whole: a leaked `"` opens a phantom string that
        # swallows the rest of the line.
        code = code_of("""let c = '"'; add_to_zone(a);""")
        self.assertIn("add_to_zone(a);", code)

    def test_inline_cfg_test_mod_is_skipped(self) -> None:
        src = (
            "fn prod() { add_to_zone(a); }\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    fn t() { add_to_zone(b); }\n"
            "}\n"
            "fn prod2() { add_to_zone(c); }\n"
        )
        code = code_of(src)
        self.assertIn("add_to_zone(a);", code)
        self.assertNotIn("add_to_zone(b);", code)
        self.assertIn("add_to_zone(c);", code)

    def test_compound_cfg_test_predicate_is_skipped(self) -> None:
        src = (
            '#[cfg(all(test, target_arch = "wasm32"))]\n'
            "mod tests {\n"
            "    fn t() { add_to_zone(b); }\n"
            "}\n"
            "fn prod() { add_to_zone(c); }\n"
        )
        code = code_of(src)
        self.assertNotIn("add_to_zone(b);", code)
        self.assertIn("add_to_zone(c);", code)

    def test_cfg_not_test_is_production(self) -> None:
        src = "#[cfg(not(test))]\nmod prod_only {\n    fn f() { add_to_zone(a); }\n}\n"
        self.assertIn("add_to_zone(a);", code_of(src))

    def test_feature_test_support_is_production(self) -> None:
        src = '#[cfg(feature = "test-support")]\nmod s {\n    fn f() { add_to_zone(a); }\n}\n'
        self.assertIn("add_to_zone(a);", code_of(src))

    def test_enclosing_fn_is_tracked(self) -> None:
        src = "fn alpha() {\n    add_to_zone(a);\n}\nfn beta() {\n    add_to_zone(b);\n}\n"
        fns = {code.strip(): fn for _i, _raw, code, fn in scan(src)}
        self.assertEqual(fns["add_to_zone(a);"], "alpha")
        self.assertEqual(fns["add_to_zone(b);"], "beta")

    def test_desync_still_raises(self) -> None:
        # The loud failure must stay loud: a census that silently mis-scopes is
        # worse than no census. The surplus `}` has to land INSIDE the skip
        # region -- that is the only place the guard is armed.
        src = "#[cfg(test)]\nmod tests {\n    fn t() { } } }\n"
        with self.assertRaises(CensusError):
            code_of(src)


if __name__ == "__main__":
    unittest.main(verbosity=2)
