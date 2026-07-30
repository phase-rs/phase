#!/usr/bin/env python3
"""CR 733 Phase-P0 reachability + mutation/write census generator (tooling only).

The CR 733 resolved-command-journal prerequisite (Phase P0) requires a checked
inventory of everything the ability-cost / mana-ability / replacement-drain
closure can *do* to game state, so that Plan 04's architecture remediation can
later assign every mutation primitive and `GameState` field write to exactly one
ordinary authority. This script is the durable GENERATOR for that inventory. It
is NOT a gate: it commits no baseline and wires no CI. The census OUTPUT is
expected to be regenerated after Plan 04 merges; the deliverable is this
generator plus its self-tests.

It answers five P0 questions over `crates/engine/src`:

  step 3  reachability   -- an over-approximate name-based call closure from the
                            activated-mana / triggered-mana / ability-cost /
                            replacement-continuation ROOTS (see ROOTS below).
  step 4  writes         -- every reachable `GameState` field write site (assign,
                            compound-assign, mutating method, `&mut` borrow, and
                            `let GameState { .. }` destructure).
  step 5  rng+allocator  -- every RNG consumer and id/timestamp allocator call.
  step 6  events+info    -- every `GameEvent` emission and information/visibility
                            side effect (reveal-family calls, visibility redaction).
  step 7  machine output -- deterministic JSON keyed for hash/count pinning.

Every detected site is annotated with whether its enclosing function is inside
the reachable closure, so a reviewer can filter the P0-relevant subset without
re-deriving reachability.

WHAT THIS TOOL DELIBERATELY DOES NOT DO (read this as an aid, not an oracle):

  * NO type resolution. A call is matched by NAME only. A method call
    `x.resolve_effect(..)` is treated identically to the free function
    `resolve_effect(..)`; both reach the node named `resolve_effect` wherever it
    is declared. Distinct functions that share a name collapse to one node.
  * NO trait-dispatch resolution. A `dyn`/generic call through a trait method is
    an unresolved call token, counted in `unresolved_calls`, never an edge.
  * NO macro expansion. Macro invocations (`foo!(..)`) are not calls here; a
    mutation performed only inside a macro body is invisible to this census.
  * NO field-alias resolution. A write through a receiver other than a literal
    `GameState` binding is matched only when the field name is unambiguously a
    `GameState` field; the receiver is recorded so review can reject false hits.
  * NO write-through-a-local resolution. A field borrowed into a local
    (`let v = &mut state.stack; v.push(x)`) is caught at the borrow, but the later
    `v.push(x)` is invisible -- `v` is not a GameState field name.
  * NO `mem::swap` / `mem::replace` / `mem::take` on a field, and no mutating
    method on an INDEXED element (`state.field[i].push(..)`): the write-site
    detectors anchor on `recv.field` directly, so a method reached through
    `mem::` or an index expression is not classified as a write.

Because of these limits the reachability set OVER-approximates (it never drops a
name silently -- unresolvable call tokens are counted, not ignored) and the
write census MAY over-report (receiver recorded for filtering). Over-reporting is
the deliberate bias: a P0 inventory that misses a mutation is worse than one that
lists a false candidate a reviewer can strike.

Usage:
    scripts/cr733_mutation_census.py --list        # human summary + population
    scripts/cr733_mutation_census.py --json PATH    # full machine-readable JSON
    scripts/cr733_mutation_census.py --hash         # stable content hash + counts
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import NamedTuple

# "Production engine code" -- inline `#[cfg(test)]` mod bodies skipped by brace
# depth, compound cfg predicates handled, strings/comments stripped, loud failure
# on brace desync -- is defined once, by the zone-authority census's scanner. It
# is reused rather than copied: a second lexer would be a second definition of
# production code, free to drift, and its disagreements would be silent in both
# directions (test code scanned as production, production skipped as test).
from zone_authority_census import (
    FN_DECL,
    REPO_ROOT,
    TEST_SUPPORT_FILES,
    CensusError,
    is_feature_gated_test_support_module,
    iter_production_lines,
)

SCHEMA = "cr733-p0-census/1"
REACHABILITY_MODE = "name-over-approximation"

# Only the engine crate: the P0 closure is engine game logic. Unlike the zone /
# continuation censuses, no adapter-side surface is in scope for this inventory.
SCOPE = "crates/engine/src"

GAME_STATE_FILE = "crates/engine/src/types/game_state.rs"
VISIBILITY_FILE = "crates/engine/src/game/visibility.rs"
GAME_DIR = "crates/engine/src/game"

# Fewer than this many parsed fields means the struct parser lost the brace span
# and is scanning the wrong region -- fail loudly rather than census a fraction.
MIN_GAMESTATE_FIELDS = 200

# ---------------------------------------------------------------------------
# Reachability ROOTS.
#
# Verified to exist (by `fn NAME(` declaration) at HEAD 931c2dc2f7. The lines are
# advisory only; `find_root_locations` re-verifies existence by grep and records
# the CURRENT line, and raises CensusError if any root name has vanished so drift
# is loud rather than a silently shrunken closure.
# ---------------------------------------------------------------------------
ROOTS: tuple[tuple[str, str], ...] = (
    ("crates/engine/src/game/mana_abilities.rs", "activate_mana_ability"),
    ("crates/engine/src/game/mana_abilities.rs", "complete_mana_ability_activation"),
    ("crates/engine/src/game/mana_abilities.rs", "advance_mana_ability_activation"),
    ("crates/engine/src/game/mana_abilities.rs", "resolve_mana_ability"),
    ("crates/engine/src/game/mana_abilities.rs", "resolve_triggered_mana_ability_inline"),
    ("crates/engine/src/game/mana_abilities.rs", "batch_activate_mana_siblings"),
    ("crates/engine/src/game/costs.rs", "pay_ability_cost_for_activation"),
    ("crates/engine/src/game/costs.rs", "pay_ability_cost_for_resolution"),
    ("crates/engine/src/game/costs.rs", "pay_ability_cost_for_replacement_may_cost"),
    ("crates/engine/src/game/engine_replacement.rs", "apply_pending_post_replacement_effect"),
    ("crates/engine/src/game/engine_replacement.rs", "apply_pending_spell_resolution"),
    ("crates/engine/src/game/effects/mod.rs", "resolve_effect"),
    ("crates/engine/src/game/effects/mod.rs", "resolve_ability_chain"),
    ("crates/engine/src/game/effects/mod.rs", "drain_pending_continuation"),
)


# ---------------------------------------------------------------------------
# Detectors. Every pattern runs on the shared scanner's STRIPPED `code`, never
# `raw`, so a comment or string literal that names a mutation cannot fabricate a
# site.
# ---------------------------------------------------------------------------

# Call token: an identifier immediately followed by `(`. This is how BOTH the
# call-graph edges AND the honest `unresolved_calls` counter are derived -- a
# token whose name is a known fn declaration is an edge; anything else (method on
# an unresolved receiver, tuple-struct constructor, external fn) is unresolved.
# Macro invocations (`foo!(`) never match: the `!` sits between name and paren.
CALL_TOKEN = re.compile(r"\b([A-Za-z_]\w*)\s*\(")

# Attribute tokens (`#[serde(..)]`, `#[cfg(..)]`, `#![allow(..)]`) contain
# parenthesised identifiers that are NOT calls. They are stripped from a line
# before `CALL_TOKEN` runs so the `unresolved_calls` metric stays true to its
# definition. `[^\]]*` stops at the first `]`; string bodies are already gone
# (the shared scanner removed them), so a bracketed string cannot hide the close.
ATTR = re.compile(r"#!?\[[^\]]*\]")

# Keywords `CALL_TOKEN` would otherwise capture as fake calls: control-flow that
# precedes a `(` (`for (`, `if (`), the `let (` tuple binding, and `pub(crate)` /
# `pub(super)` visibility. None is an edge or an honest "unresolved call". Macros
# already fall out via the `!`. A reserved keyword can never be a real fn name, so
# excluding these cannot hide a genuine edge.
NON_CALL_KEYWORDS = frozenset(
    {"if", "while", "for", "match", "return", "loop", "else", "fn", "let", "pub"}
)

# Mutating methods on a `GameState` field container (CR-agnostic: this is a
# write-surface census, not a rules gate). Omitting a membership-changing method
# does not make the write safe -- it makes the census blind to it. Ordered with
# prefixed variants first (`swap_remove` before `swap`, `extend_from_slice` before
# `extend`) for clarity; the `\.method\s*\(` anchor makes order non-load-bearing,
# since the method must start immediately after the dot and be followed by `(`.
MUT_METHODS = (
    "push_back",
    "push_front",
    "push",
    "pop_back",
    "pop_front",
    "pop",
    "insert_or_update",
    "insert",
    "swap_remove",
    "swap",
    "extend_from_slice",
    "extend",
    "split_off",
    "remove",
    "clear",
    "retain",
    "drain",
    "take",
    "replace",
    "append",
    "sort_by_key",
    "sort_by",
    "sort",
    "truncate",
    "resize",
    "dedup",
    "get_mut",
    "iter_mut",
    "values_mut",
    "last_mut",
    "first_mut",
    "entry",
    "set",
)
_MUT_METHOD_ALT = "|".join(MUT_METHODS)

# `let GameState { ... } = <expr>` destructure -- binds fields by move or `&mut`,
# a write surface for every bound field. The opener may or may not close on the
# same line; `detect_sites` carries the open brace across lines.
DESTRUCTURE_OPEN = re.compile(r"\blet\s+GameState\s*\{")
IDENT = re.compile(r"[A-Za-z_]\w*")

# RNG consumers. `state.rng` and the destructured-`rng` binding are the sources;
# the rest are the operations that consume it (CR 103 randomised choices, CR
# 701.24 shuffle). Destructured `rng` is handled in the destructure carrier, not
# here, because it is a bare identifier with no `state.` prefix.
RNG_PATTERNS = (
    ("state.rng", re.compile(r"\bstate\.rng\b")),
    ("shuffle", re.compile(r"\.shuffle\(\s*&mut")),
    ("choose", re.compile(r"\.choose\(\s*&mut")),
    ("random_bool", re.compile(r"\brandom_bool\s*\(")),
    ("random_range", re.compile(r"\brandom_range\s*\(")),
    ("next_u32", re.compile(r"\bnext_u32\s*\(")),
    ("RngCore", re.compile(r"\bRngCore\b")),
    ("rng_seed", re.compile(r"\brng_seed\b")),
    ("rng_word_pos", re.compile(r"\brng_word_pos\b")),
    ("set_word_pos", re.compile(r"\bset_word_pos\s*\(")),
)

# Allocators. The generic `next_*_id` shape future-proofs for Plan 04's
# ResolutionStack allocators; `next_timestamp` is named explicitly because it
# breaks the `_id` suffix convention. Each site records the matched symbol so the
# summary can group by allocator.
ALLOCATOR = re.compile(r"\b(next_\w*_id|next_timestamp)\b")

# GameEvent emission. `\bevents\.` matches both a bare `events.push(` (a local
# `&mut Vec<GameEvent>`) and `state.events.push(` / `self.events.push(` -- the
# word boundary sits between the `.` and `events` in the qualified forms.
EVENT_EMIT = re.compile(r"\bevents\.(push|extend)\s*\(")
EVENT_VARIANT = re.compile(r"\bevents\.(?:push|extend)\s*\(\s*(?:vec!\s*\[\s*)?GameEvent::(\w+)")

# Information boundary: redaction inside the visibility module. Reveal-family
# calls are detected separately, against the set of reveal fn names collected
# from production `game/` declarations.
REDACTION = re.compile(r"\bredact\w*", re.IGNORECASE)


class Site(NamedTuple):
    """One detected mutation / consumer / emission site.

    `reachable` is filled after the closure is computed; it is left False here and
    set by `annotate_reachability`.
    """

    family: str  # write | rng | allocator | event_emission | information
    file: str
    line: int
    fn: str
    pattern: str
    field: str | None = None
    receiver: str | None = None
    variant: str | None = None
    reachable: bool = False

    def to_json(self) -> dict:
        out = {
            "family": self.family,
            "file": self.file,
            "line": self.line,
            "fn": self.fn,
            "pattern": self.pattern,
            "reachable": self.reachable,
        }
        if self.field is not None:
            out["field"] = self.field
        if self.receiver is not None:
            out["receiver"] = self.receiver
        if self.variant is not None:
            out["variant"] = self.variant
        return out


# ---------------------------------------------------------------------------
# Scope + population.
# ---------------------------------------------------------------------------
def is_test_file(name: str) -> bool:
    return name == "tests.rs" or name.endswith("_tests.rs")


def production_rs_files() -> list[Path]:
    """Every production `.rs` file in scope, sorted for determinism.

    Excludes outlined test files (`*_tests.rs`, `tests.rs`), the shared
    test-support helper files, and feature-gated test-support game modules -- the
    same exclusions every sibling census applies through the shared seam.
    """
    out: list[Path] = []
    for path in sorted((REPO_ROOT / SCOPE).rglob("*.rs")):
        name = path.name
        if is_test_file(name) or name in TEST_SUPPORT_FILES:
            continue
        if is_feature_gated_test_support_module(path):
            continue
        out.append(path)
    return out


def rel(path: Path) -> str:
    return str(path.relative_to(REPO_ROOT))


# ---------------------------------------------------------------------------
# step 3 -- reachability.
# ---------------------------------------------------------------------------
def find_root_locations() -> list[dict]:
    """Verify every ROOT fn still exists and return its CURRENT declaration site.

    Raises CensusError if a root name has no `fn NAME(` declaration in its file --
    a vanished root would silently shrink the reachable closure, which is exactly
    the drift this census must make loud.
    """
    roots: list[dict] = []
    for file_rel, fn_name in ROOTS:
        path = REPO_ROOT / file_rel
        if not path.exists():
            raise CensusError(f"root file missing: {file_rel} (for fn `{fn_name}`)")
        decl = re.compile(rf"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+{re.escape(fn_name)}\s*[(<]")
        line_no: int | None = None
        for i, line in enumerate(path.read_text(encoding="utf-8", errors="replace").splitlines()):
            if decl.match(line):
                line_no = i + 1
                break
        if line_no is None:
            raise CensusError(
                f"root fn `{fn_name}` not found in {file_rel} -- reachability roots "
                "drifted; update ROOTS in scripts/cr733_mutation_census.py"
            )
        roots.append({"fn": fn_name, "file": file_rel, "line": line_no})
    return roots


def index_functions(files: list[Path]) -> tuple[set[str], dict[str, list[dict]]]:
    """Map every production `fn NAME` declaration to its site(s).

    Returns `(fn_names, decls)` where `fn_names` is the graph's node set and
    `decls[name]` lists `{file, line}` for each declaration (a name can be
    declared in many files -- they collapse to one over-approximated node).
    """
    fn_names: set[str] = set()
    decls: dict[str, list[dict]] = {}
    for path in files:
        r = rel(path)
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        for i, _raw, code, _fn in iter_production_lines(r, lines):
            m = FN_DECL.match(code)
            if m:
                name = m.group(1)
                fn_names.add(name)
                decls.setdefault(name, []).append({"file": r, "line": i + 1})
    return fn_names, decls


def iter_call_tokens(code: str, decl_name: str | None):
    """Yield each call-token NAME on a line, minus noise a call graph must reject.

    Control-flow keywords (`for (`, `if (`), the `let (` binding, and `pub(crate)`
    visibility are dropped -- they are neither edges nor honestly-unresolved calls.
    `decl_name` (the fn declared on THIS line, if any) is dropped too: a `fn foo(`
    signature is a declaration, not a call to `foo`, and counting it would forge a
    self-edge / self-site. Attribute tokens (`#[serde(..)]`, `#[cfg(..)]`) are
    stripped first so their parenthesised identifiers never masquerade as calls.
    """
    code = ATTR.sub(" ", code)
    for m in CALL_TOKEN.finditer(code):
        name = m.group(1)
        if name in NON_CALL_KEYWORDS or name == decl_name:
            continue
        yield name


def extract_call_edges(
    fn_names: set[str], code: str, enclosing_fn: str, decl_name: str | None = None
) -> tuple[list[tuple[str, str]], int]:
    """Return `(edges, unresolved_count)` for one line of stripped code.

    Every `NAME(` call token whose NAME is a known fn declaration becomes an edge
    `enclosing_fn -> NAME`. A token that is not a known fn (method on an
    unresolved receiver, tuple-struct ctor, external fn) is counted as unresolved
    -- never silently dropped -- which is what makes the over-approximation honest.
    """
    edges: list[tuple[str, str]] = []
    unresolved = 0
    for name in iter_call_tokens(code, decl_name):
        if name in fn_names:
            edges.append((enclosing_fn, name))
        else:
            unresolved += 1
    return edges, unresolved


def bfs_reachable(adjacency: dict[str, set[str]], root_names: set[str]) -> set[str]:
    """Reachable-name closure (BFS) from the root names over the call graph."""
    seen: set[str] = set()
    frontier = list(root_names)
    while frontier:
        name = frontier.pop()
        if name in seen:
            continue
        seen.add(name)
        frontier.extend(adjacency.get(name, ()))
    return seen


# ---------------------------------------------------------------------------
# step 4 -- GameState field list + write sites.
# ---------------------------------------------------------------------------
def parse_gamestate_fields(lines: list[str]) -> list[str]:
    """Parse the `pub struct GameState { .. }` field names by brace-depth span.

    Keys on `^\\s+pub NAME:` at brace depth 1 (directly inside the struct), so doc
    comments, `#[serde(..)]` / `#[cfg(..)]` attrs, and nested generic field types
    (`Vec<Foo<Bar>>`) never register as fields. The span ends when the struct's
    own brace closes, tracked by depth rather than a fixed line so it survives
    field churn. No minimum is enforced here -- callers that census the REAL
    struct apply `MIN_GAMESTATE_FIELDS`; the parser stays pure for tests.
    """
    field = re.compile(r"^\s+pub\s+(\w+)\s*:")
    start: int | None = None
    for i, line in enumerate(lines):
        if re.match(r"^\s*pub struct GameState\s*\{", line):
            start = i
            break
    if start is None:
        return []
    fields: list[str] = []
    depth = 0
    for j in range(start, len(lines)):
        line = lines[j]
        depth += line.count("{") - line.count("}")
        if j > start:
            if depth == 1:
                m = field.match(line)
                if m:
                    fields.append(m.group(1))
            if depth == 0:
                break
    return fields


def collect_gamestate_fields() -> list[str]:
    """Parse the real `GameState` fields, failing loudly on an implausible count."""
    path = REPO_ROOT / GAME_STATE_FILE
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    fields = parse_gamestate_fields(lines)
    if len(fields) < MIN_GAMESTATE_FIELDS:
        raise CensusError(
            f"parsed only {len(fields)} GameState fields (< {MIN_GAMESTATE_FIELDS}); "
            f"the struct brace span in {GAME_STATE_FILE} likely drifted"
        )
    return fields


def build_write_regexes(fields: list[str]) -> dict[str, re.Pattern]:
    """Compile the field-anchored write detectors once for a given field set.

    A single alternation over all field names, so each detector runs once per
    line instead of once per (line x field).
    """
    field_alt = "|".join(re.escape(f) for f in sorted(fields, key=len, reverse=True))
    return {
        # `recv.field =` / `+=` / `-=`, including an indexed element
        # `recv.field[i] = ...` (and multi-index `[i][j]`). The `(?![=>])` rejects
        # both `==` (comparison) and `=>` (match-arm fat arrow); the op capture
        # accepts only `=`, `+=`, `-=`, so `<=`, `>=`, `!=` never match (a `<`/`>`/`!`
        # sits where the op alternation requires `=`/`+`/`-`).
        "assign": re.compile(
            rf"\b(?P<recv>\w+)\.(?P<field>{field_alt})(?:\[[^\]]*\])*\s*(?P<op>\+=|-=|=)(?![=>])"
        ),
        # `recv.field.<mut-method>(`.
        "mut_method": re.compile(
            rf"\b(?P<recv>\w+)\.(?P<field>{field_alt})\.(?P<method>{_MUT_METHOD_ALT})\s*\("
        ),
        # `&mut recv.field`.
        "borrow": re.compile(rf"&mut\s+(?P<recv>\w+)\.(?P<field>{field_alt})\b"),
    }


def detect_write_sites(code: str, write_res: dict[str, re.Pattern]) -> list[tuple[str, str, str]]:
    """Return `(pattern, field, receiver)` for every field write on one line.

    Over-reports by design: `recv` may be any binding (`state`, `simulated`, `s`,
    `clone`), and only the field name proves GameState-hood, so the receiver is
    recorded for a reviewer to strike a false hit (e.g. a same-named field on an
    unrelated struct).
    """
    hits: list[tuple[str, str, str]] = []
    for pattern_name, regex in write_res.items():
        method_suffix = pattern_name == "mut_method"
        for m in regex.finditer(code):
            label = pattern_name
            if method_suffix:
                label = f"mut_method:{m.group('method')}"
            hits.append((label, m.group("field"), m.group("recv")))
    return hits


def extract_event_variant(code: str) -> str | None:
    """The `GameEvent::Variant` name pushed on this line, if the push is direct."""
    m = EVENT_VARIANT.search(code)
    return m.group(1) if m else None


def detect_allocators(code: str) -> list[str]:
    """Every allocator symbol (`next_*_id`, `next_timestamp`) called on this line."""
    return [m.group(1) for m in ALLOCATOR.finditer(code)]


# ---------------------------------------------------------------------------
# step 6 -- reveal-family fn names (information boundary).
# ---------------------------------------------------------------------------
def collect_reveal_fns(files: list[Path]) -> set[str]:
    """Production `game/` fn names whose name contains `reveal`.

    Calls to these are the information-boundary family. Collected from production
    declarations only (the shared seam skips test bodies), so a test helper named
    `..._reveal_...` never enters the set.
    """
    reveal: set[str] = set()
    game_root = REPO_ROOT / GAME_DIR
    for path in files:
        try:
            path.relative_to(game_root)
        except ValueError:
            continue
        r = rel(path)
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        for _i, _raw, code, _fn in iter_production_lines(r, lines):
            m = FN_DECL.match(code)
            if m and "reveal" in m.group(1).lower():
                reveal.add(m.group(1))
    return reveal


# ---------------------------------------------------------------------------
# Per-file site + edge detection (single pass over production lines).
# ---------------------------------------------------------------------------
class FileScan(NamedTuple):
    sites: list[Site]
    edges: list[tuple[str, str]]
    unresolved: int


def join_fluent_chains(
    rows: "list[tuple[int, str, str, str]]",
) -> "list[tuple[int, str, str, str]]":
    """Merge rustfmt-split fluent-chain continuations into one logical line.

    `state\\n    .field\\n    .entry(k)\\n    .push_back(v);` is one statement,
    but the write regexes match single lines and would miss it entirely. A
    production line whose code starts with `.` is appended to the preceding
    line of the same function; the merged statement keeps the first line's
    number. Lines starting with `..` (struct update syntax) merge harmlessly:
    no write pattern can match through `..`.
    """
    joined: list[tuple[int, str, str, str]] = []
    for i, raw, code, fn in rows:
        stripped = code.lstrip()
        if joined and stripped.startswith(".") and joined[-1][3] == fn:
            pi, praw, pcode, pfn = joined[-1]
            joined[-1] = (pi, praw, pcode.rstrip() + stripped, pfn)
        else:
            joined.append((i, raw, code, fn))
    return joined


def scan_file(
    path: Path,
    fn_names: set[str],
    fields_set: frozenset[str],
    write_res: dict[str, re.Pattern],
    reveal_fns: set[str],
) -> FileScan:
    """One production pass over a file: call edges + every family's sites."""
    r = rel(path)
    is_visibility = r == VISIBILITY_FILE
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()

    sites: list[Site] = []
    edges: list[tuple[str, str]] = []
    unresolved = 0
    # `let GameState { .. }` may span lines; carry the open destructure until its
    # `}` so bound field names on continuation lines are still captured.
    in_destructure = False

    for i, _raw, code, fn in join_fluent_chains(list(iter_production_lines(r, lines))):
        line_no = i + 1
        decl_m = FN_DECL.match(code)
        decl_name = decl_m.group(1) if decl_m else None

        # --- reachability edges + honest unresolved-call count ---
        line_edges, line_unresolved = extract_call_edges(fn_names, code, fn, decl_name)
        edges.extend(line_edges)
        unresolved += line_unresolved

        # --- destructure carrier (write family, plus rng if `rng` is bound) ---
        if in_destructure or DESTRUCTURE_OPEN.search(code):
            fragment = code
            if not in_destructure:
                m = DESTRUCTURE_OPEN.search(code)
                fragment = code[m.end():]
                in_destructure = True
            # Everything up to the closing brace is the binding list.
            close = fragment.find("}")
            binding = fragment if close == -1 else fragment[:close]
            for name in IDENT.findall(binding):
                if name in fields_set:
                    sites.append(
                        Site("write", r, line_no, fn, "destructure", field=name, receiver="GameState")
                    )
                if name == "rng":
                    sites.append(Site("rng", r, line_no, fn, "destructured-rng"))
            if close != -1:
                in_destructure = False

        # --- write family (assign / compound / mut-method / borrow) ---
        for pattern_label, field, receiver in detect_write_sites(code, write_res):
            sites.append(Site("write", r, line_no, fn, pattern_label, field=field, receiver=receiver))

        # --- rng family ---
        for label, regex in RNG_PATTERNS:
            if regex.search(code):
                sites.append(Site("rng", r, line_no, fn, label))

        # --- allocator family ---
        for alloc in detect_allocators(code):
            sites.append(Site("allocator", r, line_no, fn, "next-id-alloc", variant=alloc))

        # --- event emission family ---
        if EVENT_EMIT.search(code):
            sites.append(
                Site("event_emission", r, line_no, fn, "events-push", variant=extract_event_variant(code))
            )

        # --- information family: reveal-fn calls + visibility redaction ---
        for name in iter_call_tokens(code, decl_name):
            if name in reveal_fns:
                sites.append(Site("information", r, line_no, fn, "reveal-call", variant=name))
        if is_visibility and REDACTION.search(code):
            sites.append(Site("information", r, line_no, fn, "visibility-redaction"))

    return FileScan(sites, edges, unresolved)


