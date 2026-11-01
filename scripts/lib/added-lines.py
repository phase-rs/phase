#!/usr/bin/env python3
"""The genuinely-added lines of a diff, over one gate's view.

The diff-based gates (check-parser-combinators.sh, check-engine-authorities.sh,
check-test-card-data-load.sh, check-legacy-quantity-callsites.sh) flag only
*newly added* offending lines. A line moved verbatim is not new, so this helper
is the one authority for which added lines count:

    For a given gate, an added line is *moved* iff its whitespace-trimmed content
    matches a deleted line whose pre-image path lies in that gate's scope list,
    after its exclusions. Matching is a multiset over the whole range: each
    deleted line exempts at most one added line. A line entering a gate's view
    from outside it is checked.

The view is the gate's pathspec list (scope plus `:(exclude)` entries), passed
verbatim to a single `git diff`, so every deleted line read here already has an
in-view pre-image path. The rule ignores file shape: whole-file renames, splits,
and function moves between files all pool across every file in the view.

Output: one `<path>\\t<post-image line>\\t<content>\\n` record per genuinely-added
line, in diff order, and exit 0. An empty output is a real answer. On any failure
(git error, unparseable diff, undecodable bytes, unreadable file) it prints
`added-lines: <reason>` to stderr, writes nothing to stdout, and exits 2.

Usage (from the repository root, like every gate):

    python3 scripts/lib/added-lines.py [--cached | --include-untracked] <base> -- <pathspec>...

Without a mode flag the working tree is compared with <base>; `--cached` compares
the index (the pre-commit hook's window); `--include-untracked` also reads every
untracked file in the view as added lines, for a caller that never stages.
"""

from __future__ import annotations

import argparse
import collections
import os
import re
import subprocess
import sys
from pathlib import Path

