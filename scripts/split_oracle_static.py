#!/usr/bin/env python3
"""Split oracle_static.rs into oracle_static/ per issue #1674 (line-based)."""

from __future__ import annotations

import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "crates/engine/src/parser/oracle_static.rs"
OUT = ROOT / "crates/engine/src/parser/oracle_static"

CATEGORIES: dict[str, list[str]] = {
    "mana_transform": [
        "try_parse_transform_unspent_mana_static",
        "try_parse_retain_unspent_mana_static",
    ],
    "cost_mod": [
        "parse_activated_cost_reduction_minimum_mana",
        "parse_pay_life_as_colored_mana",
        "parse_cost_payment_prohibition_statics",
        "parse_spells_alternative_cost",
    ],
    "loyalty": [
        "parse_self_loyalty_activation_permission",
        "parse_loyalty_activation_timing_permission",
    ],
    "type_change": [
        "parse_arcane_adaptation_chosen_type_static",
        "parse_every_creature_type_static",
        "parse_chosen_creature_type_static",
        "parse_self_chosen_type_static",
        "parse_collection_counter_play_permission_static",
        "parse_enchanted_is_type",
        "parse_additive_type",
        "parse_bare_becomes_type",
        "parse_pronoun_becomes_type",
        "parse_all_creature_types_grant",
        "parse_land_type_change",
        "parse_creature_type_change",
        "try_parse_self_is_also_subtypes",
        "parse_becomes_type_addition",
        "parse_all_permanents_are_type",
        "parse_all_subject_are_color",
        "parse_each_noncreature_subject_is_creature_with_pt_mv",
        "parse_additive_type_clause_modifications",
        "parse_chosen_creature_type_static_sentence",
        "parse_chosen_creature_type_static_prefix",
        "parse_chosen_creature_type_static_subject",
        "parse_every_creature_type_static_sentence",
        "parse_every_creature_type_static_prefix",
        "core_type_from_additive_word",
    ],
    "restriction": [
        "parse_cast_and_activate_only_during",
        "parse_per_player_conditional_prohibition",
        "parse_cant_cast",
        "parse_passive_cant_be_cast",
        "parse_temporal_prefix_cant_cast",
        "parse_enchanted_controller_cant_cast",
        "parse_per_turn_cast_limit",
        "parse_conditional_subject_per_turn_cast_limit",
        "parse_cant_draw",
        "parse_cant_enter_battlefield",
        "parse_cant_search_library",
        "parse_suppress_triggers",
        "strip_casting_prohibition_subject",
        "strip_controller_possessive_scope",
        "parse_filter_scoped_cant_be_activated",
        "parse_cant_be_activated_exemption",
        "parse_activation_exemption_suffix",
        "parse_cant_be_countered",
        "parse_legend_rule",
        "parse_per_turn_draw_limit",
        "try_parse_top_of_library_cast_permission",
        "try_parse_graveyard_cast_permission",
        "try_parse_exile_cast_permission",
        "try_parse_cast_free_permission",
        "try_parse_max_hand_size",
    ],
    "evasion": [
        "parse_min_blockers_phrase",
        "parse_source_power_block_restriction",
        "classify_block_exception",
        "parse_max_combat_creatures_static",
        "parse_can_attack_despite_defender",
        "parse_assign_damage_as_though_unblocked",
        "parse_attached_creature_assign_damage_as_though_unblocked",
        "try_parse_ignore_landwalk_for_blocking",
        "try_split_and_can_attack_despite_defender",
        "try_split_and_must_attack_block",
        "parse_subject_combat_rule_static",
        "parse_combat_tax",
        "parse_assigns_damage_from_toughness",
        "parse_attached_assigns_damage_from_toughness",
        "parse_activate_abilities_as_though_haste",
        "try_parse_scoped_must_attack_block",
        "parse_doubler_source_filter",
        "parse_subject_rule_static",
        "parse_compound_subject_rule_static",
        "parse_compound_subject_keyword_static",
        "parse_rule_static_separator_nom",
        "parse_property_descriptor",
        "try_parse_compound_subtypes",
    ],
    "keyword_grant": [
        "try_parse_graveyard_keyword_grant_clause",
        "parse_spells_have_keyword",
        "parse_continuous_modifications",
        "parse_quoted_ability_modifications",
        "classify_quoted",
        "split_keyword_list",
        "parse_keyword_with_where_x",
        "parse_spells_have_keyword_for_test",
        "apply_spell_keyword_subject_constraints",
        "parse_chosen_qualifier_subject",
        "push_grant_clause_modifications",
        "parse_compound_subject",
        "RuleStaticPredicate",
    ],
    "anthem": [
        "parse_continuous_gets_has",
        "parse_dynamic_pt",
        "parse_dynamic_for_each_pt",
        "parse_base_pt",
        "parse_contextual_continuous",
        "parse_subject_continuous",
        "parse_subject_rule_static",
        "parse_typed_you_control",
        "parse_compound_turn_counter_animation",
        "parse_animation_modifications",
        "parse_subject_additive_type_static",
        "parse_compound_subject",
        "parse_property_descriptor",
        "parse_conditional_static",
        "parse_soulbond_paired_static",
        "parse_controlled_compound_continuous",
        "parse_contextual_continuous_subject_static",
        "continuous_subject_verb",
        "predicate_condition",
        "contextual_continuous_subject_filter",
        "parse_controlled_compound_continuous_subject_filter",
        "parse_counter_minimum",
        "bind_where_x_in_quantity_expr",
    ],
    "cda": ["parse_cda"],
}

