#!/usr/bin/env python3
"""Gate H: selected helper-result discard syntax, with reviewed per-file ceilings.

This is not a proof that unread Oracle text cannot be dropped. Named ignored
remainders are conservative obligations; defaults freeze syntax, not semantic
loss. Same-file deletion/addition can fit under a ceiling. No arbitrary dataflow,
unwrap_or_else, future Parsed API, or waiver enforcement is claimed here.
Expression-with-block statement boundaries are shared by closure tails and
comma-free match arms. Import aliases are lexical (module boundaries do not
inherit them); precise-capture use<...> bounds are not imports. Bare closures
are not helper results: only established result-forwarding methods unwrap them.
Binding annotations and ref/mut modifiers do not hide ignored remainders.
Let-else initializers stop before the top-level else, not its diverging block.
Each discarding top-level or-pattern alternative counts separately, including
parenthesized alternatives: splitting an arm preserves its obligation count.
Mixed alternatives count only the discarding alternatives, not the whole arm.

--list reports each path:line family symbol and scan totals; --baseline prints
sorted ledger rows (never writes); --check validates scripts/residue-ratchet.txt.
--index reads SOURCE AND LEDGER from git's index, using this executing tool.
Unexpected per-file scanner failures report the path and fail closed.
"""
from __future__ import annotations

import argparse
from bisect import bisect_right
from collections import Counter
from pathlib import Path, PurePosixPath
import posixpath
import re
import subprocess
import sys
from typing import NamedTuple

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from zone_authority_census import (
    CensusError, ScanState, strip_noncode, iter_production_lines, non_test_code,
)

SCOPE = "crates/engine/src/parser"
LEDGER = "scripts/residue-ratchet.txt"
HELPERS = frozenset(("parse_target", "parse_target_with_ctx", "parse_target_with_syntax",
                     "parse_target_disjunction", "parse_target_with_disjunctive_restriction",
                     "parse_fight_target", "parse_type_phrase_folding",
                     "parse_type_phrase_folding_with_ctx"))
# ParseContext methods verified to return `f(...)` unchanged (context.rs).
# Values are total argument counts; only the last argument is the closure.
RESULT_WRAPPERS = {"with_player_antecedents": 1, "with_player_scope": 2}
TOKEN = re.compile(r"[A-Za-z_][A-Za-z_0-9]*|[0-9]+|::|=>|[^\s]")


class Hit(NamedTuple):
    path: str
    line: int
    family: str
    symbol: str


def production_stream(relpath, lines):
    codes = [""] * len(lines)
    for i, _raw, code, _fn in iter_production_lines(relpath, lines, skip_cfg_test_items=True):
        codes[i] = code
    return "\n".join(codes)


