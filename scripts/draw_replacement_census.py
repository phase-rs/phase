#!/usr/bin/env python3
"""Freeze the `ReplacementEvent::Draw` definition surface (Plan 03, CR 121 / CR 614).

Plan 03 rewrites draw delivery into a three-stage CR 121.2 state machine, which
requires every Draw replacement definition to declare, at construction, whether
it modifies the *instruction* count (CR 121.2a, "you draw that many cards plus
one instead") or replaces one *individual* draw (CR 121.2, "if you would draw a
card"). Scope is not derivable after the fact: today's `execute` chain, quantity
modification, description text, and count all fail to distinguish the two -- so
the rewrite has to set it at each producer, and a producer it misses gets a
silently wrong default.

This census freezes both halves of that surface so the rewrite cannot miss one:

  (A) --producers  the production sites that can mint a Draw definition
  (B) --corpus     the exported card corpus that carries one, with the scope
                   each row must be assigned

Section (A) is a source scan and runs anywhere. Section (B) needs the generated
`data/card-data.json`, which is gitignored and produced by a *different* CI job
than the Rust lint job -- so it is wired into the card-data gate, not the lint
gate. Running `--corpus` without card-data is an error, never a silent skip: a
gate that quietly passes when its input is missing is not a gate.

Both baselines are exact-match (not ratchets, unlike the zone-authority census):
an added, removed, or reclassified row fails until a human reviews it and
re-freezes with --write.

Usage:
    scripts/draw_replacement_census.py --producers --check   # gate (lint job)
    scripts/draw_replacement_census.py --corpus --check      # gate (card-data job)
    scripts/draw_replacement_census.py --producers --write   # re-freeze
    scripts/draw_replacement_census.py --corpus --write
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

# "Production engine code" (inline `#[cfg(test)]` mod bodies skipped by brace
# depth, compound cfg predicates, strings/comments stripped, loud failure on
# brace desync) is defined once, by the zone-authority census's scanner. Reused
# rather than copied: two copies would be two definitions of production code,
# free to drift, and their disagreements would be silent in both directions.
from zone_authority_census import (
    REPO_ROOT,
    SCOPES as ENGINE_SCOPES,
    TEST_SUPPORT_FILES,
    iter_production_lines,
)

PRODUCERS_BASELINE = REPO_ROOT / "scripts" / "draw-replacement-producers.txt"
CORPUS_BASELINE = REPO_ROOT / "scripts" / "draw-replacement-corpus.tsv"
CARD_DATA = REPO_ROOT / "data" / "card-data.json"

# The zone census scans the engine, because only the engine mutates zones. A Draw
# REPLACEMENT DEFINITION, though, can be minted by anything that builds one and
# hands it to the engine — and `crates/mtgish-import` does exactly that. Scanning
# it is a READ; nothing here modifies it.
#
# Omitting it is how the first cut of this gate froze 6 producers while a 7th was
# live: a census is only as honest as the population it admits, and "the engine"
# was the wrong population for this question.
SCOPES = ENGINE_SCOPES + ("crates/mtgish-import/src",)

# ---------------------------------------------------------------------------
# (A) Producer surface
# ---------------------------------------------------------------------------
#
# Three shapes mint a Draw `ReplacementDefinition`. All three are matched, and all
# three are named here with the file they live in, so this block cannot drift away
# from the regexes below:
#
#   ReplacementDefinition::new(ReplacementEvent::Draw)     -> family `constructor`
#       parser/oracle_replacement.rs  (x2)
#       database/synthesis.rs::synthesize_dredge
#       game/effects/create_draw_replacement.rs::resolve
#
#   => ReplacementEvent::Draw                              -> family `event-decode`
#       types/replacements.rs::from_str
#   => Ok(ReplacementEvent::Draw)                          -> family `event-decode`
#       database/forge/replacement.rs  (Result-returning, hence the Ok wrap)
#
#   ReplacementDefinition { .. event: ReplacementEvent::Draw .. }
#                                                          -> family `struct-literal`
#       mtgish-import/convert/replacement.rs::convert_replace_would_draw
#
# The struct-literal family is not hypothetical scaffolding for the one mtgish
# site: `ReplacementDefinition { .. }` is a live idiom inside the engine too --
# 50 literals across crates/engine/src (non-test files), 17 of them in
# database/synthesis.rs, 13 of those in production code. None is a Draw today, so
# the family has exactly one occupant -- but the next engine Draw written as a
# literal would have been invisible to a constructor+decode census.
#
# KNOWN-UNMATCHED, ZERO OCCUPANTS: a fourth mint shape exists in principle --
# field assignment after construction (`def.event = ReplacementEvent::Draw;`).
# None of the three patterns matches it. There are ZERO occurrences of
# `.event = ReplacementEvent::` anywhere under crates/ today (any variant, not
# just Draw), so the shape is latent rather than live. It is named here rather
# than silently omitted: an instrument that lists what it misses is honest; one
# that implies completeness is not.

# Matches ONE of the two live ways Rust code builds a `ReplacementDefinition`:
# the `::new(..)` builder. It does NOT match the other one -- a struct literal --
# which is why `STRUCT_LITERAL` below exists. Across `crates/` today: 429 `::new(`
# sites and 91 `ReplacementDefinition {` sites, so neither shape is exotic.
#
# Naming this "the way a definition is built in Rust" is what the first cut of
# this comment did, and it is the same category error that hid the mtgish
# producer for a full review cycle: a comment that describes a family instead of
# a pattern invites the reader to believe the pattern covers the family.
CONSTRUCTOR = re.compile(r"ReplacementDefinition::new\(\s*ReplacementEvent::Draw(Cards)?\b")

# A match arm YIELDING the event (see the two shapes above). Keyed on the arm's
# RESULT, not on a `"Draw"` string literal, because the shared scanner strips
# string literals before matching (brace counting must not see a `{` inside a
# string) — a pattern spelled `"Draw"\s*=>` would match nothing and the census
# would report zero decode sites while passing green.
#
# Direction matters: an arrow BEFORE the variant produces it; an arrow AFTER it
# merely matches on it (coverage.rs's `ReplacementEvent::Draw | ... => {`). Only
# the former is a producer, so the `=>` must lead.
EVENT_DECODE = re.compile(r"=>\s*(?:Ok\()?\s*ReplacementEvent::Draw(Cards)?\b")

# The `event:` field of a struct literal. Keyed on the field binding rather than
# on `ReplacementDefinition {`, because the literal spans many lines and the
# scanner is line-oriented: the type name and the event sit on different lines.
STRUCT_LITERAL = re.compile(r"\bevent:\s*ReplacementEvent::Draw(Cards)?\b")

FAMILIES = (
    ("constructor", CONSTRUCTOR),
    ("event-decode", EVENT_DECODE),
    ("struct-literal", STRUCT_LITERAL),
)


def collect_producers() -> dict[tuple[str, str, str], int]:
    counts: dict[tuple[str, str, str], int] = {}
    for scope in SCOPES:
        for path in sorted((REPO_ROOT / scope).rglob("*.rs")):
            name = path.name
            if name in TEST_SUPPORT_FILES:
                continue
            if name == "tests.rs" or name.endswith("_tests.rs"):
                continue
            rel = str(path.relative_to(REPO_ROOT))
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
            # No exemption annotation is honoured here, unlike the zone census: a
            # Draw producer is never exempt. Every one of them must assign a scope.
            for _i, _raw, code, fn in iter_production_lines(rel, lines):
                for family, pattern in FAMILIES:
                    if pattern.search(code):
                        key = (rel, fn, family)
                        counts[key] = counts.get(key, 0) + 1
    return counts


PRODUCERS_HEADER = """\
# Frozen census of every production site MATCHING THE THREE SHAPES BELOW that
# mints a `ReplacementEvent::Draw` replacement definition (Plan 03 / CR 121.2).
#
# The phrasing is deliberate. This is not "every site that can mint one" -- that
# is a claim about a CATEGORY, and this file implements PATTERNS. A fourth shape
# exists in principle (field assignment, `def.event = ReplacementEvent::Draw;`)
# and no pattern here matches it; it has zero occurrences under crates/ today.
# See the family block in draw_replacement_census.py.
#
# Generated by scripts/draw_replacement_census.py --producers --write.
# Columns: file <TAB> enclosing fn <TAB> family <TAB> count.
# Keyed on the enclosing function, not the line, so it survives line drift.
#
# Plan 03 adds `DrawReplacementScope::{InstructionCount, IndividualDraw}` to
# `ReplacementDefinition` and requires `Some(scope)` exactly when the event is
# `Draw`. Scope is declared at construction and never inferred later, so EVERY
# row below is a site the rewrite must touch. A new row means a new producer
# that would otherwise get a silently wrong default scope.
#
# family=constructor     `ReplacementDefinition::new(ReplacementEvent::Draw)`
# family=event-decode    `=> ReplacementEvent::Draw` / `=> Ok(ReplacementEvent::Draw)`
#                        (from_str; the Forge importer). Mints a definition
#                        without calling the constructor.
# family=struct-literal  `ReplacementDefinition { .. event: ReplacementEvent::Draw .. }`
#                        (mtgish-import). Same: no constructor call.
#
# SCOPE, stated exactly rather than implied: this scans the engine crates AND
# crates/mtgish-import/src. It does NOT scan any other crate. The first cut of
# this gate scanned the engine only, and froze 6 producers while a 7th was live in
# mtgish-import -- the count was right about the population it looked at and wrong
# about the question it claimed to answer.
#
# COVERAGE BOUNDARY, stated so this is not mistaken for "every possible source":
# a `ReplacementDefinition` also comes back to life via its plain serde derive
# when the engine loads the card-data export. That path is structurally singular
# (one struct's own derive, not a scanned set of call sites), so it is gated by
# the corpus baseline instead -- which reads exactly the bytes serde reads.
#
# This is an exact-match gate, not a ratchet: adding or removing a producer
# fails until a human reviews it and re-freezes.
#
"""


# ---------------------------------------------------------------------------
# (B) Card corpus
# ---------------------------------------------------------------------------


def walk(node: object):
    """Yield every dict nested anywhere inside `node`."""
    if isinstance(node, dict):
        yield node
        for v in node.values():
            yield from walk(v)
    elif isinstance(node, list):
        for v in node:
            yield from walk(v)


def reads_event_context_amount(count: object) -> bool:
    """True when a Draw's count reads the count of the event it is replacing.

    This is the CR 121.2a count-modifier shape ("you draw that many cards plus
    one instead") -- the *only* mechanical signal that separates an instruction
    -count replacement from an individual-draw replacement. A fixed substitute
    (Blood Scrivener's `Draw { count: Fixed 2 }`) does not read it.
    """
    return any(d.get("type") == "EventContextAmount" for d in walk(count))


# The only `quantity_modification` values this classifier has been taught to
# reason about. `Prevent` cancels the event outright and says nothing about
# instruction-vs-individual scope, so it falls through to the execute-shape rule
# (and both pool cards carrying it -- Living Conundrum, Possessed Portal -- have
# singular antecedents and classify IndividualDraw, which is correct).
#
# Every OTHER variant (`Times`, `Half`, `Plus`, `Minus`, ...) modifies the COUNT.
# On a Draw row that is a CR 121.2a instruction-count modifier by definition, and
# the execute-shape rule below would silently call it IndividualDraw -- wrong.
KNOWN_QUANTITY_MODIFICATIONS = {"Prevent"}


class ClassificationError(Exception):
    """The classifier met a shape it was never taught. Refuse to guess."""


def classify_scope(card: str, repl: dict) -> str:
    """The `DrawReplacementScope` this definition must be assigned.

    CR 121.2a: an instruction to draw multiple cards can be modified by
    replacement effects that refer to the number of cards drawn, and that
    modification happens before any individual card draw. Those -- and only
    those -- are `InstructionCount`. Everything else (Dredge per CR 702.52a,
    prevention, Notion-Thief-class substitution, the Words runtime shields)
    replaces one individual draw: CR 121.2, "cards may only be drawn one at a
    time".

    A count-modifying `quantity_modification` would ALSO be `InstructionCount`,
    and this function does not classify one -- it refuses. No card in the pool
    carries one on a Draw row today, so any rule written for it would ship with
    zero validating cases, and an unvalidated rule in a gate that a human is
    invited to trust is exactly where a wrong answer hides. Better to stop and
    make someone look: that is what an exact-match gate is FOR.
    """
    qm = repl.get("quantity_modification")
    if isinstance(qm, dict):
        qm = qm.get("type")
    if qm is not None and qm not in KNOWN_QUANTITY_MODIFICATIONS:
        raise ClassificationError(
            f"{card}: Draw replacement carries quantity_modification `{qm}`, which "
            f"this classifier has never seen.\n"
            f"A count-modifying quantity_modification is a CR 121.2a instruction-count "
            f"modifier and must be classified InstructionCount -- but no pool card has "
            f"exercised that path, so the rule is unwritten rather than wrong.\n"
            f"Decide the scope for this row deliberately, teach it to classify_scope() "
            f"(and to KNOWN_QUANTITY_MODIFICATIONS), then re-freeze."
        )

    effect = ((repl.get("execute") or {}).get("effect")) or {}
    if effect.get("type") == "Draw" and reads_event_context_amount(effect.get("count")):
        return "InstructionCount"
    return "IndividualDraw"


def corpus_rows(export: dict) -> list[tuple[str, ...]]:
    rows: list[tuple[str, ...]] = []
    for card, value in export.items():
        if not isinstance(value, dict):
            continue
        for repl in value.get("replacements") or []:
            if repl.get("event") not in ("Draw", "DrawCards"):
                continue
            execute = repl.get("execute")
            effect = (execute or {}).get("effect") or {}
            qm = repl.get("quantity_modification")
            if isinstance(qm, dict):
                qm = qm.get("type", "?")
            # Does the substitute itself start a new Draw instruction? Those are
            # the rows that push a child frame onto the draw stack (CR 121.6b /
            # CR 616.1g), so the rewrite must not collapse them into a count bump.
            nested_draw = any(
                d.get("type") == "Draw" and "count" in d for d in walk(execute)
            )

            # The scope column stopped being an aspiration the day the engine
            # started emitting `draw_scope`. Two INDEPENDENT derivations must now
            # agree: what the producer declared at construction (`emitted`), and
            # what this script derives from the definition's shape (`required`).
            #
            # Checking only that the field is present would be vacuous — a producer
            # that defaulted every Draw to IndividualDraw would sail through. The
            # cross-check is what makes a wrong scope a build failure, and it is the
            # whole reason the scope is declared rather than inferred at match time.
            required = classify_scope(card, repl)
            emitted = repl.get("draw_scope")
            if emitted is None:
                raise ClassificationError(
                    f"{card}: Draw replacement has no `draw_scope` (CR 121.2 requires "
                    f"every Draw definition to declare its scope at construction; "
                    f"expected {required!r}). A producer is not setting it."
                )
            if emitted != required:
                raise ClassificationError(
                    f"{card}: producer declared draw_scope={emitted!r} but the "
                    f"definition's shape requires {required!r} (CR 121.2a). Either the "
                    f"producer is wrong, or this card is a shape the classifier has "
                    f"not been taught."
                )

            rows.append(
                (
                    card,
                    repl["event"],
                    (repl.get("mode") or {}).get("type", "none"),
                    str(qm or "none"),
                    effect.get("type", "none") if execute else "none",
                    "nested-draw" if nested_draw else "-",
                    required,
                )
            )
    return sorted(rows)


CORPUS_HEADER = """\
# Frozen `ReplacementEvent::Draw` card corpus (Plan 03 / CR 121.2).
#
# Generated by scripts/draw_replacement_census.py --corpus --write from
# data/card-data.json. Do not hand-edit.
# Columns: card <TAB> event <TAB> mode <TAB> quantity_modification <TAB>
#          execute root <TAB> nested-draw <TAB> required DrawReplacementScope.
#
# The scope column is the contract: when Plan 03 adds
# `DrawReplacementScope` to `ReplacementDefinition`, the value each producer
# assigns to these cards must equal the value frozen here. It is derived
# mechanically (InstructionCount iff the substitute's Draw count reads
# `EventContextAmount`, the CR 121.2a count-modifier shape), NOT hand-listed.
#
# `nested-draw` marks a substitute whose own execute starts a fresh Draw
# instruction. Per CR 121.6b / CR 616.1g those drain as a *child* draw before
# the original sequence resumes -- they never become `count > 1` on the
# individual draw they replaced.
#
# `event=DrawCards` must stay at zero rows: it is a dead registry alias
# (replacement.rs `registry.insert(DrawCards, stub())`) with no corpus member.
# Plan 03 removes it rather than carrying a second runtime event class.
#
# Exact-match gate: an added, removed, or reclassified card fails until a human
# reviews it and re-freezes.
#
"""


# ---------------------------------------------------------------------------


def render(header: str, rows: list) -> str:
    body = "\n".join("\t".join(str(c) for c in r) for r in rows)
    return header + body + ("\n" if body else "")


def load_baseline(path: Path) -> list[tuple[str, ...]]:
    if not path.exists():
        return []
    out = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("#") or not line.strip():
            continue
        out.append(tuple(line.split("\t")))
    return out


def diff_and_report(kind: str, actual: list, baseline: list, refreeze: str) -> int:
    added = [r for r in actual if r not in baseline]
    removed = [r for r in baseline if r not in actual]
    if not added and not removed:
        print(f"draw-replacement {kind}: PASS ({len(actual)} rows, baseline frozen)")
        return 0
    print(f"ERROR: the frozen Draw-replacement {kind} changed.\n", file=sys.stderr)
    for r in added:
        print(f"  ADDED    {'  '.join(str(c) for c in r)}", file=sys.stderr)
    for r in removed:
        print(f"  REMOVED  {'  '.join(str(c) for c in r)}", file=sys.stderr)
    print(
        f"\nPlan 03 pins this surface because a Draw replacement's CR 121.2 scope is\n"
        f"declared at construction and cannot be recovered afterwards. Review the\n"
        f"change -- especially any row whose required scope moved -- then re-freeze:\n"
        f"    {refreeze}\n",
        file=sys.stderr,
    )
    return 1


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    what = ap.add_mutually_exclusive_group(required=True)
    what.add_argument("--producers", action="store_true", help="census the Rust producer sites")
    what.add_argument("--corpus", action="store_true", help="census the exported card corpus")
    how = ap.add_mutually_exclusive_group(required=True)
    how.add_argument("--check", action="store_true", help="gate against the frozen baseline")
    how.add_argument("--write", action="store_true", help="re-freeze the baseline")
    ap.add_argument("--card-data", type=Path, default=CARD_DATA, help="path to card-data.json")
    args = ap.parse_args()

    if args.producers:
        counts = collect_producers()
        # Stringify the count: `load_baseline` parses every column back as text,
        # so an int here would make every row compare unequal to its own baseline.
        rows = [(f, fn, fam, str(n)) for (f, fn, fam), n in sorted(counts.items())]
        baseline_path, header, kind = PRODUCERS_BASELINE, PRODUCERS_HEADER, "producers"
        refreeze = "scripts/draw_replacement_census.py --producers --write"
    else:
        if not args.card_data.exists():
            print(
                f"ERROR: {args.card_data} not found.\n\n"
                "The corpus gate reads the generated card-data export, which is\n"
                "gitignored. Generate it first (Tilt `card-data` resource, or\n"
                "`cargo run --profile tool --features cli --bin oracle-gen -- data/ \\\n"
                "  --stats --names-out data/card-names.json > data/card-data.json`),\n"
                "or point at another export with --card-data.\n\n"
                "This is an error, not a skip: a gate that passes when its input is\n"
                "missing would report green on a corpus it never read.",
                file=sys.stderr,
            )
            return 2
        export = json.loads(args.card_data.read_text(encoding="utf-8"))
        try:
            rows = corpus_rows(export)
        except ClassificationError as e:
            print(f"ERROR: {e}", file=sys.stderr)
            return 3
        baseline_path, header, kind = CORPUS_BASELINE, CORPUS_HEADER, "corpus"
        refreeze = "scripts/draw_replacement_census.py --corpus --write"

    if args.write:
        baseline_path.write_text(render(header, rows), encoding="utf-8")
        print(f"wrote {baseline_path.relative_to(REPO_ROOT)}: {len(rows)} rows")
        return 0

    return diff_and_report(kind, rows, load_baseline(baseline_path), refreeze)


if __name__ == "__main__":
    sys.exit(main())
