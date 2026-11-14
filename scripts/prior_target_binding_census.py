#!/usr/bin/env python3
"""Freeze the cross-slot object-relative target-binding surface (backlog root
cause #27: Puca's Mischief / Spawnbroker / Daring Thief).

This is a corpus-only census (no producer-side scan, unlike
draw_replacement_census.py): the surface it measures is entirely a property
of exported card data, not of Rust call sites. It follows the same
`--check` / `--write` exact-match idiom.

Sections, all derived from `data/card-data.json`:

  narrowed             target slots matching `target_filter_binds_prior_target`
                       (the CR 601.2c prior-object-relative predicate Unit B
                       implements) -- 5 before Unit A, 7 after (see "GAP-3
                       TIGHTENING" below for why this differs from the plan's
                       stated 8/10).
  wide                 additionally, slots matching the REJECTED
                       `filter_contains(ParentTarget | ParentTargetSlot)`
                       disjunct (deferral D1) -- kept measurable so the cost
                       of the rejected design stays visible.
  composed             `narrowed` slots that ALSO carry a relative
                       `ControllerRef` (deferral D2) -- expected empty.
  perturbable_readers  cards where a `narrowed` slot co-occurs with any of
                       {SameNameAsParentTarget, CanEnchant, DifferentNameFrom,
                       DistinctFrom} (MG-B R4/R5/R6), OR with a non-`Target`
                       `ObjectScope` inside any quantity-carrying prop
                       (MG-B/REQ-1's `object_for_scope`/`object_id_for_scope`
                       `Recipient` fallbacks and the positional
                       `TargetDamageSourceBinding` read at quantity.rs:6425-
                       6430 -- that last reader is a `ResolvedAbility`-only
                       runtime field, never present in exported
                       `AbilityDefinition` JSON, so it is recorded as
                       structurally unmeasurable from this corpus rather than
                       silently reported as zero).
  exchange_control     every `ExchangeControl` SLOT (both `target_a` and
                       `target_b`, across every effect instance -- Shifting
                       Grift's 3 modes and Kitsune, Dragon's Daughter's 2
                       triggers each contribute their own effect instance),
                       tagged with its MG-A class. UNIT IS SLOTS, stated once
                       here: a `TargetFilter::SelfRef` slot is not surfaced by
                       `collect_target_slots` (`ability_utils.rs:2667`) and is
                       recorded as `selfref-skip`, excluded from the class
                       tally. The summary line also reports the EFFECT-unit
                       count (effects containing >=1 slot of a class) for
                       cross-reference against MG-A's prose, since the two
                       units disagree (a plain-typed slot and a class-3
                       symmetric slot can sit in the same effect).
  amass_delta          target slots where the OLD hand-rolled
                       `target_filter_contains_amassed_army_ref` triple and
                       the NEW parameterized `target_filter_contains_quantity_
                       scope(_, AmassedArmy)` disagree -- expected empty (T9).

SCOPE, stated exactly rather than implied: "target slot" here is approximated
by NAME, not by re-deriving every `Effect::target_filter()` match arm in
Python. The scanned field names are exactly {target, target_a, target_b,
player_a, player_b, subject}, which is the field name `Effect::target_filter()`
returns for the overwhelming majority of variants (grep-verified against
`types/ability.rs`). A handful of variants target through a differently named
field (`Effect::Token`'s `owner`/`attach_to`, `EachSourceDealsDamage`'s
`recipient: Shared(filter)`) and are OUT OF SCOPE for this census -- named
here rather than silently omitted. `is_context_ref` is approximated by the
STATIC variant-name list from `TargetFilter::is_context_ref` (`types/
ability.rs:18132`); the two DYNAMIC arms (`references_exiled_by_source`,
`chosen_player_index`) are not reproduced, so a filter that is dynamically a
context-ref but does not match the static list is (rarely) over-counted here.

GAP-3 TIGHTENING (this revision), stated precisely rather than by example:
`target_slot_rows` now threads the NEAREST ENCLOSING ability's effect `type` +
`scope` + `target_choice_timing` through the same traversal that used to be a
blind field-name scan (see `_walk_keyed` / `_row_excluded` / `_extract_effect_
context`), and drops a field that the real `Effect::target_filter()` /
`collect_target_slots` gate would never have surfaced as a target slot:
  (1) `target_choice_timing == "Resolution"` on the field's OWN enclosing
      ability node (not inherited from a parent) -- `collect_target_slots`
      gates every arm on `== TargetChoiceTiming::Stack`.
  (2) The enclosing effect's `Effect::target_filter()` is verified-`None` for
      that exact variant (`UNCONDITIONAL_NONE_EFFECT_TYPES`: DamageAll /
      DestroyAll / GainControlAll / GoadAll / BounceAll / CounterAll /
      ChangeZoneAll / PutCounterAll / DoublePTAll / PumpAll -- all mass
      POPULATION filters, `types/ability.rs:18813-18999`) or `None` only for
      the observed `scope` (`SCOPE_GATED_NONE_EFFECT_TYPES`: SetTapState /
      Transform / ForceAttack / Suspect / Unsuspect, `None` iff
      `scope == "All"`, `types/ability.rs:18741-18809`). `ExchangeControl` /
      `ExchangeLifeTotals` / `Fight` / `CreateDamageReplacement` /
      `EachDealsDamageEqualToPower` are explicitly exempted
      (`MULTI_SLOT_BRANCH_EFFECT_TYPES`): the plan's population definition
      ORs these in via `collect_target_slots`' explicit multi-slot branches,
      not via `Effect::target_filter()`, which returns `None` for all five.

RESULT, reported rather than forced to match: after this tightening,
`narrowed` = 7 (5 before Unit A + Puca's Mischief + Spawnbroker after) and
`wide` = 21 -- NOT the plan's stated 8 (pre-fix) / 10 (post-fix) / 38. Per
GAP-3's explicit instruction, this number is reported as measured, not forced.
THE DELTA, verified against source rather than guessed:

  - Sacrifice does NOT return `None` from `Effect::target_filter()` (only
    from the SEPARATE `triggers::extract_target_filter_from_effect`, which
    special-cases it per CR 701.21a) -- `types/ability.rs:18504` matches it
    into the same `Some(target)` arm as every other plain-targeted effect.
    Braids Arisen Nightmare and Spirit-Sister's Call's Sacrifice.target slots
    are therefore correctly RETAINED in `narrowed`, unlike what an earlier
    draft of this docstring (and the orchestrator's own gap description,
    which this revision's source trace superseded) assumed.
  - Baral and Kari Zev and Counterlash's CastFromZone.target sub-abilities DO
    carry `target_choice_timing: Resolution` in the exported JSON (verified
    directly), so they are correctly EXCLUDED by criterion (1) above --
    despite the plan's own "Per-card classification of all 8" table listing
    both as members of its narrowed-8 population (with "Reaches the seam? NO,
    Resolution timing" as the stated reason in the SAME row).
  - Rally the Righteous's SetTapState{scope: All}.target sub-ability
    genuinely returns `None` from `Effect::target_filter()` (verified
    directly, `types/ability.rs:18749-18752`), so it is correctly EXCLUDED by
    criterion (2) -- despite the plan's table likewise listing it as a
    narrowed-8 member (with "target_filter() = None" as its OWN stated
    reason).
  - Cleansing Beam / Wojek Embermage (DamageAll), Fell the Mighty / Leave No
    Trace (DestroyAll), and Mists of Lórien (BounceAll) are excluded by the
    SAME criterion (2), for the SAME reason as Rally -- but the plan's hand
    census never enumerated them. These are structurally the identical
    "Radiance"-class shape as Rally (a real first target plus an "and each
    other X that shares Y with it" MASS expansion whose own field is spelled
    "target" but is never `Effect::target_filter()`-surfaced) -- the plan's
    hand enumeration is INTERNALLY INCONSISTENT: it kept Rally's instance of
    this shape while missing five siblings of the identical shape.
  My reading: the population as rigorously and UNIFORMLY re-derived from
  `Effect::target_filter()` + `target_choice_timing` (this script's stated
  population definition, taken literally) is 5 pre-fix / 7 post-fix, not
  8/10 -- the plan's own "8" is an incomplete/inconsistent hand enumeration
  against its own stated restriction, not a number this script should be
  bent to reproduce. `wide` is reduced by the identical mechanism.
Do not read `narrowed`/`wide`/`composed` row COUNTS as authoritative for the
plan's prose Blast Radius argument without re-checking that argument against
this finding. This script's `exchange_control` section has no such gap
(`ExchangeControl.target_a`/`target_b` is a `collect_target_slots` EXPLICIT
branch, unconditional on `target_choice_timing`, so REQ-2's per-class tally
is exact) and is unaffected by this tightening (still 34 effects / 68 slots,
matching the plan's anchor exactly).

This census is NOT wired into Tilt or CI in this PR (following probe-pin/
engine-census.toml's honesty about its own enforcement): it is a re-runnable
measurement, not a gate. `data/card-data.json` is gitignored, so the SCRIPT --
not its output -- is the durable artifact; the baseline TSV pins today's
numbers so a corpus change that moves them is visible, not silent.

Usage:
    scripts/prior_target_binding_census.py --corpus --check
    scripts/prior_target_binding_census.py --corpus --write
    scripts/prior_target_binding_census.py --corpus --check --card-data PATH
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
CARD_DATA = REPO_ROOT / "data" / "card-data.json"
BASELINE = REPO_ROOT / "scripts" / "prior-target-binding-corpus.tsv"

# ---------------------------------------------------------------------------
# TargetFilter / FilterProp / QuantityExpr structural walkers (Python mirror
# of filter::filter_contains / filter_prop_contains / quantity::
# quantity_expr_contains_scope). See the module docstring's SCOPE note for
# what this mirror does NOT reproduce.
# ---------------------------------------------------------------------------

TARGET_FIELD_NAMES = {"target", "target_a", "target_b", "player_a", "player_b", "subject"}

# Static half of `TargetFilter::is_context_ref` (`types/ability.rs:18132`).
IS_CONTEXT_REF_TYPES = {
    "None", "SelfRef", "SourceOrPaired", "Controller", "SourceController",
    "OriginalController", "OriginalSource", "ScopedPlayer",
    "TriggeringSpellController", "TriggeringSpellOwner", "TriggeringPlayer",
    "TriggeringSource", "EventTarget", "DefendingPlayer", "LastCreated",
    "Neighbor", "AttachedTo", "CostPaidObject", "AmassedArmy", "ParentTarget",
    "ParentTargetSlot", "ParentTargetController", "ParentTargetOwner",
    "SourceChosenPlayer", "PostReplacementSourceController",
    "PostReplacementDamageSource", "PostReplacementDamageTarget",
    "PostReplacementDamageTargetOwner", "ControllerAndControlledPermanents",
    "TrackedSet", "TrackedSetFiltered",
}

# QuantityRef variants carrying a bare `scope: ObjectScope` field (mirrors
# `quantity::quantity_expr_contains_scope`'s `ref_contains_scope`,
# quantity.rs:834-865).
QUANTITY_SCOPE_REF_TYPES = {
    "Power", "BasePower", "Toughness", "ObjectManaValue", "ObjectColorCount",
    "ObjectNameWordCount", "ObjectTypelineComponentCount",
    "ManaSymbolsInManaCost", "CountersOn",
}

# The OLD hand-rolled triple's narrower QuantityRef set (Power, Toughness,
# ObjectManaValue, ObjectColorCount, ObjectNameWordCount,
# ObjectTypelineComponentCount, ManaSymbolsInManaCost) -- deliberately missing
# BasePower and CountersOn, per T9's doc.
OLD_AMASS_QUANTITY_REF_TYPES = QUANTITY_SCOPE_REF_TYPES - {"BasePower", "CountersOn"}


def is_context_ref(filter_node: dict | None) -> bool:
    if not isinstance(filter_node, dict):
        return True
    return filter_node.get("type") in IS_CONTEXT_REF_TYPES


def quantity_expr_contains_scope(expr: Any, scope_type: str, ref_types: frozenset) -> bool:
    if not isinstance(expr, dict):
        return False
    t = expr.get("type")
    if t == "Fixed":
        return False
    if t == "Ref":
        qty = expr.get("qty") or {}
        if qty.get("type") in ref_types:
            return (qty.get("scope") or {}).get("type") == scope_type
        return False
    if t in ("DivideRounded", "Offset", "ClampMin", "Multiply"):
        return quantity_expr_contains_scope(expr.get("inner"), scope_type, ref_types)
    if t in ("Sum", "Max"):
        return any(
            quantity_expr_contains_scope(e, scope_type, ref_types)
            for e in expr.get("exprs") or []
        )
    if t == "UpTo":
        return quantity_expr_contains_scope(expr.get("max"), scope_type, ref_types)
    if t == "Power":
        return quantity_expr_contains_scope(expr.get("exponent"), scope_type, ref_types)
    if t == "Difference":
        return quantity_expr_contains_scope(
            expr.get("left"), scope_type, ref_types
        ) or quantity_expr_contains_scope(expr.get("right"), scope_type, ref_types)
    return False


def player_filter_contains(_player_node: Any, _leaf) -> bool:
    # Out of scope: `PlayerFilter`'s own nested-filter axes (`ControlsCount`,
    # `TrackedSetPossessor`, `OpponentDealtDamage`, ...) are not walked. Named
    # rather than silently answering `False` by omission of a call site: any
    # `ControllerMatches` / `PlayerMatching` crossing is undercounted by this
    # census. No corpus row currently depends on this (T9's `PlayerMatching
    # {ControlsCount{filter}}` claim is validated by the Rust unit test, not
    # by this script).
    return False


def target_filter_contains(filter_node: Any, leaf) -> bool:
    if not isinstance(filter_node, dict):
        return False
    if leaf(filter_node):
        return True
    t = filter_node.get("type")
    if t in ("And", "Or"):
        return any(target_filter_contains(f, leaf) for f in filter_node.get("filters") or [])
    if t == "Not":
        return target_filter_contains(filter_node.get("filter"), leaf)
    if t == "PlayerMatching":
        return player_filter_contains(filter_node.get("player"), leaf)
    if t == "TrackedSetFiltered":
        return target_filter_contains(filter_node.get("filter"), leaf)
    if t == "ChosenDamageSource":
        inner = filter_node.get("filter")
        return inner is not None and target_filter_contains(inner, leaf)
    if t == "Typed":
        return any(filter_prop_contains(p, leaf) for p in filter_node.get("properties") or [])
    return False


def filter_prop_contains(prop: dict, leaf) -> bool:
    t = prop.get("type")
    if t == "CanEnchant":
        return target_filter_contains(prop.get("target"), leaf)
    if t in ("DifferentNameFrom", "TargetsOnly", "Targets"):
        return target_filter_contains(prop.get("filter"), leaf)
    if t == "DistinctFrom":
        return target_filter_contains(prop.get("reference"), leaf)
    if t == "SharesQuality":
        ref = prop.get("reference")
        return ref is not None and target_filter_contains(ref, leaf)
    if t == "ControllerMatches":
        return player_filter_contains(prop.get("player"), leaf)
    if t == "AnyOf":
        return any(filter_prop_contains(p, leaf) for p in prop.get("props") or [])
    if t == "Not":
        return filter_prop_contains(prop.get("prop") or {}, leaf)
    return False


def _quantity_value_field(prop: dict) -> Any:
    return prop.get("count") if prop.get("type") == "Counters" else prop.get("value")


def filter_prop_binds_prior_target(prop: dict) -> bool:
    """Python mirror of `ability_utils::filter_prop_binds_prior_target`."""
    t = prop.get("type")
    if t in ("Cmc", "Counters", "PtComparison"):
        return quantity_expr_contains_scope(
            _quantity_value_field(prop), "Target", QUANTITY_SCOPE_REF_TYPES
        )
    if t == "SharesQuality":
        ref = prop.get("reference")
        return isinstance(ref, dict) and ref.get("type") == "ParentTarget"
    if t == "AnyOf":
        return any(filter_prop_binds_prior_target(p) for p in prop.get("props") or [])
    if t == "Not":
        return filter_prop_binds_prior_target(prop.get("prop") or {})
    return False


def _narrowed_leaf(filter_node: dict) -> bool:
    if filter_node.get("type") != "Typed":
        return False
    return any(filter_prop_binds_prior_target(p) for p in filter_node.get("properties") or [])


def target_filter_binds_prior_target(filter_node: dict) -> bool:
    return target_filter_contains(filter_node, _narrowed_leaf)


def _wide_leaf(filter_node: dict) -> bool:
    return filter_node.get("type") in ("ParentTarget", "ParentTargetSlot")


def target_filter_contains_parent_target_family(filter_node: dict) -> bool:
    return target_filter_contains(filter_node, _wide_leaf)


def _relative_controller_present(filter_node: Any) -> bool:
    if not isinstance(filter_node, dict):
        return False
    t = filter_node.get("type")
    if t == "Typed":
        if filter_node.get("controller") in ("You", "TargetPlayer", "TargetOpponent"):
            return True
        for p in filter_node.get("properties") or []:
            if p.get("type") == "Owned" and p.get("controller") in (
                "You",
                "TargetPlayer",
                "TargetOpponent",
            ):
                return True
        return False
    if t in ("Or", "And"):
        return any(_relative_controller_present(f) for f in filter_node.get("filters") or [])
    if t == "Not":
        return _relative_controller_present(filter_node.get("filter"))
    return False


PERTURBABLE_PROP_TYPES = {
    "SameNameAsParentTarget",
    "CanEnchant",
    "DifferentNameFrom",
    "DistinctFrom",
}


def _card_walk(node: Any):
    """Yield every dict nested anywhere inside `node` (no key tracking)."""
    if isinstance(node, dict):
        yield node
        for v in node.values():
            yield from _card_walk(v)
    elif isinstance(node, list):
        for v in node:
            yield from _card_walk(v)


def card_has_perturbable_prop(card_data: dict) -> bool:
    return any(
        isinstance(n, dict) and n.get("type") in PERTURBABLE_PROP_TYPES
        for n in _card_walk(card_data)
    )


def card_has_non_target_quantity_scope(card_data: dict) -> bool:
    """REQ-1: a quantity-carrying prop's `QuantityRef` scope is present and is
    NOT `Target` anywhere in the card -- the `object_for_scope` /
    `object_id_for_scope` `Recipient`-fallback shape (quantity.rs:5705-5713,
    5792-5799) and siblings. Scans the whole card (not just target-field
    slots): the perturbable reader lives in quantity RESOLUTION, which reads
    `ability.targets` regardless of which field mints the quantity.
    """
    for n in _card_walk(card_data):
        if not isinstance(n, dict):
            continue
        if n.get("type") in QUANTITY_SCOPE_REF_TYPES and "scope" in n:
            scope = n.get("scope")
            if isinstance(scope, dict) and scope.get("type") not in (None, "Target"):
                return True
    return False


# ---------------------------------------------------------------------------
# Row collection
#
# GAP-3 TIGHTENING: the population is meant to be "fields `Effect::target_
# filter()` returns `Some` for, plus `collect_target_slots`' explicit
# multi-slot branches" (the plan's own restriction sentence). A pure
# field-NAME scan (the original `_walk_keyed`) cannot tell a `DamageAll.target`
# (a mass POPULATION filter -- `Effect::target_filter()` is unconditionally
# `None`, verified at `types/ability.rs:18832`) from a `DealDamage.target` (a
# real declared target -- `Some`) because both fields are spelled "target".
# The walk below threads the NEAREST ENCLOSING ability's effect `type` +
# `scope` + `target_choice_timing` through the SAME recursive traversal
# `_walk_keyed` used (so every row it found is still found, including target
# fields nested inside a `FilterProp` such as `CanEnchant.target`), and uses
# that context to decide whether the field the plan's Rust authority would
# have surfaced as a target slot in the first place.
# ---------------------------------------------------------------------------

# `Effect::target_filter()` returns `None` for these variants UNCONDITIONALLY
# despite each carrying a TARGET_FIELD_NAMES-named field -- verified against
# `types/ability.rs:18813-18999` ("--- Effects with no player-selectable
# target field ---"). Each is a mass POPULATION filter enumerated at
# resolution, not a stack-time declared target.
UNCONDITIONAL_NONE_EFFECT_TYPES = {
    "DamageAll", "DestroyAll", "GainControlAll", "GoadAll", "BounceAll",
    "CounterAll", "ChangeZoneAll", "PutCounterAll", "DoublePTAll", "PumpAll",
}

# `Effect::target_filter()` returns `None` for these variants ONLY when
# `scope == "All"` (`Some` when `scope == "Single"`, the serde default) --
# verified at `types/ability.rs:18741-18809`.
SCOPE_GATED_NONE_EFFECT_TYPES = {
    "SetTapState", "Transform", "ForceAttack", "Suspect", "Unsuspect",
}

# Effect kinds surfaced NOT via `Effect::target_filter()` (which returns
# `None` for every one of these -- `types/ability.rs:18894` for
# `ExchangeControl`, `:18899` for `EachDealsDamageEqualToPower`, `:18912` for
# `ExchangeLifeTotals`) but via `collect_target_slots`' explicit multi-slot
# branches, which the plan's population definition ORs in separately. Immune
# to the None-effect-type exclusion above; `target_choice_timing` still
# applies (none of these carry it in the corpus today, but the rule stays
# uniform rather than special-cased away).
MULTI_SLOT_BRANCH_EFFECT_TYPES = {
    "ExchangeControl", "ExchangeLifeTotals", "Fight", "CreateDamageReplacement",
    "EachDealsDamageEqualToPower",
}


def _extract_effect_context(ability_node: dict) -> tuple[str, str | None, str] | None:
    """`ability_node` is ability-shaped (has an `effect` dict with a `type`).
    Returns `(effect_type, scope_type_or_None, target_choice_timing)`,
    defaulting `target_choice_timing` to `"Stack"` (the engine's
    `collect_target_slots` gate default) when the field is absent, matching
    how omitted-vs-Stack is treated identically in the exported JSON.
    """
    effect = ability_node.get("effect")
    if not isinstance(effect, dict) or "type" not in effect:
        return None
    scope = effect.get("scope")
    scope_type = scope.get("type") if isinstance(scope, dict) else scope
    choice_timing = ability_node.get("target_choice_timing", "Stack")
    return (effect["type"], scope_type, choice_timing)


def _row_excluded(ctx: tuple[str, str | None, str] | None) -> bool:
    if ctx is None:
        return False
    effect_type, scope_type, choice_timing = ctx
    # CR 115.1d: a sub-ability whose target is chosen at RESOLUTION (not
    # stack-push time) is never surfaced by `collect_target_slots`, which
    # gates every arm on `target_choice_timing == Stack`.
    if choice_timing == "Resolution":
        return True
    if effect_type in MULTI_SLOT_BRANCH_EFFECT_TYPES:
        return False
    if effect_type in UNCONDITIONAL_NONE_EFFECT_TYPES:
        return True
    if effect_type in SCOPE_GATED_NONE_EFFECT_TYPES:
        return scope_type == "All"
    return False


def _walk_keyed(
    node: Any, key: str | None = None, ctx: tuple[str, str | None, str] | None = None
):
    """Yield (key, node, ctx) for every dict, where `key` is the dict key that
    held it (or the key that held the enclosing list) and `ctx` is the nearest
    enclosing ability's `_extract_effect_context()` result (or `None` before
    any ability node has been seen). Entering an ability-shaped dict (its own
    `effect` key holds a typed dict) updates `ctx` for its own subtree,
    including its `sub_ability` chain -- each ability node's
    `target_choice_timing` is its own, never inherited from a parent.
    """
    if isinstance(node, dict):
        yield key, node, ctx
        new_ctx = _extract_effect_context(node) or ctx
        for k, v in node.items():
            yield from _walk_keyed(v, k, new_ctx)
    elif isinstance(node, list):
        for v in node:
            yield from _walk_keyed(v, key, ctx)


def target_slot_rows(export: dict) -> list[tuple[str, str, dict]]:
    """Every (card, field, filter_node) pair reached via a `TARGET_FIELD_NAMES`
    key, excluding context-ref filters AND (GAP-3 tightening) fields the
    engine's own `Effect::target_filter()` / `target_choice_timing` gate would
    never surface as a target slot in the first place. See the module
    docstring's SCOPE note for what remains unmodeled.
    """
    rows: list[tuple[str, str, dict]] = []
    for card, data in export.items():
        if not isinstance(data, dict):
            continue
        for key, node, ctx in _walk_keyed(data):
            if key not in TARGET_FIELD_NAMES:
                continue
            if not isinstance(node, dict) or "type" not in node:
                continue
            if is_context_ref(node):
                continue
            if _row_excluded(ctx):
                continue
            rows.append((card, key, node))
    return rows


def _summarize(filter_node: dict) -> str:
    t = filter_node.get("type")
    if t == "Typed":
        props = ",".join(p.get("type", "?") for p in filter_node.get("properties") or [])
        return f"Typed(ctrl={filter_node.get('controller')},props=[{props}])"
    return str(t)


# ---------------------------------------------------------------------------
# exchange_control section
# ---------------------------------------------------------------------------

STACK_LEAF_TYPES = {"StackSpell"}


def _contains_leaf_type(filter_node: dict, types: set) -> bool:
    return target_filter_contains(filter_node, lambda f: f.get("type") in types)


def _has_symmetric_shares_quality(filter_node: dict) -> bool:
    def leaf(f: dict) -> bool:
        if f.get("type") != "Typed":
            return False
        for p in f.get("properties") or []:
            if p.get("type") == "SharesQuality" and p.get("reference") is None:
                return True
        return False

    return target_filter_contains(filter_node, leaf)


def classify_exchange_slot(filter_node: dict | None) -> str:
    """MG-A per-slot classification. UNIT IS SLOTS -- see module docstring."""
    if filter_node is None:
        return "no-slot"
    t = filter_node.get("type")
    if t == "SelfRef":
        return "selfref-skip"
    if t == "TriggeringSource":
        return "class4-stack-object"
    if _contains_leaf_type(filter_node, STACK_LEAF_TYPES):
        return "class4-stack-object"
    if target_filter_binds_prior_target(filter_node):
        return "class2-prior-target-relative"
    if _has_symmetric_shares_quality(filter_node):
        return "class3-symmetric-shares-quality"
    return "class1-plain-typed"


def exchange_control_rows(export: dict) -> list[tuple[str, ...]]:
    rows: list[tuple[str, ...]] = []
    for card, data in export.items():
        if not isinstance(data, dict):
            continue
        effect_index = 0
        for n in _card_walk(data):
            if not (isinstance(n, dict) and n.get("type") == "ExchangeControl"):
                continue
            effect_index += 1
            for slot_letter, field in (("A", "target_a"), ("B", "target_b")):
                filt = n.get(field)
                cls = classify_exchange_slot(filt)
                summary = _summarize(filt) if isinstance(filt, dict) else "None"
                rows.append((card, str(effect_index), slot_letter, cls, summary))
    return sorted(rows)


def exchange_control_summary(rows: list[tuple[str, ...]]) -> str:
    slot_classes: dict[str, int] = {}
    effect_classes: dict[tuple[str, str], set[str]] = {}
    for card, effect_idx, _slot, cls, _summary in rows:
        slot_classes[cls] = slot_classes.get(cls, 0) + 1
        effect_classes.setdefault((card, effect_idx), set()).add(cls)
    effects_by_class: dict[str, int] = {}
    for classes in effect_classes.values():
        for cls in classes:
            effects_by_class[cls] = effects_by_class.get(cls, 0) + 1
    total_effects = len(effect_classes)
    lines = [
        f"exchange_control: {total_effects} effects, {len(rows)} slot rows "
        f"(2 per effect; SelfRef slots counted as selfref-skip, not surfaced).",
        "  by SLOT (primary unit):",
    ]
    for cls in sorted(slot_classes):
        lines.append(f"    {cls}: {slot_classes[cls]}")
    lines.append("  by EFFECT (secondary unit, >=1 slot of that class):")
    for cls in sorted(effects_by_class):
        lines.append(f"    {cls}: {effects_by_class[cls]}")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# amass_delta section
# ---------------------------------------------------------------------------


def _old_amass_leaf(filter_node: dict) -> bool:
    if filter_node.get("type") != "Typed":
        return False
    for p in filter_node.get("properties") or []:
        if p.get("type") in ("Cmc", "Counters", "PtComparison") and quantity_expr_contains_scope(
            _quantity_value_field(p), "AmassedArmy", OLD_AMASS_QUANTITY_REF_TYPES
        ):
            return True
    return False


def _old_amass_contains(filter_node: Any) -> bool:
    """Mirrors the OLD hand-rolled `target_filter_contains_amassed_army_ref` /
    `filter_prop_contains_amassed_army_ref` pair, which recursed ONLY through
    `CanEnchant` / `DifferentNameFrom` / `TargetsOnly` / `Targets` /
    `SharesQuality` / `AnyOf` / `Not` -- NOT through `ControllerMatches`,
    `DistinctFrom`, `PlayerMatching`, or `ChosenDamageSource` -- and used the
    narrower `OLD_AMASS_QUANTITY_REF_TYPES` leaf set (no `BasePower` / no
    `CountersOn`).
    """
    if not isinstance(filter_node, dict):
        return False
    t = filter_node.get("type")
    if t == "Typed":
        if _old_amass_leaf(filter_node):
            return True
        for p in filter_node.get("properties") or []:
            pt = p.get("type")
            if pt == "CanEnchant" and _old_amass_contains(p.get("target")):
                return True
            if pt in ("DifferentNameFrom", "TargetsOnly", "Targets") and _old_amass_contains(
                p.get("filter")
            ):
                return True
            if pt == "SharesQuality":
                ref = p.get("reference")
                if ref is not None and _old_amass_contains(ref):
                    return True
            if pt == "AnyOf" and any(
                _old_amass_contains_prop(inner) for inner in p.get("props") or []
            ):
                return True
            if pt == "Not" and _old_amass_contains_prop(p.get("prop") or {}):
                return True
        return False
    if t in ("Not", "TrackedSetFiltered"):
        return _old_amass_contains(filter_node.get("filter"))
    if t in ("Or", "And"):
        return any(_old_amass_contains(f) for f in filter_node.get("filters") or [])
    return False


def _old_amass_contains_prop(prop: dict) -> bool:
    pt = prop.get("type")
    if pt in ("Cmc", "Counters", "PtComparison"):
        return quantity_expr_contains_scope(
            _quantity_value_field(prop), "AmassedArmy", OLD_AMASS_QUANTITY_REF_TYPES
        )
    if pt == "CanEnchant":
        return _old_amass_contains(prop.get("target"))
    if pt in ("DifferentNameFrom", "TargetsOnly", "Targets"):
        return _old_amass_contains(prop.get("filter"))
    if pt == "SharesQuality":
        ref = prop.get("reference")
        return ref is not None and _old_amass_contains(ref)
    if pt == "AnyOf":
        return any(_old_amass_contains_prop(p) for p in prop.get("props") or [])
    if pt == "Not":
        return _old_amass_contains_prop(prop.get("prop") or {})
    return False


def _new_amass_contains(filter_node: dict) -> bool:
    return target_filter_contains(
        filter_node,
        lambda f: f.get("type") == "Typed"
        and any(
            p.get("type") in ("Cmc", "Counters", "PtComparison")
            and quantity_expr_contains_scope(
                _quantity_value_field(p), "AmassedArmy", QUANTITY_SCOPE_REF_TYPES
            )
            for p in f.get("properties") or []
        ),
    )


def amass_delta_rows(rows: list[tuple[str, str, dict]]) -> list[tuple[str, str]]:
    out = []
    for card, field, node in rows:
        old = _old_amass_contains(node)
        new = _new_amass_contains(node)
        if old != new:
            out.append((card, field))
    return sorted(set(out))


# ---------------------------------------------------------------------------
# Section assembly
# ---------------------------------------------------------------------------


def build_sections(export: dict) -> dict[str, list[tuple[str, ...]]]:
    rows = target_slot_rows(export)

    narrowed = sorted(
        {
            (card, field, _summarize(node))
            for card, field, node in rows
            if target_filter_binds_prior_target(node)
        }
    )
    wide = sorted(
        {
            (card, field, _summarize(node))
            for card, field, node in rows
            if target_filter_contains_parent_target_family(node)
        }
    )
    composed = sorted(
        {
            (card, field, _summarize(node))
            for card, field, node in rows
            if target_filter_binds_prior_target(node) and _relative_controller_present(node)
        }
    )

    narrowed_cards = {card for card, _field, _node in rows if target_filter_binds_prior_target(_node)}
    perturbable = []
    for card in sorted(narrowed_cards):
        data = export.get(card)
        if not isinstance(data, dict):
            continue
        reasons = []
        if card_has_perturbable_prop(data):
            reasons.append("same-name-or-enchant-or-distinct-prop")
        if card_has_non_target_quantity_scope(data):
            reasons.append("non-target-object-scope-in-quantity")
        if reasons:
            perturbable.append((card, ";".join(reasons)))
    perturbable.append(
        (
            "(structural)",
            "TargetDamageSourceBinding::Bound positional read (quantity.rs:6425-6430) "
            "is a ResolvedAbility-only runtime field, never present in exported "
            "AbilityDefinition JSON -- unmeasurable from this corpus.",
        )
    )

    exchange_control = exchange_control_rows(export)
    amass_delta = amass_delta_rows(rows)

    return {
        "narrowed": narrowed,
        "wide": wide,
        "composed": composed,
        "perturbable_readers": perturbable,
        "exchange_control": exchange_control,
        "amass_delta": [(c, f) for c, f in amass_delta],
    }


SECTION_ORDER = [
    "narrowed",
    "wide",
    "composed",
    "perturbable_readers",
    "exchange_control",
    "amass_delta",
]

HEADER = """\
# Frozen cross-slot object-relative target-binding census (backlog root cause
# #27: Puca's Mischief / Spawnbroker / Daring Thief).
#
# Generated by scripts/prior_target_binding_census.py --corpus --write from
# data/card-data.json. Do not hand-edit. See the script's module docstring
# for what each section measures and its stated scope limits.
#
# Columns: section <TAB> ... (varies by section, see docstring).
# Exact-match gate: an added, removed, or reclassified row fails until a
# human reviews it and re-freezes with --write.
#
"""


def render(sections: dict[str, list[tuple[str, ...]]]) -> str:
    lines = [HEADER.rstrip("\n")]
    for section in SECTION_ORDER:
        for row in sections[section]:
            lines.append("\t".join([section, *row]))
    return "\n".join(lines) + "\n"


def load_baseline(path: Path) -> dict[str, list[tuple[str, ...]]]:
    sections: dict[str, list[tuple[str, ...]]] = {s: [] for s in SECTION_ORDER}
    if not path.exists():
        return sections
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("#") or not line.strip():
            continue
        cols = line.split("\t")
        section, rest = cols[0], tuple(cols[1:])
        if section in sections:
            sections[section].append(rest)
    return sections


def diff_and_report(actual: dict[str, list[tuple[str, ...]]], baseline: dict[str, list[tuple[str, ...]]]) -> int:
    ok = True
    for section in SECTION_ORDER:
        a = actual[section]
        b = baseline[section]
        added = [r for r in a if r not in b]
        removed = [r for r in b if r not in a]
        if added or removed:
            ok = False
            print(f"ERROR: section `{section}` changed vs the frozen baseline.", file=sys.stderr)
            for r in added:
                print(f"  ADDED    {section}\t{'  '.join(r)}", file=sys.stderr)
            for r in removed:
                print(f"  REMOVED  {section}\t{'  '.join(r)}", file=sys.stderr)
        else:
            print(f"prior-target-binding {section}: PASS ({len(a)} rows)")
    print()
    print(exchange_control_summary(actual["exchange_control"]))
    if not ok:
        print(
            "\nReview the change -- especially any `exchange_control` or "
            "`perturbable_readers` row -- then re-freeze:\n"
            "    scripts/prior_target_binding_census.py --corpus --write\n",
            file=sys.stderr,
        )
        return 1
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--corpus", action="store_true", required=True, help="census the exported card corpus")
    how = ap.add_mutually_exclusive_group(required=True)
    how.add_argument("--check", action="store_true", help="gate against the frozen baseline")
    how.add_argument("--write", action="store_true", help="re-freeze the baseline")
    ap.add_argument("--card-data", type=Path, default=CARD_DATA, help="path to card-data.json")
    args = ap.parse_args()

    if not args.card_data.exists():
        print(
            f"ERROR: {args.card_data} not found.\n\n"
            "The corpus gate reads the generated card-data export, which is\n"
            "gitignored. Generate it first (./scripts/gen-card-data.sh), or\n"
            "point at another export with --card-data.\n\n"
            "This is an error, not a skip: a gate that passes when its input is\n"
            "missing would report green on a corpus it never read.",
            file=sys.stderr,
        )
        return 2

    export = json.loads(args.card_data.read_text(encoding="utf-8"))
    sections = build_sections(export)

    if args.write:
        BASELINE.write_text(render(sections), encoding="utf-8")
        total = sum(len(v) for v in sections.values())
        print(f"wrote {BASELINE.relative_to(REPO_ROOT)}: {total} rows")
        print()
        print(exchange_control_summary(sections["exchange_control"]))
        return 0

    return diff_and_report(sections, load_baseline(BASELINE))


if __name__ == "__main__":
    sys.exit(main())
