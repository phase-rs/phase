#!/usr/bin/env python3
"""Tests for check-skill-doc.sh.

The script is a drift gate, so what matters is not that it runs but that it
refuses what it should. Every anchor row is a claim that renaming its symbol
breaks the build; a row that keeps passing after its symbol is gone is worse
than no row, because it reports green while the doc it guards has rotted.

That is not hypothetical. Two classes of it have already shipped:

  * An unanchored substring row absorbed its own longer siblings, so
    `fn parse_type_phrase` kept matching `parse_type_phrase_folding` and ~48
    `#[test] fn parse_type_phrase_*` names after the symbol it named was
    renamed away (phase-rs/phase#8613, #8633).
  * A row carrying no declaration keyword was satisfied by any mention, so
    `ROUTER_KEYWORD_CASES` matched a doc comment and a string literal and
    survived a rename of the `const` it documented (#8633 review).

Each case builds a throwaway repository from the real tree, mutates one thing
in it, and asserts the gate's behaviour there.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SCRIPT = REPO / "scripts" / "check-skill-doc.sh"
SKILL = Path(".claude/skills/oracle-parser/SKILL.md")

# Files the gate reads. Copying the parser tree wholesale keeps the fixture
# honest: the table under test is the real one, not a hand-written stand-in.
TREE = [Path("crates/engine/src/parser"), SKILL]

def _shell() -> str:
    """The bash to run the gate under.

    `bash` from PATH is correct on CI and any POSIX host. It is not always
    correct on Windows, where that name often resolves to WSL's stub, which
    exits non-zero without running anything -- every case then fails for a
    reason that has nothing to do with the gate. `SKILL_DOC_TEST_BASH`
    overrides; otherwise fall back to whatever `shutil.which` finds.
    """
    override = os.environ.get("SKILL_DOC_TEST_BASH")
    if override:
        return override
    found = shutil.which("bash")
    return found or "bash"


SHELL_BIN = _shell()

METACHARACTERS = set(".*+?[]{}()|^$")


def ere_body(pat: str) -> str:
    """A row pattern with its trailing `\\b` anchor removed."""
    return pat[:-2] if pat.endswith("\\b") else pat


def unescaped_metacharacters(pat: str) -> set[str]:
    """The ERE metacharacters a pattern leaves unescaped.

    `check-skill-doc.sh` documents escaping as the supported way to carry one,
    and the gate does accept `fn peel_clause\\(`, so an escaped pair is not an
    offender. Dropping every backslash-escaped pair first is what makes this
    screen agree with the contract rather than overrule it.
    """
    return set(re.sub(r"\\.", "", ere_body(pat))) & METACHARACTERS


class Gate:
    """A throwaway repo. The script cds to its own parent's parent, so copying
    it into <root>/scripts makes <root> the repository it inspects."""

    def __init__(self, root: Path) -> None:
        self.root = root
        (root / "scripts").mkdir(parents=True)
        shutil.copy2(SCRIPT, root / "scripts" / SCRIPT.name)
        for rel in TREE:
            src, dst = REPO / rel, root / rel
            dst.parent.mkdir(parents=True, exist_ok=True)
            if src.is_dir():
                shutil.copytree(src, dst)
            else:
                shutil.copy2(src, dst)

    def run(self) -> subprocess.CompletedProcess:
        return subprocess.run(
            [SHELL_BIN, str(self.root / "scripts" / SCRIPT.name)],
            capture_output=True,
            text=True,
        )

    def read(self, rel: Path) -> str:
        return (self.root / rel).read_text(encoding="utf-8")

    def write(self, rel: Path, text: str) -> None:
        (self.root / rel).write_text(text, encoding="utf-8", newline="\n")

    def rows(self) -> list[tuple[str, str]]:
        """Invariant (2)'s anchor table, as (pattern, file) pairs."""
        body = re.search(
            r"\(2\) Documented anchor symbols.*?<<'EOF'\n(.*?)\nEOF\n",
            self.read(Path("scripts") / SCRIPT.name),
            re.S,
        ).group(1)
        return [
            (line.split("\t", 1)[0], line.split("\t", 1)[1])
            for line in body.split("\n")
            if "\t" in line
        ]

    def retired(self) -> list[str]:
        """Invariant (4)'s retired-symbol list.

        Anchored on the `grep -rqE "fn $dead\\b"` line rather than on prose, so
        rewording a comment cannot silently reshape what this returns.
        """
        script = self.read(Path("scripts") / SCRIPT.name)
        found = re.search(r'grep -rqE "fn \$dead.*?<<\'EOF\'\n(.*?)\nEOF\n', script, re.S)
        if found is None:
            raise AssertionError(
                "could not locate invariant (4)'s retired-symbol heredoc in "
                + SCRIPT.name
                + " -- update Gate.retired() alongside the gate"
            )
        return [line for line in found.group(1).split("\n") if line.strip()]


def gate(fn):
    """Run `fn(g)` against a fresh throwaway repo."""

    def wrapper(self):
        with tempfile.TemporaryDirectory() as tmp:
            return fn(self, Gate(Path(tmp) / "repo"))

    return wrapper