class Expressions:
    """Balanced token views; follow result-preserving wrappers, not nested args."""
    def __init__(self, source):
        matches = list(TOKEN.finditer(source))
        self.offsets = [m.start() for m in matches]
        newlines = [m.start() for m in re.finditer("\n", source)]
        self.lines = [bisect_right(newlines, m.start()) + 1 for m in matches]
        self.values = [m.group() for m in matches]
        self.pairs = {}
        stack = []
        for i, value in enumerate(self.values):
            if value in ("(", "[", "{"):
                stack.append(i)
            elif value in (")", "]", "}"):
                if not stack or self.values[stack[-1]] != {")": "(", "]": "[", "}": "{"}[value]:
                    raise CensusError("unbalanced expression delimiters")
                opened = stack.pop()
                self.pairs[opened] = i
                self.pairs[i] = opened
        if stack:
            raise CensusError("unclosed expression delimiters")
        # Imports belong to lexical scopes, not the whole file. A module starts
        # a fresh name environment; ordinary blocks inherit their parent scope.
        v = self.values
        module_bodies = {i + 2 for i in range(len(v) - 2)
                         if v[i] == "mod" and re.fullmatch(r"\w+", v[i + 1])
                         and v[i + 2] == "{"}
        self.scopes = []
        self.parents = {-1: None}
        self.aliases = {-1: {}}
        scopes = [-1]
        for i, value in enumerate(v):
            self.scopes.append(scopes[-1])
            if value == "{":
                self.parents[i] = None if i in module_bodies else scopes[-1]
                self.aliases[i] = {}
                scopes.append(i)
            elif value == "}":
                scopes.pop()
        for i, value in enumerate(v):
            if value != "use" or i + 1 == len(v) or v[i + 1] == "<":
                continue  # `use<'a>` is a precise-capture bound, not an item.
            before = i - 1
            if before >= 0 and v[before] == ")":
                before = self.pairs[before] - 1
            if before >= 0 and v[before] == "pub":
                before -= 1
            if before >= 0 and v[before] not in (";", "{", "}", "]"):
                continue
            end = self.end(i + 1, {";"})
            if end == len(v):
                continue
            for j in range(i + 1, end - 2):
                if v[j + 1] == "as":
                    self.aliases[self.scopes[i]][v[j + 2]] = (
                        v[j] if v[j] in HELPERS else None)

    def helper_name(self, token):
        name = self.values[token]
        scope = self.scopes[token]
        while scope is not None:
            if name in self.aliases[scope]:
                return self.aliases[scope][name]
            scope = self.parents[scope]
        return name if name in HELPERS else None

    def split(self, start, end, separator=","):
        out = []
        part = start
        i = start
        while i < end:
            if self.values[i] in ("(", "[", "{"):
                i = self.pairs[i] + 1
                continue
            if self.values[i] == separator:
                out.append((part, i))
                part = i + 1
            i += 1
        if part < end:
            out.append((part, end))
        return out

    def end(self, start, stops, limit=None):
        i = start
        limit = len(self.values) if limit is None else limit
        while i < limit:
            if self.values[i] in stops:
                break
            if self.values[i] in ("(", "[", "{"):
                i = self.pairs[i] + 1
            else:
                i += 1
        return i

    def block_expression_end(self, start, end):
        """End of an expression-with-block, before any postfix/operator."""
        v = self.values
        if start >= end:
            return None
        head = start
        if v[head] == "'" and head + 3 < end and v[head + 2] == ":":
            head += 3
        if v[head] == "{":
            return self.pairs[head] + 1
        if v[head] not in ("if", "match", "unsafe", "loop", "while", "for", "async", "const"):
            return None
        opened = self.end(head + 1, {"{"}, end)
        if opened == end:
            return None
        after = self.pairs[opened] + 1
        if v[head] == "if" and after < end and v[after] == "else":
            return self.block_expression_end(after + 1, end)
        return after

    def block_tail(self, start, end):
        """Skip semicolon statements and comma-free expression-with-blocks."""
        while start < end:
            after = self.block_expression_end(start, end)
            if after is not None and after < end and self.values[after] not in (
                    ".", "?", "(", "[", "+", "-", "*", "/", "%", "as"):
                start = after
                continue
            stop = self.end(start, {";"}, end)
            if stop == end:
                return start, end
            start = stop + 1
        return end, end

    def match_patterns(self, start, end):
        """Arm patterns without guards; expressions-with-block need no comma."""
        v = self.values
        arm = start
        while arm < end:
            if v[arm] == ",":
                arm += 1
                continue
            arrow = self.end(arm, {"=>"}, end)
            if arrow == end:
                return
            pattern_end = self.end(arm, {"if"}, arrow)
            yield arm, pattern_end
            body = arrow + 1
            after = self.block_expression_end(body, end)
            if after is not None:
                arm = after
                # A postfix/operator makes this a larger expression, which
                # requires comma termination rather than the block shortcut.
                next_pattern = (arm < end and v[arm] in ("(", "[")
                                and self.pairs[arm] + 1 < end
                                and v[self.pairs[arm] + 1] in ("if", "=>"))
                if not next_pattern and arm < end and v[arm] in (".", "?", "(", "[", "+", "-", "*", "/", "%", "as"):
                    arm = self.end(arm, {","}, end)
            else:
                arm = self.end(body, {","}, end)

    def origin(self, start, end):
        """Return (helper token index, symbol) only for a helper RESULT."""
        v = self.values
        while start < end and v[end - 1] == "?":
            end -= 1
        while start < end and v[start] == "(" and self.pairs[start] == end - 1:
            start += 1
            end -= 1
        if start >= end:
            return None
        if v[start] == "{" and self.pairs[start] == end - 1:
            return self.origin(*self.block_tail(start + 1, end - 1))
        if v[end - 1] != ")":
            return None
        opened = self.pairs[end - 1]
        if opened <= start:
            return None
        name = v[opened - 1]
        path = v[start:opened]
        if path and path[0] == "::":
            path = path[1:]
        if not (len(path) % 2 == 1
                and all(re.fullmatch(r"\w+", x) for x in path[::2])
                and all(x in ("::", ".") for x in path[1::2])):
            return None
        helper = self.helper_name(opened - 1)
        if helper and "." not in path:
            return opened - 1, helper
        args = self.split(opened + 1, end - 1)
        if name in ("Some", "Ok") and len(args) == 1:
            a, b = args[0]
            if a < b and v[a] not in ("|", "move"):
                return self.origin(a, b)
        if "." in path and len(args) == RESULT_WRAPPERS.get(name):
            a, b = args[-1]
            closure = a + (a < b and v[a] == "move")
            if closure < b and v[closure] == "|":
                close = self.end(closure + 1, {"|"}, b)
                if close < b:
                    return self.origin(close + 1, b)
        return None

    def residue_pattern(self, start, end, arity=2):
        v = self.values
        while start < end:
            if v[start] == "(" and self.pairs[start] == end - 1:
                parts = self.split(start + 1, end - 1)
                rest = next((j for j, (a, b) in enumerate(parts)
                             if v[a:b] == [".", "."]), None)
                residue = 1
                if rest is not None:
                    suffix = len(parts) - rest - 1
                    if rest <= residue < arity - suffix:
                        return True
                    if residue >= arity - suffix:
                        residue = rest + 1 + residue - (arity - suffix)
                if len(parts) > residue:
                    a, b = parts[residue]
                    while a < b and v[a] in ("ref", "mut"):
                        a += 1
                    return b == a + 1 and v[a].startswith("_")
                start += 1
                end -= 1
            elif end > start + 2 and v[start] in ("Some", "Ok") and v[start + 1] == "(":
                start += 1
            else:
                return False
        return False

    def discard_patterns(self, start, end, arity=2):
        """Yield distinct discarding alternatives, preserving their token sites."""
        alternatives = self.split(start, end, "|")
        if len(alternatives) > 1:
            for a, b in alternatives:
                if a < b:  # A leading `|` is an empty prefix, not an obligation.
                    yield from self.discard_patterns(a, b, arity)
            return
        if self.residue_pattern(start, end, arity):
            yield start
            return
        v = self.values
        if start < end and v[start] in ("Some", "Ok"):
            start += 1
        if start < end and v[start] == "(" and self.pairs[start] == end - 1:
            # Only grouping/wrappers expose whole-result alternatives. Do not
            # confuse alternatives inside the filter component with tuple arms.
            if len(self.split(start + 1, end - 1)) == 1:
                yield from self.discard_patterns(start + 1, end - 1, arity)