DISPATCH_NAMES = {"parse_static_line_inner", "InvertedAsLongAs"}
DISPATCH_LINE_RANGE = (874, 2613)  # inclusive 1-based: `parse_static_line_inner` through closing `}`
# Public API lives in mod.rs only.
MOD_API = {"parse_static_line", "parse_static_line_ir", "lower_static_ir"}

MODULE_CR: dict[str, str] = {
    "shared": "// CR 604 / CR 613 — shared static parser infrastructure.\n",
    "dispatch": "// CR 604 — `parse_static_line_inner` category dispatch.\n",
    "keyword_grant": "// CR 613.3f (Layer 6) — keyword-grant static abilities.\n",
    "anthem": "// CR 613.3g (Layer 7) — P/T anthem static abilities.\n",
    "type_change": "// CR 613.3d (Layer 4) — type-changing static abilities.\n",
    "restriction": "// CR 601.3 — casting/activation restriction statics.\n",
    "evasion": "// CR 509.1b — combat restriction / evasion statics.\n",
    "cost_mod": "// CR 601.2e — cost modification static abilities.\n",
    "cda": "// CR 604.3 — characteristic-defining ability statics.\n",
    "loyalty": "// CR 606.3 — planeswalker loyalty activation statics.\n",
    "mana_transform": "// CR 613.3 — mana transformation static abilities.\n",
}

# `body` include order: shared first, then first-seen category modules.
BODY_INCLUDE_ORDER: list[str] = []


def module_for(name: str, kind: str) -> str:
    if name in DISPATCH_NAMES or name == "parse_static_line_inner":
        return "dispatch"
    if kind == "enum" and name == "InvertedAsLongAs":
        return "dispatch"
    best = "shared"
    best_len = 0
    for mod, prefixes in CATEGORIES.items():
        for p in prefixes:
            if (name == p or name.startswith(p)) and len(p) > best_len:
                best = mod
                best_len = len(p)
    return best


def top_level_items(lines: list[str]) -> list[tuple[int, int, str, str]]:
    """Return (start_line_1based, end_line_1based, kind, name) for each top-level item."""
    pat = re.compile(r"^(pub(?:\(crate\))? )?(fn|enum|struct) (\w+)")
    starts: list[tuple[int, str, str, str]] = []
    for i, line in enumerate(lines):
        m = pat.match(line)
        if m:
            starts.append((i + 1, m.group(2), m.group(3), line))
    items = []
    for j, (start, kind, name, _) in enumerate(starts):
        end = starts[j + 1][0] - 1 if j + 1 < len(starts) else len(lines)
        # include preceding attributes
        attr_start = start - 1
        while attr_start > 0:
            prev = lines[attr_start - 1].strip()
            if prev.startswith("#[") or prev.startswith("///"):
                attr_start -= 1
            else:
                break
        items.append((attr_start, end, kind, name))
    return items


