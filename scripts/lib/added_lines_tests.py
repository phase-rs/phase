#!/usr/bin/env python3
"""Controls for scripts/lib/added-lines.py, the diff-based gates' added-line authority.

The helper decides which added lines a gate checks. A regression in it does not
fail a gate: it silently exempts real violations or re-flags moved code. So the
gates run this suite before they trust the helper (scripts/lib/added-lines.sh).

Every test builds a throwaway git repository, drives the helper through its CLI
exactly as a gate does, and asserts the exact records it prints. Each move-shape
negative also plants one genuinely new line elsewhere in the same diff and
asserts the output is exactly that record: a helper that printed nothing would
fail every negative, so none of them can pass vacuously.

Run:  python3 scripts/lib/added_lines_tests.py
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HELPER = Path(__file__).with_name("added-lines.py")

ENV = {
    **os.environ,
    "GIT_AUTHOR_NAME": "added-lines tests",
    "GIT_AUTHOR_EMAIL": "added-lines-tests@example.invalid",
    "GIT_COMMITTER_NAME": "added-lines tests",
    "GIT_COMMITTER_EMAIL": "added-lines-tests@example.invalid",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_NOSYSTEM": "1",
}
# A hook runs this suite with git's environment exported, and in a linked
# worktree GIT_DIR / GIT_INDEX_FILE are absolute: inherited, the fixture repo's
# `git add` would overwrite the OUTER commit's index (see the same unset in
# scripts/check-cr-citation-anchors.sh).
# `git -c k=v commit` exports GIT_CONFIG_PARAMETERS / GIT_CONFIG_COUNT to the
# hook, which would otherwise reach every fixture git call.
# The suite asserts git's default pathspec semantics (`:(exclude)**/tests.rs` is
# a glob), so the pathspec-semantics variables are removed too.
for _var in (
    "GIT_INDEX_FILE", "GIT_DIR", "GIT_WORK_TREE", "GIT_PREFIX",
    "GIT_CONFIG_PARAMETERS", "GIT_CONFIG_COUNT",
    "GIT_LITERAL_PATHSPECS", "GIT_GLOB_PATHSPECS", "GIT_NOGLOB_PATHSPECS", "GIT_ICASE_PATHSPECS",
):
    ENV.pop(_var, None)

VIEW = ["src/parser", ":(exclude)**/tests.rs"]

# A forbidden-shaped line, and a line holding a form feed: one line to git, two
# to str.splitlines().
BAD = '    if text.strip_prefix("when ").is_some() {'
FF = 'let ff = "a\x0cb";'
GUARD = 'fn guard(t: &str) -> bool { t.contains("guard") }'

Record = tuple[str, int, str]


def fn(name: str, body_lines: int = 6) -> list[str]:
    """A function holding BAD, so moving it moves a forbidden line."""
    return [f"fn {name}() {{", BAD, *(f"    let {name}_{i} = {i};" for i in range(body_lines)), "}"]


class Repo:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.git("init", "-q")

    def git(self, *args: str) -> str:
        return subprocess.run(
            ["git", *args], cwd=self.root, env=ENV, check=True, capture_output=True, text=True
        ).stdout

    def write(self, path: str, lines: list[str]) -> None:
        p = self.root / path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text("".join(f"{line}\n" for line in lines))

    def remove(self, path: str) -> None:
        (self.root / path).unlink()

    def commit(self) -> str:
        self.git("add", "-A")
        self.git("commit", "-q", "--no-verify", "--allow-empty", "-m", "c")
        return self.git("rev-parse", "HEAD").strip()

    def run_helper(
        self, base: str, *mode: str, env: dict[str, str] = ENV, check: bool = False
    ) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(HELPER), *mode, base, "--", *VIEW],
            cwd=self.root, env=env, check=check, capture_output=True, text=True,
        )

    def added(self, base: str, *mode: str, env: dict[str, str] = ENV) -> list[Record]:
        """The helper's records; a non-zero exit fails the calling test."""
        proc = self.run_helper(base, *mode, env=env, check=True)
        # git's line model is "\n" only; str.splitlines() would also split on \x0c.
        records = []
        for line in proc.stdout.split("\n"):
            if line:
                path, n, content = line.split("\t", 2)
                records.append((path, int(n), content))
        return records


class AddedLinesTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._tmp.name))

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def plant_guard(self) -> Record:
        """The paired reach guard: one genuinely new line in another in-view file."""
        self.repo.write("src/parser/guard.rs", [GUARD])
        return ("src/parser/guard.rs", 1, GUARD)

    # -- the move rule ------------------------------------------------------

    def test_new_line_in_renamed_file_is_added(self) -> None:
        r = self.repo
        r.write("src/parser/a.rs", fn("a"))
        base = r.commit()
        new_line = 'fn n(t: &str) { t.split_once(" "); }'
        r.remove("src/parser/a.rs")
        r.write("src/parser/moved/a.rs", fn("a") + [new_line])
        r.commit()
        self.assertEqual(r.added(base), [("src/parser/moved/a.rs", 10, new_line)])

    def test_whole_file_rename_is_not_added(self) -> None:
        r = self.repo
        r.write("src/parser/a.rs", fn("a", 40))
        base = r.commit()
        r.remove("src/parser/a.rs")
        r.write("src/parser/b.rs", fn("a", 40))
        guard = self.plant_guard()
        r.commit()
        self.assertEqual(r.added(base), [guard])

    def test_small_half_split_into_new_file_is_not_added(self) -> None:
        # The shape `-C` copy detection misses: a small half leaving a large file.
        r = self.repo
        r.write("src/parser/mixed.rs", fn("keep", 900) + fn("lower", 8))
        base = r.commit()
        r.write("src/parser/mixed.rs", fn("keep", 900))
        r.write("src/parser/lower/mixed.rs", fn("lower", 8))
        guard = self.plant_guard()
        r.commit()
        self.assertEqual(r.added(base), [guard])

    def test_function_moved_between_surviving_files_is_not_added(self) -> None:
        r = self.repo
        r.write("src/parser/a.rs", fn("a") + fn("m"))
        r.write("src/parser/b.rs", fn("b"))
        base = r.commit()
        r.write("src/parser/a.rs", fn("a"))
        r.write("src/parser/b.rs", fn("b") + fn("m"))
        guard = self.plant_guard()
        r.commit()
        self.assertEqual(r.added(base), [guard])

    def test_function_moved_into_new_file_is_not_added(self) -> None:
        r = self.repo
        r.write("src/parser/a.rs", fn("a") + fn("m"))
        base = r.commit()
        r.write("src/parser/a.rs", fn("a"))
        r.write("src/parser/m.rs", fn("m"))
        guard = self.plant_guard()
        r.commit()
        self.assertEqual(r.added(base), [guard])

    def test_reindented_move_is_not_added(self) -> None:
        # Content is whitespace-trimmed, so wrapping moved code in `mod m { … }`
        # leaves only the two wrapper lines added: `mod m {` and the one closing
        # `}` that no deleted `}` accounts for.
        r = self.repo
        r.write("src/parser/a.rs", fn("a") + fn("m"))
        r.write("src/parser/b.rs", fn("b"))
        base = r.commit()
        r.write("src/parser/a.rs", fn("a"))
        r.write("src/parser/b.rs", fn("b") + ["mod m {"] + [f"    {line}" for line in fn("m")] + ["}"])
        guard = self.plant_guard()
        r.commit()
        self.assertEqual(
            r.added(base),
            [("src/parser/b.rs", 10, "mod m {"), ("src/parser/b.rs", 20, "}"), guard],
        )

    def test_deleted_once_added_twice_is_added_once(self) -> None:
        r = self.repo
        r.write("src/parser/a.rs", ["fn a() {", BAD, "}"])
        r.write("src/parser/b.rs", ["fn b() {", "}"])
        r.write("src/parser/c.rs", ["fn c() {", "}"])
        base = r.commit()
        r.write("src/parser/a.rs", ["fn a() {", "}"])
        r.write("src/parser/b.rs", ["fn b() {", BAD, "}"])
        r.write("src/parser/c.rs", ["fn c() {", BAD, "}"])
        r.commit()
        self.assertEqual(r.added(base), [("src/parser/c.rs", 2, BAD)])

    # -- the view -----------------------------------------------------------

    def test_line_moved_in_from_outside_the_scope_is_added(self) -> None:
        r = self.repo
        r.write("src/types/t.rs", ["fn t() {", BAD, "}"])
        r.write("src/parser/a.rs", ["fn a() {", "}"])
        base = r.commit()
        r.write("src/types/t.rs", ["fn t() {", "}"])
        r.write("src/parser/a.rs", ["fn a() {", BAD, "}"])
        r.commit()
        self.assertEqual(r.added(base), [("src/parser/a.rs", 2, BAD)])

    def test_line_moved_in_from_an_excluded_path_is_added(self) -> None:
        r = self.repo
        r.write("src/parser/x/tests.rs", ["fn t() {", BAD, "}"])
        r.write("src/parser/a.rs", ["fn a() {", "}"])
        base = r.commit()
        r.write("src/parser/x/tests.rs", ["fn t() {", "}"])
        r.write("src/parser/a.rs", ["fn a() {", BAD, "}"])
        r.commit()
        self.assertEqual(r.added(base), [("src/parser/a.rs", 2, BAD)])

    # -- input modes --------------------------------------------------------

    def test_cached_reads_the_index_not_the_working_tree(self) -> None:
        r = self.repo
        r.write("src/parser/a.rs", fn("a"))
        base = r.commit()
        r.remove("src/parser/a.rs")
        r.write("src/parser/b.rs", fn("a"))
        r.git("add", "-A")
        r.write("src/parser/b.rs", fn("a") + [BAD])
        self.assertEqual(r.added(base, "--cached"), [])
        self.assertEqual(r.added(base), [("src/parser/b.rs", 10, BAD)])

    def test_include_untracked_pools_against_tracked_deletions(self) -> None:
        r = self.repo
        r.write("src/parser/a.rs", fn("a") + fn("m"))
        base = r.commit()
        r.write("src/parser/a.rs", fn("a"))
        r.write("src/parser/m.rs", fn("m") + [BAD, FF])
        self.assertEqual(
            r.added(base, "--include-untracked"),
            [("src/parser/m.rs", 10, BAD), ("src/parser/m.rs", 11, FF)],
        )
        self.assertEqual(r.added(base), [])

    # -- diff grammar -------------------------------------------------------

    def test_ordinary_edit_reports_every_added_line_with_its_number(self) -> None:
        r = self.repo
        lines = [f"let x{i} = {i};" for i in range(10)]
        r.write("src/parser/e.rs", lines)
        base = r.commit()
        # Insert three lines after line 3 (one blank, one starting with `+`),
        # replace the old line 6, append one.
        r.write(
            "src/parser/e.rs",
            lines[:3] + ["fn n() {", "", "+1/+1 counter"] + lines[3:5] + ["let x5 = 50;"] + lines[6:] + ["// tail"],
        )
        r.commit()
        self.assertEqual(
            r.added(base),
            [
                ("src/parser/e.rs", 4, "fn n() {"),
                ("src/parser/e.rs", 5, ""),
                ("src/parser/e.rs", 6, "+1/+1 counter"),
                ("src/parser/e.rs", 9, "let x5 = 50;"),
                ("src/parser/e.rs", 14, "// tail"),
            ],
        )

    def test_copy_detection_config_cannot_hide_a_copy(self) -> None:
        # A copy duplicates code; its source lines were not deleted, so every
        # line of the copy is added whatever rename/copy detection is configured.
        r = self.repo
        r.git("config", "diff.renames", "copies")
        r.write("src/parser/a.rs", fn("a", 20))
        base = r.commit()
        r.write("src/parser/a.rs", fn("a", 20) + ["// edited"])
        r.write("src/parser/copy.rs", fn("a", 20))
        r.commit()
        self.assertEqual(
            r.added(base),
            [("src/parser/a.rs", 24, "// edited")]
            + [("src/parser/copy.rs", n, line) for n, line in enumerate(fn("a", 20), start=1)],
        )

    def test_header_path_with_a_space(self) -> None:
        # git ends such a header path with a TAB, which must not reach the record.
        r = self.repo
        r.write("src/parser/my file.rs", ["fn a() {", "}"])
        base = r.commit()
        r.write("src/parser/my file.rs", ["fn a() {", BAD, "}"])
        r.commit()
        self.assertEqual(r.added(base), [("src/parser/my file.rs", 2, BAD)])

    def test_inter_hunk_context_config_cannot_merge_hunks(self) -> None:
        r = self.repo
        r.git("config", "diff.interHunkContext", "5")
        lines = [f"let x{i} = {i};" for i in range(8)]
        r.write("src/parser/e.rs", lines)
        base = r.commit()
        r.write("src/parser/e.rs", lines[:2] + ["let y2 = 2;"] + lines[3:4] + ["let y4 = 4;"] + lines[5:])
        r.commit()
        self.assertEqual(
            r.added(base),
            [("src/parser/e.rs", 3, "let y2 = 2;"), ("src/parser/e.rs", 5, "let y4 = 4;")],
        )

    def test_git_diff_opts_cannot_widen_hunks(self) -> None:
        # GIT_DIFF_OPTS overrides -U on git's command line, so the helper must
        # drop it. Added for this one helper call; the suite's ENV does not
        # remove it, so an outer GIT_DIFF_OPTS exercises every test too.
        r = self.repo
        lines = [f"let x{i} = {i};" for i in range(12)]
        r.write("src/parser/e.rs", lines)
        base = r.commit()
        r.write("src/parser/e.rs", lines[:2] + ["let y2 = 2;"] + lines[3:9] + ["let y9 = 9;"] + lines[10:])
        r.commit()
        self.assertEqual(
            r.added(base, env={**ENV, "GIT_DIFF_OPTS": "--unified=3"}),
            [("src/parser/e.rs", 3, "let y2 = 2;"), ("src/parser/e.rs", 10, "let y9 = 9;")],
        )

    def test_textconv_config_cannot_rewrite_lines(self) -> None:
        # A user-level attributes file can map `*.rs` to a textconv driver, and
        # porcelain `git diff` applies it by default: every line would be matched
        # as the driver's output, not as the source.
        r = self.repo
        attributes = r.root / ".git" / "textconv-attributes"
        attributes.write_text("*.rs diff=rewrite\n")
        r.git("config", "core.attributesFile", str(attributes))
        r.git("config", "diff.rewrite.textconv", "sed 's/^/rewritten: /'")
        r.write("src/parser/e.rs", ["fn a() {", "}"])
        base = r.commit()
        r.write("src/parser/e.rs", ["fn a() {", BAD, "}"])
        r.commit()
        self.assertEqual(r.added(base), [("src/parser/e.rs", 2, BAD)])

    # -- failure ------------------------------------------------------------

    def test_unknown_base_fails_closed(self) -> None:
        r = self.repo
        r.write("src/parser/a.rs", fn("a"))
        r.commit()
        proc = r.run_helper("nosuchref")
        self.assertEqual(proc.returncode, 2, proc.stderr)
        self.assertEqual(proc.stdout, "")


if __name__ == "__main__":
    unittest.main()