def find_discards(relpath, lines):
    """Pure classification. No filesystem, git, baseline, or mutable global state."""
    ex = Expressions(production_stream(relpath, lines))
    v = ex.values
    hits = {}

    def add(origin, family, occurrence=None):
        if origin:
            i, symbol = origin
            # Overlapping recognizers share a result occurrence. Distinct arm
            # alternatives are distinct syntactic discards of that result.
            site = i if occurrence is None else occurrence
            hits.setdefault((i, site), Hit(relpath, ex.lines[site], family, symbol))

    for i, value in enumerate(v):
        if value == "let":
            equals = ex.end(i + 1, {"=", ";", "}"})
            if equals >= len(v) or v[equals] != "=":
                continue
            end = ex.end(equals + 1, {";", "{", "}", "else"})
            # A closure/block RHS needs its opening brace, unlike if-let's body.
            if end < len(v) and v[end] == "{" and end == equals + 1:
                end = ex.pairs[end] + 1
            origin = ex.origin(equals + 1, end)
            arity = 3 if origin and origin[1] == "parse_target_with_syntax" else 2
            pattern_end = ex.end(i + 1, {":"}, equals)
            patterns = list(ex.discard_patterns(i + 1, pattern_end, arity))
            for site in patterns:
                add(origin, "binding", site if len(patterns) > 1 else None)
            if not patterns and v[i + 1:pattern_end] == ["_"]:
                add(origin, "whole-result")
        if value == "match":
            end = ex.end(i + 1, {"{"})
            origin = ex.origin(i + 1, end)
            if origin and end < len(v):
                arity = 3 if origin[1] == "parse_target_with_syntax" else 2
                for a, b in ex.match_patterns(end + 1, ex.pairs[end]):
                    for site in ex.discard_patterns(a, b, arity):
                        add(origin, "binding", site)
        if value == "." and i + 1 < len(v) and v[i + 1] in ("0", "2"):
            # Walk backward over the receiver's balanced call/parentheses and
            # qualified/method name. Do not absorb an enclosing assignment.
            start = i - 1
            if start < 0 or v[start] != ")":
                continue
            start = ex.pairs[start]
            if start > 0 and re.fullmatch(r"\w+", v[start - 1]) and v[start - 1] != "return":
                start -= 1
                while start >= 2 and v[start - 1] in ("::", ".") and re.fullmatch(r"\w+", v[start - 2]):
                    start -= 2
            origin = ex.origin(start, i)
            if origin and (v[i + 1] == "0" or origin[1] == "parse_target_with_syntax"):
                add(origin, "projection")
        if value == "drop" and i + 1 < len(v) and v[i + 1] == "(":
            add(ex.origin(i + 2, ex.pairs[i + 1]), "whole-result")
        if value == ";":
            start = i - 1
            while start >= 0 and v[start] not in (";", "{", "}"):
                if v[start] in (")", "]"):
                    start = ex.pairs[start]
                start -= 1
            add(ex.origin(start + 1, i), "whole-result")
        if value == "unwrap_or" and i > 0 and v[i - 1] == "." and i + 1 < len(v) and v[i + 1] == "(":
            end = ex.pairs[i + 1]
            arg = v[i + 2:end]
            if len(arg) == 3 and arg[:2] == ["TargetFilter", "::"] and arg[2] in ("SelfRef", "Any"):
                hits[i] = Hit(relpath, ex.lines[i], "default", "TargetFilter::" + arg[2])
    return sorted(hits.values())


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args])