MAX_MODULE_BYTES = 80 * 1024


def shard_module_file(path: Path, max_bytes: int = MAX_MODULE_BYTES) -> list[str]:
    """Split an oversized module file into include shards (each <= max_bytes)."""
    raw = path.read_text()
    if len(raw.encode()) <= max_bytes:
        return [path.name]

    lines = raw.splitlines(keepends=True)
    header_end = 0
    for i, line in enumerate(lines):
        if re.match(r"^//", line) or (not line.strip() and i < 5):
            header_end = i + 1
        elif re.match(r"^(pub(?:\(crate\))? )?(fn|enum|struct) ", line):
            break
    header = lines[:header_end]
    body = lines[header_end:]

    fn_starts: list[int] = []
    for i, line in enumerate(body):
        if re.match(r"^(pub(?:\(crate\))? )?(fn|enum|struct) ", line):
            attr = i
            while attr > 0 and (
                body[attr - 1].strip().startswith("#[")
                or body[attr - 1].strip().startswith("///")
            ):
                attr -= 1
            fn_starts.append(attr)
    if not fn_starts:
        return [path.name]

    segments: list[list[str]] = []
    current: list[str] = list(header)
    current_size = sum(len(l.encode()) for l in current)

    for j, start in enumerate(fn_starts):
        end = fn_starts[j + 1] if j + 1 < len(fn_starts) else len(body)
        seg = body[start:end]
        seg_size = sum(len(l.encode()) for l in seg)
        if current_size + seg_size > max_bytes and len(current) > len(header):
            segments.append(current)
            current = list(header)
            current_size = sum(len(l.encode()) for l in current)
        current.extend(seg)
        current_size += seg_size
    if current:
        segments.append(current)

    stem = path.stem
    names: list[str] = []
    for idx, seg_lines in enumerate(segments):
        name = f"{stem}.rs" if idx == 0 else f"{stem}_{idx + 1}.rs"
        (path.parent / name).write_text("".join(seg_lines))
        names.append(name)
        kb = (path.parent / name).stat().st_size / 1024
        print(f"  {name}: {kb:.1f} KB (sharded)")
    return names


