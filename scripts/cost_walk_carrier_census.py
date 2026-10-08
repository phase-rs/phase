#!/usr/bin/env python3
"""Drift guard: no `AbilityCost` may sit on an effect-payload-nested `AbilityDefinition`.

`types/ability_visit.rs` carries a documented KNOWN GAP. `visit_effect_scoped`
descends nested `AbilityDefinition`s under eight inline branch carriers (all via
`visit_nested_ability_def_scoped`); `visit_ability_def_costs_scoped` does not --
it follows only the chain-link axis (`cost`, `unless_pay.cost`, `sub_ability`,
`else_ability`, `mode_abilities`).

Consequence if the shape ever appears: CR 605.1a's cost criterion ("its cost and
effect don't move any card to or from a library") is never applied to a cost
sitting on a carrier-nested definition, so an ability whose branch pays `Mill` /
`Exile { Library }` / `ExileWithAggregate { Library }` / `ReturnToHand { Library }`
wrongly keeps mana-ability status. Those four are the ones that matter, because
they carry no nested `Effect` at all and are therefore structurally invisible to
any effect-shaped visitor.

**Why this is a CI gate and not a Rust test.** The trigger lives in a DIFFERENT
subsystem: the gap activates when a *parser* production starts emitting a cost on
a branch-carried definition. That change compiles cleanly, fails no existing test,
and its author is working in `parser/oracle_effect/` where nothing points at
`ability_visit.rs`. That author's change arrives through CI -- so the guard has to
run there. An equivalent `#[test]` reading the full export via
`support::shared_card_export_json()` would self-skip in the CI `Rust tests` job
(the export is gitignored and absent there; see the header of
`scripts/check-test-card-data-load.sh`), i.e. it would be green for exactly the
person it exists to stop. Same reasoning, and the same job placement, as
`scripts/draw_replacement_census.py --corpus`.

Running without card-data is an error, never a silent skip: a gate that quietly
passes when its input is missing is not a gate.

The real fix is to drive both walks from one shared carrier -> nested-def list;
that widens cost coverage for every existing `ResolutionScope::IncludeRegisteredLater`
caller (8 public entry points), so it needs its own census and is chartered
separately rather than riding along on a classifier change.

Usage:
    scripts/cost_walk_carrier_census.py --check                    # gate (card-data job)
    scripts/cost_walk_carrier_census.py --check --card-data PATH   # e.g. client/public/card-data.json
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
CARD_DATA = REPO_ROOT / "data" / "card-data.json"

# Serialized `Effect` tags that carry a nested `AbilityDefinition` in a payload
# the cost walk never enters. Kept as serialized tags rather than typed variants
# because this guard reads the raw export, which is exactly the representation a
# parser change would alter.
INLINE_BRANCH_CARRIERS = frozenset(
    {
        "Vote",
        "SeparateIntoPiles",
        "RevealFromHand",
        "FlipCoin",
        "FlipCoins",
        "FlipCoinUntilLose",
        "RollDie",
        "ChooseOneOf",
    }
)

# A truncated or partially-generated export would satisfy the zero-hit assertion
# vacuously. The real corpus is ~35k cards.
MIN_CARDS = 30_000


def is_ability_def_shaped(node: dict) -> bool:
    """An `AbilityDefinition`-shaped node carries an `effect` or a `kind`.

    Those are the nodes `visit_nested_ability_def_scoped` descends into and that
    `visit_ability_def_costs_scoped` never reaches from a carrier.
    """
    return "effect" in node or "kind" in node


def carrier_tag(node: dict) -> str | None:
    tag = node.get("type")
    return tag if isinstance(tag, str) and tag in INLINE_BRANCH_CARRIERS else None


def walk(node, under_carrier: str | None, trail: str, hits: list[str], seen: list[int]) -> None:
    """Record every `AbilityDefinition`-shaped node under a carrier that has a cost.

    Once inside a carrier payload the whole subtree is unreachable from the cost
    walk -- the nested definition's own `cost` and every `cost` on its
    `sub_ability` chain alike -- so `under_carrier` stays sticky.
    """
    if isinstance(node, dict):
        inner = carrier_tag(node)
        if inner is not None:
            seen[0] += 1
        if under_carrier is not None and is_ability_def_shaped(node) and node.get("cost") is not None:
            hits.append(f"{trail} (under `{under_carrier}`)")
        # A node can be BOTH nested under an outer carrier and a carrier itself;
        # the innermost tag is the more useful one to report.
        nxt = inner if inner is not None else under_carrier
        for key, value in node.items():
            walk(value, nxt, f"{trail}/{key}", hits, seen)
    elif isinstance(node, list):
        for index, value in enumerate(node):
            walk(value, under_carrier, f"{trail}[{index}]", hits, seen)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--check", action="store_true", required=True, help="gate: fail on any hit")
    ap.add_argument("--card-data", type=Path, default=CARD_DATA, help="path to card-data.json")
    args = ap.parse_args()

    if not args.card_data.exists():
        print(
            f"ERROR: {args.card_data} not found.\n\n"
            "This gate reads the generated card-data export, which is gitignored.\n"
            "Generate it first (Tilt `card-data` resource, or `cargo run --profile\n"
            "tool --features cli --bin oracle-gen -- data/ --stats --names-out\n"
            "data/card-names.json > data/card-data.json`), or point at another\n"
            "export with --card-data.\n\n"
            "This is an error, not a skip: a gate that passes when its input is\n"
            "missing would report green on a corpus it never read.",
            file=sys.stderr,
        )
        return 2

    export = json.loads(args.card_data.read_text(encoding="utf-8"))

    if len(export) < MIN_CARDS:
        print(
            f"ERROR: reach guard failed -- export has {len(export)} entries, expected >= {MIN_CARDS}.\n"
            "A truncated export makes the zero-hit result below meaningless.",
            file=sys.stderr,
        )
        return 2

    hits: list[str] = []
    seen = [0]
    for name, card in export.items():
        walk(card, None, name, hits, seen)

    # Second reach guard, and the one that actually proves the WALK ran: the
    # carriers must be present in the corpus at all. If a rename made every tag
    # stale, the scan would find nothing and pass for the wrong reason.
    if seen[0] == 0:
        print(
            f"ERROR: reach guard failed -- no node in the export carries any of\n"
            f"{sorted(INLINE_BRANCH_CARRIERS)}.\n"
            "The serialized tags were probably renamed, so this gate is scanning\n"
            "for shapes that no longer exist and would pass vacuously. Re-derive\n"
            "the tag list from `Effect`'s serde representation.",
            file=sys.stderr,
        )
        return 2

    if hits:
        print(
            f"ERROR: {len(hits)} cost(s) now sit on an effect-payload-nested "
            f"`AbilityDefinition`.\n\n"
            "This is the KNOWN GAP documented on `visit_ability_def_costs_scoped` in\n"
            "crates/engine/src/types/ability_visit.rs: the cost walk does NOT descend\n"
            "inline branch carriers, so CR 605.1a's cost criterion (\"its cost and\n"
            "effect don't move any card to or from a library\") is silently NOT applied\n"
            "to these costs. If any of them is `Mill`, `Exile { Library }`,\n"
            "`ExileWithAggregate { Library }` or `ReturnToHand { Library }`, the owning\n"
            "ability is now misclassified as a mana ability.\n\n"
            "Fix by driving `visit_ability_def_costs_scoped` and `visit_effect_scoped`\n"
            "from one shared carrier->nested-def list (and census the 8\n"
            "`ResolutionScope::IncludeRegisteredLater` entry points, whose cost coverage\n"
            "widens with it).\n",
            file=sys.stderr,
        )
        for hit in hits:
            print(f"  {hit}", file=sys.stderr)
        return 1

    print(
        f"ok: scanned {len(export)} cards, {seen[0]} carrier nodes, "
        f"0 costs on effect-payload-nested AbilityDefinitions"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