def read_snapshot(root, path, index):
    if index:
        return git(root, "show", ":" + path).decode("utf-8")
    return (root / path).read_text(encoding="utf-8")


def scan_file(path, operation, lines):
    """Keep internal scanner failures actionable at both per-file entrypoints."""
    try:
        return operation(path, lines)
    except CensusError as error:
        raise CensusError(f"{path}: {error}") from error
    except Exception as error:
        raise CensusError(
            f"detector internal error at {path}: {type(error).__name__}: {error}") from error


def module_edges(path, lines):
    """Outlined edges with physical paths and local production availability.

    Compare declarations at the same code offsets, not by module name. Inline
    modules contribute their directory. Attribute literals are inspected only
    after the shared lexer identifies a real `path` attribute.
    """
    state = ScanState()
    codes = []
    for line in lines:
        code, state = strip_noncode(line, state)
        codes.append(code)
    source = "\n".join(codes)
    production = "\n".join(non_test_code(path, lines))
    live_modules = {m.start() for m in re.finditer(r"\bmod\s+\w+\s*[;{]", production)}
    inner_test = bool(re.search(r"#\s*!\s*\[\s*cfg\s*\(", source) and not production.strip())
    ex = Expressions(source)
    v = ex.values
    parent = PurePosixPath(path)
    directory = parent.parent if parent.name in ("mod.rs", "lib.rs", "main.rs") else parent.with_suffix("")

    def path_attribute(mod):
        before = mod
        if before and v[before - 1] == ")":
            opened = ex.pairs[before - 1]
            if opened and v[opened - 1] == "pub":
                before = opened - 1
        elif before and v[before - 1] == "pub":
            before -= 1
        while before and v[before - 1] == "]":
            end = before - 1
            opened = ex.pairs[end]
            if not opened or v[opened - 1] != "#":
                break
            if v[opened + 1:opened + 3] == ["path", "="]:
                raw = "\n".join(lines[ex.lines[opened - 1] - 1:ex.lines[end]])
                paths = re.findall(r'#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]', raw)
                # Ambiguous same-line path attributes stay conservative: do not
                # infer a test-only physical target from an uncertain literal.
                return paths[0] if len(paths) == 1 else None
            before = opened - 1
        return None

    edges = []

    def walk(start, end, module_dir, attr_dir, parent_live):
        n = start
        while n + 2 < end:
            if v[n] != "mod" or not re.fullmatch(r"\w+", v[n + 1]) or v[n + 2] not in (";", "{"):
                n += 1
                continue
            name = v[n + 1]
            live = parent_live and ex.offsets[n] in live_modules
            attr = path_attribute(n)
            if v[n + 2] == "{":
                close = ex.pairs[n + 2]
                child_dir = attr_dir / attr if attr is not None else module_dir / name
                walk(n + 3, close, child_dir, child_dir, live)
                n = close + 1
            else:
                targets = ([attr_dir / attr] if attr is not None else
                           [module_dir / (name + ".rs"), module_dir / name / "mod.rs"])
                edges.extend((posixpath.normpath(target.as_posix()), live) for target in targets)
                n += 3

    walk(0, len(v), directory, parent.parent, not inner_test)
    return edges, inner_test