def main() -> None:
    text = SRC.read_text()
    lines = text.splitlines(keepends=True)

    # Tests: main `mod tests` block (not the small `#[cfg(test)]` helpers mid-file).
    test_start = next(
        (
            i
            for i, line in enumerate(lines)
            if line.strip() == "#[cfg(test)]"
            and i + 1 < len(lines)
            and lines[i + 1].strip().startswith("mod tests")
        ),
        len(lines),
    )
    prod_lines = lines[:test_start]
    test_lines = lines[test_start:]

    items_for_preamble = top_level_items([l.rstrip("\n") for l in prod_lines])
    # Imports only — exclude doc comments attached to the first item.
    preamble_end = items_for_preamble[0][0] - 1 if items_for_preamble else 0
    preamble = prod_lines[:preamble_end]

    all_items = top_level_items([l.rstrip("\n") for l in prod_lines])

    by_mod: dict[str, list[str]] = {}
    include_order: list[str] = []
    for j, (start, _end, kind, name) in enumerate(all_items):
        if name == "parse_static_line_inner" or name in MOD_API:
            continue
        end = all_items[j + 1][0] - 1 if j + 1 < len(all_items) else len(prod_lines)
        mod = module_for(name, kind)
        chunk = "".join(prod_lines[start - 1 : end])
        by_mod.setdefault(mod, []).append(chunk)
        if mod not in include_order and mod != "dispatch":
            include_order.append(mod)

    # Full dispatch block: InvertedAsLongAs enum + inner (lines 648-651 enum may be separate item)
    # Inclusive 1-based end → exclusive 0-based slice end.
    dispatch_inner = "".join(
        prod_lines[DISPATCH_LINE_RANGE[0] - 1 : DISPATCH_LINE_RANGE[1]]
    )
    dispatch_extra = by_mod.pop("dispatch", [])
    dispatch_body = "".join(dispatch_extra) + dispatch_inner
    # Parent `mod.rs` must call into dispatch.
    dispatch_body = dispatch_body.replace(
        "fn parse_static_line_inner(",
        "pub(crate) fn parse_static_line_inner(",
        1,
    )
    dispatch_body = dispatch_body.replace(
        "enum InvertedAsLongAs {",
        "pub(crate) enum InvertedAsLongAs {",
        1,
    )

    OUT.mkdir(parents=True, exist_ok=True)

    # Per-category source files (included into `body` for a single namespace).
    for mod, chunks in sorted(by_mod.items()):
        header = MODULE_CR.get(mod, f"//! {mod}\n")
        path = OUT / f"{mod}.rs"
        path.write_text(header + "".join(chunks))
        kb = path.stat().st_size / 1024
        print(f"  {mod}.rs: {kb:.1f} KB")
        if kb > 80:
            print(f"    WARNING: exceeds 80 KB")

    dispatch_header = MODULE_CR["dispatch"]
    dispatch_path = OUT / "dispatch.rs"
    dispatch_body = dispatch_body.replace(
        "super::oracle_quantity::",
        "crate::parser::oracle_quantity::",
    )
    dispatch_content = dispatch_header + "use super::*;\n\n" + dispatch_body
    dispatch_path.write_text(dispatch_content)
    dkb = dispatch_path.stat().st_size / 1024
    print(f"  dispatch.rs: {dkb:.1f} KB")
    if dkb > 80:
        print("    WARNING: dispatch exceeds 80 KB")

    # Shard any module file over the 80 KiB limit.
    final_includes: list[str] = []
    for mod in ["shared"] + [m for m in include_order if m != "shared"]:
        path = OUT / f"{mod}.rs"
        if not path.exists():
            continue
        for name in shard_module_file(path):
            if name not in final_includes:
                final_includes.append(name)

    category_includes = "\n".join(f'include!("{name}");' for name in final_includes)

    # mod.rs: imports + category includes at module scope + dispatch submodule
    mod_rs = (
        "//! Oracle static ability parser (CR 604 / CR 613).\n\n"
        + "".join(preamble)
        + "\n"
        + category_includes
        + "\nmod dispatch;\n\n"
        + """use dispatch::{parse_static_line_inner, InvertedAsLongAs};

/// Parse a static/continuous ability line into a `StaticDefinition`.
#[tracing::instrument(level = "debug")]
pub fn parse_static_line(text: &str) -> Option<crate::types::ability::StaticDefinition> {
    let ir = parse_static_line_ir(text)?;
    Some(lower_static_ir(&ir))
}

/// IR production: parse a static line into `StaticIr` (pre-lowering).
pub(crate) fn parse_static_line_ir(text: &str) -> Option<StaticIr> {
    let definition = parse_static_line_inner(text, InvertedAsLongAs::Allow)?;
    Some(StaticIr {
        definition,
        source_text: text.to_string(),
        body_ir: None,
    })
}

/// Lowering: apply post-parse transforms to produce the final `StaticDefinition`.
pub(crate) fn lower_static_ir(ir: &StaticIr) -> crate::types::ability::StaticDefinition {
    let mut def = ir.definition.clone();
    populate_active_zones_from_condition(&mut def);
    def
}

"""
    )
    # tests in separate include
    tests_path = OUT / "tests.inc.rs"
    tests_path.write_text("".join(test_lines))
    tkb = tests_path.stat().st_size / 1024
    print(f"  tests.inc.rs: {tkb:.1f} KB")

    mod_rs += "\n#[cfg(test)]\ninclude!(\"tests.inc.rs\");\n"
    (OUT / "mod.rs").write_text(mod_rs)
    mkb = (OUT / "mod.rs").stat().st_size / 1024
    print(f"  mod.rs: {mkb:.1f} KB")
    print("Done. Delete oracle_static.rs — parser/mod.rs already uses `pub mod oracle_static;`.")


if __name__ == "__main__":
    main()
