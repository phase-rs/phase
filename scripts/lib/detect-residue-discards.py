#!/usr/bin/env python3
"""Gate H: selected helper-result discard syntax, with reviewed per-file ceilings.

This is not a proof that unread Oracle text cannot be dropped. Named ignored
remainders are conservative obligations; defaults freeze syntax, not semantic
loss. Same-file deletion/addition can fit under a ceiling. No arbitrary dataflow,
unwrap_or_else, future Parsed API, or waiver enforcement is claimed here.
Expression-with-block statement boundaries are shared by closure tails and
comma-free match arms. Import aliases are lexical (module boundaries do not
inherit them) and apply only to unqualified calls; qualified calls use the final
identifier's own name. Precise-capture use<...> bounds are not imports. Bare
closures are not helper results: only established result-forwarding methods unwrap them.
Local bindings shadowing imported aliases are not tracked; helper import aliases
do not occur in the current production parser tree.
Binding annotations and ref/mut modifiers do not hide ignored remainders or
whole results: a single `_`/`_name` pattern counts one whole-result obligation,
even if later used, consistently with conservative residue-binding policy.
Let-else initializers stop before the top-level else, not its diverging block.
Block initializers include postfixes before result classification; trailing `?`
preserves helper origin, but arbitrary methods and tuple projections do not.
Projection receivers include standalone bare blocks, including return/break
operands, not keyword/header bodies or struct literals; origin follows the tail.
Not modelled: a bare-block projection receiver after `&mut`, `>` or `>>`, or as
an if/while/match scrutinee (none occurs in the parser tree); parenthesize it.
Each discarding top-level or-pattern alternative counts separately, including
parenthesized alternatives: splitting an arm preserves its obligation count.
Mixed alternatives count only the discarding alternatives, not the whole arm.
Argument and wrapper-pattern splitting accepts an optional trailing comma.
Range-endpoint projections count; free `drop` calls accept explicit generic
arguments; single-dot `.drop` methods never classify their helper arguments.
Registered result wrappers and their receiver segments accept turbofish arguments,
including nested types and function arrows; free-helper generic calls are unchanged.
Semicolon-terminated bare statement blocks discard helper tails, not header
blocks or let initializers (which retain their binding classification).
Discovery excludes exactly tests.rs and *_tests.rs, snapshots, and modules
proven test-only; a filename merely containing `tests` remains production.

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
EXPRESSION_PREFIXES = frozenset(("return", "break"))
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


def follows_expression_prefix(values, start):
    """Return/break take operands; a break label is not a block owner or callee."""
    if start > 0 and values[start - 1] in EXPRESSION_PREFIXES:
        return True
    return (start >= 3 and values[start - 3] == "break"
            and values[start - 2] == "'"
            and re.fullmatch(r"\w+", values[start - 1]) is not None)


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
        if token > 0 and self.values[token - 1] == "::":
            return name if name in HELPERS else None
        scope = self.scopes[token]
        while scope is not None:
            if name in self.aliases[scope]:
                return self.aliases[scope][name]
            scope = self.parents[scope]
        return name if name in HELPERS else None

    def turbofish_end(self, i):
        """Index after the `::<...>` generic-argument list starting at i, else None.

        Generic-argument commas are not expression separators, so `end` and `split`
        step over the balanced list. A turbofish is only `::` followed by `<`; a
        comparison never is, and the `>` of a `->` inside the list does not close it.
        """
        v = self.values
        if v[i] != "::" or i + 1 >= len(v) or v[i + 1] != "<":
            return None
        depth = 0
        j = i + 1
        while j < len(v):
            if v[j] in ("(", "[", "{"):
                j = self.pairs[j] + 1
                continue
            if v[j] == "<":
                depth += 1
            elif v[j] == ">" and v[j - 1] != "-":
                depth -= 1
                if depth == 0:
                    return j + 1
            j += 1
        return None

    def turbofish_start(self, end):
        """Find `::` for a balanced turbofish ending at end, else None."""
        v = self.values
        if end == 0 or v[end - 1] != ">":
            return None
        depth = 0
        i = end - 1
        while i >= 0:
            if v[i] in (")", "]", "}"):
                i = self.pairs[i] - 1
                continue
            if v[i] == ">" and (i == 0 or v[i - 1] != "-"):
                depth += 1
            elif v[i] == "<":
                depth -= 1
                if depth == 0:
                    start = i - 1
                    return (start if start >= 0 and self.turbofish_end(start) == end
                            else None)
            i -= 1
        return None

    def split(self, start, end, separator=","):
        out = []
        part = start
        i = start
        while i < end:
            if self.values[i] in ("(", "[", "{"):
                i = self.pairs[i] + 1
                continue
            after = self.turbofish_end(i)
            if after is not None:
                i = after
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
                continue
            after = self.turbofish_end(i)
            i = i + 1 if after is None else after
        return i

    def body_brace(self, head, end):
        """The `{` opening the body of the `if`/`while`/`for`/`match` at head.

        Rust forbids struct literals in these headers, but struct PATTERNS are
        legal between `let` and `=` (and between `for` and `in`), so braces in
        that window are not the body.
        """
        v = self.values
        in_pattern = v[head] == "for"
        n = head + 1
        while n < end:
            token = v[n]
            if token in ("(", "["):
                n = self.pairs[n] + 1
                continue
            if token == "{":
                if not in_pattern:
                    return n
                n = self.pairs[n] + 1
                continue
            if token == "let":
                in_pattern = True
            elif in_pattern and token == "in" and v[head] == "for":
                in_pattern = False
            elif (in_pattern and token == "=" and v[head] != "for"
                  and v[n - 1] not in (".", "=", "!", "<", ">")
                  and n + 1 < end and v[n + 1] != "="):
                in_pattern = False
            n += 1
        return end

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
        opened = self.body_brace(head, end)
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
                                and v[self.pairs[arm] + 1] in ("if", "=>", "|"))
                if not next_pattern and arm < end and v[arm] in (".", "?", "(", "[", "+", "-", "*", "/", "%", "as"):
                    arm = self.end(arm, {","}, end)
            else:
                arm = self.end(body, {","}, end)

    def receiver_start(self, dot):
        """Walk one postfix receiver backward without absorbing header keywords."""
        v = self.values
        start = dot
        while start > 0:
            token = v[start - 1]
            if token == "?":
                start -= 1
            elif token == ">":
                generic = self.turbofish_start(start)
                if generic is None:
                    break
                start = generic
            elif token in (")", "]"):
                start = self.pairs[start - 1]
                if (start == 0 or follows_expression_prefix(v, start)
                        or v[start - 1] in ("if", "while", "match", "for", "in")):
                    break
                if not (re.fullmatch(r"\w+", v[start - 1])
                        or v[start - 1] in (")", "]", "?")
                        or self.turbofish_start(start) is not None):
                    break
            elif re.fullmatch(r"\w+", token):
                start -= 1
                if (start > 0 and v[start - 1] in ("::", ".")
                        and not (v[start - 1] == "." and start > 1
                                 and v[start - 2] == ".")):
                    start -= 1
                else:
                    break
            else:
                break
        return start

    def postfix_receiver(self, start, end):
        """Accept one receiver chain, not a larger operator expression."""
        v = self.values
        if start < end and v[start] == "::":
            start += 1
        if start >= end:
            return False
        if re.fullmatch(r"\w+", v[start]):
            start += 1
        elif v[start] in ("(", "[") and self.pairs[start] < end:
            start = self.pairs[start] + 1
        else:
            return False
        while start < end:
            token = v[start]
            if token in ("(", "[") and self.pairs[start] < end:
                start = self.pairs[start] + 1
            elif token == "?":
                start += 1
            elif (token == "::" and re.fullmatch(r"\w+", v[start - 1])
                  and (after := self.turbofish_end(start)) is not None
                  and after <= end):
                start = after
            elif (token in ("::", ".") and start + 1 < end
                  and re.fullmatch(r"\w+", v[start + 1])):
                start += 2
            else:
                return False
        return True

    def wrapper_origin(self, name, opened, end):
        """Follow the registered wrapper's final closure result."""
        v = self.values
        args = self.split(opened + 1, end - 1)
        wrapper_args = RESULT_WRAPPERS[name]
        if len(args) >= wrapper_args:
            # Return-type commas can split the last closure into several ranges.
            a, b = args[wrapper_args - 1][0], args[-1][1]
            closure = a + (a < b and v[a] == "move")
            if closure < b and v[closure] == "|":
                close = self.end(closure + 1, {"|"}, b)
                if close < b:
                    body = close + 1
                    if v[body:body + 2] == ["-", ">"]:
                        # Stop at the balanced body, not at generic/function arrows.
                        body = self.end(body + 2, {"{"}, b)
                        if body < b and self.pairs[body] == b - 1:
                            return self.origin(body, b)
                    elif len(args) == wrapper_args:
                        return self.origin(body, b)
        return None

    def origin(self, start, end):
        """Return (helper token index, symbol) only for a helper RESULT.

        Registered context wrappers forward closure tails, including blocks
        preceded by an explicit return type.
        """
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
        generic = self.turbofish_start(opened)
        callee = opened - 1 if generic is None else generic - 1
        name = v[callee]
        if (name in RESULT_WRAPPERS and callee - 1 > start
                and v[callee - 1] == "."
                and self.postfix_receiver(start, callee - 1)):
            return self.wrapper_origin(name, opened, end)
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
        return None

    def ignored_binding(self, start, end):
        """Conservative wildcard/_name policy, irrespective of later uses."""
        while start < end and self.values[start] in ("ref", "mut"):
            start += 1
        return end == start + 1 and self.values[start].startswith("_")

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
                    return self.ignored_binding(a, b)
                start += 1
                end -= 1
            elif (end > start + 2 and v[start] in ("Some", "Ok")
                  and v[start + 1] == "(" and self.pairs[start + 1] == end - 1):
                args = self.split(start + 2, end - 1)
                if len(args) != 1:
                    return False
                start, end = args[0]
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
        if self.ignored_binding(start, end):
            yield start, "whole-result"
            return
        if self.residue_pattern(start, end, arity):
            yield start, "binding"
            return
        v = self.values
        if (start + 1 < end and v[start] in ("Some", "Ok")
                and v[start + 1] == "(" and self.pairs[start + 1] == end - 1):
            args = self.split(start + 2, end - 1)
            if len(args) == 1:
                yield from self.discard_patterns(*args[0], arity)
            return
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
                # Classify the whole initializer, including postfixes after the block.
                end = ex.end(end, {";", "{", "}", "else"})
            origin = ex.origin(equals + 1, end)
            arity = 3 if origin and origin[1] == "parse_target_with_syntax" else 2
            pattern_end = ex.end(i + 1, {":"}, equals)
            patterns = list(ex.discard_patterns(i + 1, pattern_end, arity))
            for site, family in patterns:
                add(origin, family, site if len(patterns) > 1 else None)
        if value == "match":
            end = ex.end(i + 1, {"{"})
            origin = ex.origin(i + 1, end)
            if origin and end < len(v):
                arity = 3 if origin[1] == "parse_target_with_syntax" else 2
                for a, b in ex.match_patterns(end + 1, ex.pairs[end]):
                    for site, family in ex.discard_patterns(a, b, arity):
                        add(origin, family, site)
        if value == "." and i + 1 < len(v) and v[i + 1] in ("0", "2"):
            # Walk backward over a standalone block or balanced call/parentheses.
            # Do not absorb an enclosing assignment or a keyword/header body.
            start = i - 1
            if start < 0:
                continue
            if v[start] == "}":
                start = ex.pairs[start]
                if (start > 0 and not follows_expression_prefix(v, start)
                        and (re.fullmatch(r"\w+", v[start - 1])
                             or v[start - 1] in (")", "]", ">"))):
                    continue
            elif v[start] == ")":
                start = ex.pairs[start]
                generic = ex.turbofish_start(start)
                callee = start - 1 if generic is None else generic - 1
                if (callee >= 0 and re.fullmatch(r"\w+", v[callee])
                        and not follows_expression_prefix(v, start)
                        and (generic is None or v[callee] in RESULT_WRAPPERS)):
                    start = callee
                    while start >= 2 and v[start - 1] in ("::", ".") and re.fullmatch(r"\w+", v[start - 2]):
                        start -= 2
                    if (v[callee] in RESULT_WRAPPERS and callee >= 2
                            and v[callee - 1] == "." and v[callee - 2] != "."):
                        start = min(start, ex.receiver_start(callee - 1))
            else:
                continue
            # A single dot leaves a method receiver; doubled dots are a range.
            if (start > 0 and v[start - 1] == "."
                    and not (start > 1 and v[start - 2] == ".")):
                continue
            origin = ex.origin(start, i)
            if origin and (v[i + 1] == "0" or origin[1] == "parse_target_with_syntax"):
                add(origin, "projection")
        if value == "drop" and i + 1 < len(v):
            opened = ex.turbofish_end(i + 1)
            # Extend free calls, not methods; doubled dots are range punctuation.
            if (i > 0 and v[i - 1] == "."
                    and not (i > 1 and v[i - 2] == ".")):
                continue
            if opened is None:
                opened = i + 1
            if opened < len(v) and v[opened] == "(":
                args = ex.split(opened + 1, ex.pairs[opened])
                if len(args) == 1:
                    add(ex.origin(*args[0]), "whole-result")
        if value == ";":
            if i > 0 and v[i - 1] == "}":
                opened = ex.pairs[i - 1]
                # Bare statement blocks only; headers and let RHSs have owners.
                if opened == 0 or v[opened - 1] in (";", "{", "}"):
                    add(ex.origin(opened, i), "whole-result")
            else:
                start = i - 1
                while start >= 0 and v[start] not in (";", "{", "}"):
                    if v[start] in (")", "]"):
                        start = ex.pairs[start]
                    start -= 1
                add(ex.origin(start + 1, i), "whole-result")
        if value == "unwrap_or" and i > 0 and v[i - 1] == "." and i + 1 < len(v) and v[i + 1] == "(":
            args = ex.split(i + 2, ex.pairs[i + 1])
            arg = v[args[0][0]:args[0][1]] if len(args) == 1 else []
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
    def is_test_file(path):
        name = PurePosixPath(path).name
        return name == "tests.rs" or name.endswith("_tests.rs")

    sources = {p: read_snapshot(root, p, index) for p in sorted(paths)
               if p.endswith(".rs") and "snapshots" not in PurePosixPath(p).parts}
    if not any(not is_test_file(p) for p in sources):
        raise ValueError("empty parser scope; cannot establish a zero-obligation baseline")
    edges = {}
    # Exact test filenames stay in the module graph so the children they outline
    # (tests/support.rs) are test-only too; their own text is never scanned.
    test_files = {p for p in sources if is_test_file(p)}
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