def production_sources(root, index):
    if index:
        paths = [p.decode() for p in git(root, "ls-files", "-z", "--", SCOPE).split(b"\0") if p]
    else:
        scope = root / SCOPE
        if not scope.is_dir():
            raise ValueError(f"missing parser root: {scope}")
        paths = [p.relative_to(root).as_posix() for p in scope.rglob("*.rs")]
    sources = {p: read_snapshot(root, p, index) for p in sorted(paths)
               if p.endswith(".rs") and "snapshots" not in PurePosixPath(p).parts
               and "tests" not in PurePosixPath(p).name}
    if not sources:
        raise ValueError("empty parser scope; cannot establish a zero-obligation baseline")
    edges = {}
    test_files = set()
    for path, text in sources.items():
        declared, inner_test = scan_file(path, module_edges, text.splitlines())
        edges[path] = [(target, live) for target, live in declared if target in sources]
        if inner_test:
            test_files.add(path)

    def reachable(seeds, production_only):
        seen = set(seeds)
        pending = list(seen)
        while pending:
            for target, live in edges.get(pending.pop(), ()):
                if production_only and (not live or target in test_files):
                    continue
                if target not in seen:
                    seen.add(target)
                    pending.append(target)
        return seen

    test_targets = test_files | {target for declared in edges.values() for target, live in declared if not live}
    test_reachable = reachable(test_targets, False)
    all_targets = {target for declared in edges.values() for target, _live in declared}
    # Orphan files are conservatively production. A production path to a shared
    # physical file wins over every test-only alias, including descendant edges.
    roots = (sources.keys() - test_reachable) | (sources.keys() - all_targets - test_files)
    production = reachable(roots, True)
    excluded = test_reachable - production
    return {p: text for p, text in sources.items() if p not in excluded}


def parse_ledger(text):
    ceilings = {}
    for n, raw in enumerate(text.splitlines(), 1):
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        fields = raw.split()
        if len(fields) != 2 or not re.fullmatch(r"[0-9]+", fields[1]):
            raise ValueError(f"malformed ledger line {n}: {raw}")
        path = fields[0].replace("\\", "/")
        parts = path.split("/")
        if (not path.startswith(SCOPE + "/") or not path.endswith(".rs")
                or any(p in ("", ".", "..") for p in parts) or ":" in path):
            raise ValueError(f"invalid ledger path at line {n}: {path}")
        if path in ceilings:
            raise ValueError(f"duplicate ledger entry for '{path}'")
        ceilings[path] = int(fields[1])
    return ceilings


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group(required=True)
    for mode in ("check", "list", "baseline"):
        modes.add_argument("--" + mode, action="store_true")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--index", action="store_true")
    args = parser.parse_args()
    try:
        sources = production_sources(args.root, args.index)
        if not sources:
            raise ValueError("empty production parser scope")
        found = [h for p, text in sources.items()
                 for h in scan_file(p, find_discards, text.splitlines())]
        counts = Counter(h.path for h in found)
        if args.baseline:
            for path, count in sorted(counts.items()):
                print(f"{path} {count}")
        elif args.list:
            for h in found:
                print(f"{h.path}:{h.line}: {h.family} {h.symbol}")
            print(f"TOTAL {len(found)} obligations; files scanned {len(sources)}; "
                  f"snapshot {'index' if args.index else 'worktree'}")
            for family, count in sorted(Counter(h.family for h in found).items()):
                print(f"FAMILY {family} {count}")
        else:
            ceilings = parse_ledger(read_snapshot(args.root, LEDGER, args.index))
            failed = False
            for path in sorted(counts.keys() | ceilings.keys()):
                actual, ceiling = counts[path], ceilings.get(path, 0)
                if actual > ceiling:
                    print(f"Gate H FAIL: {path}: actual {actual}, ceiling {ceiling}"
                          + (" (no ledger entry)" if path not in ceilings else ""))
                    for h in found:
                        if h.path == path:
                            print(f"  {h.path}:{h.line}: {h.family} {h.symbol}")
                    failed = True
                elif actual < ceiling:
                    print(f"residue-ratchet: tighten {path} {ceiling} -> {actual}"
                          + (" (entry can be deleted)" if actual == 0 else ""))
            if failed:
                return 1
            print(f"Gate H PASS (residue: {len(found)} legacy obligations, ceiling {sum(ceilings.values())}; "
                  f"files scanned {len(sources)}; snapshot {'index' if args.index else 'worktree'})")
    except (ValueError, OSError, CensusError, subprocess.CalledProcessError) as error:
        print(f"Gate H FAIL: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