HUNK_RE = re.compile(r"^@@ -\d+(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


class DiffError(Exception):
    """The diff text does not have the shape this reader pins."""


def header_path(line: str, prefix: str) -> str | None:
    """The path of a `--- a/<p>` / `+++ b/<p>` header, or None for /dev/null."""
    rest = line[4:]
    if rest == "/dev/null":
        return None
    if rest.startswith('"'):
        raise DiffError(f"quoted path not supported: {line!r}")
    if not rest.startswith(prefix):
        raise DiffError(f"unexpected header: {line!r}")
    # git appends one TAB to a header path that contains a space. A name that
    # itself ends in a TAB is C-quoted, which the check above already refuses.
    return rest[len(prefix):].removesuffix("\t")


def parse_diff(text: str) -> tuple[list[str], list[tuple[str, int, str]]]:
    """Read a unified=0 diff into (deleted contents, added (path, line, content)).

    Each hunk is read by its `@@ -a,b +c,d @@` counts, so a body line can never
    be misread as a header. Line numbers are post-image and advance per `+` line.
    """
    deleted: list[str] = []
    added: list[tuple[str, int, str]] = []
    old_path = new_path = None
    lines = text.split("\n")
    i = 0
    while i < len(lines):
        line = lines[i]
        i += 1
        if line.startswith("diff --git "):
            old_path = new_path = None
            continue
        if line.startswith("--- "):
            old_path = header_path(line, "a/")
            continue
        if line.startswith("+++ "):
            new_path = header_path(line, "b/")
            continue
        m = HUNK_RE.match(line)
        if not m:
            # `index …`, `new file mode …`, `Binary files … differ`, …
            continue
        dels = int(m.group(1)) if m.group(1) is not None else 1
        lineno = int(m.group(2))
        adds = int(m.group(3)) if m.group(3) is not None else 1
        while dels or adds:
            if i >= len(lines):
                raise DiffError("truncated hunk")
            body = lines[i]
            i += 1
            if body.startswith("\\"):
                # `\ No newline at end of file`
                continue
            if body.startswith("-") and dels:
                if old_path is None:
                    raise DiffError("deletion without a pre-image path")
                deleted.append(body[1:])
                dels -= 1
            elif body.startswith("+") and adds:
                added.append((new_path, lineno, body[1:]))
                lineno += 1
                adds -= 1
            else:
                raise DiffError(f"hunk body out of step: {body!r}")
    return deleted, added


def untracked_added(pathspecs: list[str]) -> list[tuple[str, int, str]]:
    """Every line of every untracked file in the view, as added records."""
    listing = subprocess.run(
        ["git", "ls-files", "--others", "--exclude-standard", "-z", "--", *pathspecs],
        check=True,
        capture_output=True,
    ).stdout.decode("utf-8")
    out: list[tuple[str, int, str]] = []
    for name in filter(None, listing.split("\0")):
        data = Path(name).read_bytes()
        if b"\0" in data:
            # Binary, by the same rule as `grep -I`.
            continue
        lines = data.decode("utf-8").split("\n")
        if lines[-1] == "":
            lines.pop()
        out.extend((name, n, content) for n, content in enumerate(lines, start=1))
    return out


def genuinely_added(
    deleted: list[str], added: list[tuple[str, int, str]]
) -> list[tuple[str, int, str]]:
    """The added records that no deleted line accounts for, in diff order.

    Each deleted line's whitespace-trimmed content exempts at most one added line
    with the same trimmed content, so re-indented code counts as moved. Residual:
    a net-zero line, deleted at one in-scope site and re-added at another, stays
    exempt.
    """
    pool = collections.Counter(c.strip() for c in deleted)
    out = []
    for record in added:
        key = record[2].strip()
        if pool[key] > 0:
            pool[key] -= 1
        else:
            out.append(record)
    return out


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        prog="added-lines.py",
        description="The genuinely-added lines of a diff, over one gate's view.",
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--cached", action="store_true", help="compare the index with <base>")
    mode.add_argument(
        "--include-untracked",
        action="store_true",
        help="also read untracked files in the view as added lines",
    )
    parser.add_argument("base")
    parser.add_argument("pathspec", nargs="+")
    args = parser.parse_args(argv)

    # Each flag pins the diff grammar against user config:
    #   --src-prefix/--dst-prefix  diff.noprefix, diff.mnemonicPrefix
    #   --inter-hunk-context=0     diff.interHunkContext (merged hunks carry context lines)
    #   --no-renames               diff.renames=copies (a copy would hide its added lines)
    #   --no-color, --no-ext-diff  color.*, diff.external, GIT_EXTERNAL_DIFF
    #   --no-textconv              diff.<driver>.textconv (rewrites both sides' lines)
    # GIT_DIFF_OPTS is dropped from the env instead: git lets it override -U.
    cmd = [
        "git", "diff", "--no-color", "--no-ext-diff", "--no-textconv", "--no-renames", "--unified=0",
        "--inter-hunk-context=0", "--src-prefix=a/", "--dst-prefix=b/",
    ]
    if args.cached:
        cmd.append("--cached")
    cmd += [args.base, "--", *args.pathspec]
    # Every other variable is inherited, GIT_INDEX_FILE included: it is what
    # makes `--cached` read the commit a pre-commit hook is creating.
    env = {k: v for k, v in os.environ.items() if k != "GIT_DIFF_OPTS"}
    try:
        proc = subprocess.run(cmd, capture_output=True, env=env)
        if proc.returncode != 0:
            raise DiffError(proc.stderr.decode("utf-8", errors="replace").strip())
        deleted, added = parse_diff(proc.stdout.decode("utf-8"))
        if args.include_untracked:
            added += untracked_added(args.pathspec)
    except (DiffError, subprocess.CalledProcessError, UnicodeDecodeError, OSError) as e:
        print(f"added-lines: {e}", file=sys.stderr)
        return 2
    sys.stdout.write("".join(f"{p}\t{n}\t{c}\n" for p, n, c in genuinely_added(deleted, added)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