# ---------------------------------------------------------------------------
# Census assembly.
# ---------------------------------------------------------------------------
def annotate_reachability(sites: list[Site], reachable: set[str]) -> list[Site]:
    return [s._replace(reachable=s.fn in reachable) for s in sites]


def site_sort_key(s: Site) -> tuple:
    return (s.file, s.line, s.family, s.pattern, s.field or "", s.variant or "", s.receiver or "")


def head_commit() -> str:
    try:
        out = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
            check=True,
        )
        return out.stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return "unknown"


def build_census() -> dict:
    """Run the full census against the worktree and return the JSON document."""
    files = production_rs_files()
    roots = find_root_locations()
    fn_names, _decls = index_functions(files)
    fields = collect_gamestate_fields()
    fields_set = frozenset(fields)
    write_res = build_write_regexes(fields)
    reveal_fns = collect_reveal_fns(files)

    all_sites: list[Site] = []
    adjacency: dict[str, set[str]] = {}
    unresolved_total = 0
    for path in files:
        scan = scan_file(path, fn_names, fields_set, write_res, reveal_fns)
        all_sites.extend(scan.sites)
        unresolved_total += scan.unresolved
        for src, dst in scan.edges:
            adjacency.setdefault(src, set()).add(dst)

    root_names = {root["fn"] for root in roots}
    reachable = bfs_reachable(adjacency, root_names)
    all_sites = annotate_reachability(all_sites, reachable)
    all_sites.sort(key=site_sort_key)

    per_family: dict[str, int] = {}
    per_field: dict[str, int] = {}
    for s in all_sites:
        per_family[s.family] = per_family.get(s.family, 0) + 1
        if s.family == "write" and s.field:
            per_field[s.field] = per_field.get(s.field, 0) + 1

    return {
        "schema": SCHEMA,
        "head_commit": head_commit(),
        "reachability_mode": REACHABILITY_MODE,
        "scope": SCOPE,
        "population": {
            "files": len(files),
            "lines": sum(len(p.read_text(encoding="utf-8", errors="replace").splitlines()) for p in files),
        },
        "roots": roots,
        "functions_indexed": len(fn_names),
        "reachable_functions": len(reachable),
        "unresolved_calls": unresolved_total,
        "gamestate_fields": len(fields),
        "sites": [s.to_json() for s in all_sites],
        "summary": {
            "per_family_counts": per_family,
            "per_field_write_counts": per_field,
        },
    }