class SkillDocGate(unittest.TestCase):
    @gate
    def test_clean_tree_passes(self, g: Gate) -> None:
        """The control. Without this, every refusal below could be vacuous."""
        self.assertEqual(
            g.run().returncode, 0, "gate must not red an unmodified tree"
        )

    @gate
    def test_every_anchor_row_is_discriminating(self, g: Gate) -> None:
        """The property the table exists to have.

        For each row, rename the declaration it names and assert the row stops
        matching. A row that survives documents a prefix, not a symbol -- it
        would report green after the symbol it guards was renamed away.

        Checked at the row level rather than by re-running the whole gate once
        per row: the claim is about each pattern's discrimination, and a full
        invocation per row costs minutes for identical signal.
        """
        survivors = []
        for pat, rel in g.rows():
            source = g.read(Path(rel))
            body = ere_body(pat)
            # Screened here rather than deferred to
            # test_row_bodies_carry_no_unescaped_ere_metacharacter: unittest
            # orders alphabetically, so that test runs AFTER this one, and an
            # offending row would surface as an opaque `re.error` from the
            # re.search below instead of its own clean message.
            self.assertFalse(
                unescaped_metacharacters(pat),
                "row " + repr(pat) + " carries an unescaped ERE metacharacter",
            )
            keywords, _, symbol = body.rpartition(" ")
            decl = re.compile(
                re.escape(keywords) + r"\s+" + re.escape(symbol) + r"\b"
            )
            self.assertRegex(
                source, decl, "row " + repr(pat) + " names no declaration in " + rel
            )
            mutated = decl.sub(keywords + " zzz_renamed_away", source)
            # `\b` is the only ERE construct rows use, and it means the same in
            # both dialects, so the gate's pattern doubles as a Python regex.
            if re.search(pat, mutated):
                survivors.append(pat + "\t" + rel)
        self.assertEqual(
            survivors,
            [],
            "these rows stayed green after their own symbol was renamed away, "
            "so they document a prefix rather than a symbol: " + repr(survivors),
        )

    @gate
    def test_row_bodies_carry_no_unescaped_ere_metacharacter(self, g: Gate) -> None:
        """Rows are EREs. A stray `(` is a regex error reported as doc drift.

        Covers invariant (4)'s retired list too: it became ERE-interpreted in
        the same change that anchored it, so it carries the same constraint.
        """
        offenders = [pat for pat, _ in g.rows() if unescaped_metacharacters(pat)]
        offenders += [d for d in g.retired() if unescaped_metacharacters(d)]
        self.assertEqual(
            offenders, [], "unescaped ERE metacharacters: " + repr(offenders)
        )

    @gate
    def test_retired_symbol_returning_is_caught(self, g: Gate) -> None:
        """Invariant (4)'s non-vacuity guard: a dead name that comes back."""
        f = Path("crates/engine/src/parser/oracle_util.rs")
        g.write(f, g.read(f) + "\npub fn parse_keyword_from_oracle() {}\n")
        self.assertEqual(
            g.run().returncode, 1, "a retired symbol returning must be caught"
        )

    @gate
    def test_longer_sibling_does_not_false_fire_retired_list(self, g: Gate) -> None:
        """...but a DIFFERENT, longer-named function must not trip it.

        Unanchored, `grep -rq "fn $dead"` matched `parse_keyword_from_oracle_v2`
        and redded a tree that was not stale.
        """
        f = Path("crates/engine/src/parser/oracle_util.rs")
        g.write(f, g.read(f) + "\npub fn parse_keyword_from_oracle_v2() {}\n")
        self.assertEqual(
            g.run().returncode,
            0,
            "a longer-named sibling must not fire the retired-list guard",
        )

    @gate
    def test_skill_citing_dead_symbol_is_caught(self, g: Gate) -> None:
        """Invariant (4)'s forward direction: the doc naming a removed symbol."""
        g.write(SKILL, g.read(SKILL) + "\nSee `extract_keyword_line()`.\n")
        self.assertEqual(
            g.run().returncode, 1, "a dead citation in SKILL.md must be caught"
        )

    @gate
    def test_skill_citing_longer_sibling_does_not_false_fire(self, g: Gate) -> None:
        """...but citing a longer name that is not the dead one must not."""
        g.write(SKILL, g.read(SKILL) + "\nSee `extract_keyword_line_v2()`.\n")
        self.assertEqual(
            g.run().returncode,
            0,
            "a longer-named citation must not fire the dead-cite guard",
        )

    @gate
    def test_missing_documented_path_is_caught(self, g: Gate) -> None:
        """Invariant (1): a documented file that no longer exists.

        Deletes a path that NO anchor row names. Deleting one that does (say
        `oracle_target.rs`, which carries two rows) exits 1 via invariant (2)
        whatever invariant (1) does, so the test would pass with (1) removed
        entirely. Asserting on stderr pins which invariant fired.
        """
        missing = "crates/engine/src/parser/oracle_nom/PATTERNS.md"
        self.assertNotIn(
            missing,
            [rel for _, rel in g.rows()],
            "fixture assumption broken: this path is now anchored, so the "
            "assertion below would no longer isolate invariant (1)",
        )
        (g.root / missing).unlink()
        result = g.run()
        self.assertEqual(result.returncode, 1, "a missing documented path must red")
        self.assertIn("documented path missing: " + missing, result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