def canonical_json(census: dict) -> str:
    """Deterministic serialisation: sorted keys, compact, newline-terminated."""
    return json.dumps(census, sort_keys=True, indent=2, ensure_ascii=False) + "\n"


def content_hash(census: dict) -> str:
    return hashlib.sha256(canonical_json(census).encode("utf-8")).hexdigest()


# ---------------------------------------------------------------------------
# CLI.
# ---------------------------------------------------------------------------
def render_list(census: dict) -> str:
    pop = census["population"]
    lines: list[str] = []
    lines.append(f"CR733 P0 census -- scope: {census['scope']}")
    lines.append(
        f"population: {pop['files']} production files, {pop['lines']} lines "
        f"(reachability_mode={census['reachability_mode']})"
    )
    lines.append(
        f"functions_indexed={census['functions_indexed']}  "
        f"reachable_functions={census['reachable_functions']}  "
        f"unresolved_calls={census['unresolved_calls']}  "
        f"gamestate_fields={census['gamestate_fields']}"
    )
    lines.append("")
    lines.append(f"roots ({len(census['roots'])}):")
    for root in census["roots"]:
        lines.append(f"  {root['fn']}  {root['file']}:{root['line']}")
    lines.append("")
    fam = census["summary"]["per_family_counts"]
    lines.append("per-family site counts:")
    for name in sorted(fam):
        lines.append(f"  {name}\t{fam[name]}")
    lines.append("")
    reachable_sites = sum(1 for s in census["sites"] if s["reachable"])
    lines.append(f"total sites: {len(census['sites'])}  (reachable-enclosed: {reachable_sites})")
    return "\n".join(lines) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--list", action="store_true", help="human summary + scanned population")
    g.add_argument("--json", metavar="PATH", help="write the full machine-readable census JSON to PATH")
    g.add_argument("--hash", action="store_true", help="print a stable content hash + per-family counts")
    args = ap.parse_args()

    try:
        census = build_census()
    except CensusError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 1

    if args.list:
        sys.stdout.write(render_list(census))
        return 0

    if args.json:
        Path(args.json).write_text(canonical_json(census), encoding="utf-8")
        print(
            f"wrote {args.json}: {len(census['sites'])} sites, "
            f"{census['reachable_functions']}/{census['functions_indexed']} reachable fns",
            file=sys.stderr,
        )
        return 0

    # --hash
    fam = census["summary"]["per_family_counts"]
    fam_str = " ".join(f"{k}={fam[k]}" for k in sorted(fam))
    print(
        f"sha256={content_hash(census)} sites={len(census['sites'])} "
        f"reachable_functions={census['reachable_functions']} "
        f"gamestate_fields={census['gamestate_fields']} unresolved_calls={census['unresolved_calls']} "
        f"{fam_str}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
