use crate::types::ability::{
    Effect, ManaProduction, ManaSpendRestriction, QuantityExpr, QuantityRef,
};
use crate::types::mana::ManaColor;

use super::super::oracle_quantity::parse_cda_quantity;
use super::super::oracle_util::{parse_mana_production, parse_number, TextPair};

pub(super) fn try_parse_add_mana_effect(text: &str) -> Option<Effect> {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    if !lower.starts_with("add ") {
        return None;
    }

    let clause = trimmed[4..].trim();
    let clause_lower = clause.to_lowercase();
    let clause_tp = TextPair::new(clause, &clause_lower);
    let (without_where_x, where_x_expression) = super::strip_trailing_where_x(clause_tp);
    let clause = without_where_x.original.trim().trim_end_matches(['.', '"']);
    // Strip "an additional " modifier — e.g. "add an additional {G}" → "{G}"
    let clause = clause.strip_prefix("an additional ").unwrap_or(clause);

    if let Some(produced) = parse_mana_production_clause(clause) {
        return Some(Effect::Mana {
            produced,
            restrictions: vec![],
            expiry: None,
        });
    }

    // CR 106.1 / CR 106.3: "an amount of {color} equal to [quantity]"
    // e.g. "an amount of {G} equal to ~'s power"
    if let Some(effect) = try_parse_amount_equal_to(clause) {
        return Some(effect);
    }

    if let Some((count, rest)) = parse_mana_count_prefix(clause) {
        let count = apply_where_x_count_expression(count, where_x_expression.as_deref());
        let rest = rest.trim().trim_end_matches(['.', '"']).trim();
        let rest_lower = rest.to_lowercase();

        if rest_lower.starts_with("mana of any one color")
            || rest_lower.starts_with("mana of any color")
        {
            return Some(Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count,
                    color_options: all_mana_colors(),
                },
                restrictions: vec![],
                expiry: None,
            });
        }

        if rest_lower.starts_with("mana in any combination of colors") {
            return Some(Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count,
                    color_options: all_mana_colors(),
                },
                restrictions: vec![],
                expiry: None,
            });
        }

        if rest_lower.starts_with("mana of the chosen color")
            || rest_lower.starts_with("mana of that color")
        {
            return Some(Effect::Mana {
                produced: ManaProduction::ChosenColor { count },
                restrictions: vec![],
                expiry: None,
            });
        }

        // CR 106.1: "[count] {color}" → single color repeated (e.g., "six {G}" → 6 Green)
        if let Some((colors, after)) = parse_mana_production(rest) {
            let after = after.trim();
            if !colors.is_empty() && (after.is_empty() || after == ".") {
                // Single color repeated N times
                if colors.len() == 1 {
                    return Some(Effect::Mana {
                        produced: ManaProduction::AnyOneColor {
                            count,
                            color_options: colors,
                        },
                        restrictions: vec![],
                        expiry: None,
                    });
                }
            }
        }

        const ANY_COMBINATION_PREFIX: &str = "mana in any combination of ";
        if rest_lower.starts_with(ANY_COMBINATION_PREFIX) {
            let color_set_text = rest[ANY_COMBINATION_PREFIX.len()..].trim();
            if let Some(color_options) = parse_mana_color_set(color_set_text) {
                return Some(Effect::Mana {
                    produced: ManaProduction::AnyCombination {
                        count,
                        color_options,
                    },
                    restrictions: vec![],
                    expiry: None,
                });
            }
        }
    }

    let clause_lower = clause.to_lowercase();
    let fallback_count = parse_mana_count_prefix(clause)
        .map(|(count, _)| count)
        .unwrap_or(QuantityExpr::Fixed { value: 1 });
    let fallback_count =
        apply_where_x_count_expression(fallback_count, where_x_expression.as_deref());

    if clause_lower.contains("mana of any one color") || clause_lower.contains("mana of any color")
    {
        return Some(Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count: fallback_count,
                color_options: all_mana_colors(),
            },
            restrictions: vec![],
            expiry: None,
        });
    }

    if clause_lower.contains("mana in any combination of colors") {
        return Some(Effect::Mana {
            produced: ManaProduction::AnyCombination {
                count: fallback_count,
                color_options: all_mana_colors(),
            },
            restrictions: vec![],
            expiry: None,
        });
    }

    if clause_lower.contains("mana of the chosen color")
        || clause_lower.contains("mana of that color")
    {
        return Some(Effect::Mana {
            produced: ManaProduction::ChosenColor {
                count: fallback_count,
            },
            restrictions: vec![],
            expiry: None,
        });
    }

    None
}

pub(super) fn try_parse_activate_only_condition(text: &str) -> Option<Effect> {
    let trimmed = text.trim().trim_end_matches('.');
    let lower = trimmed.to_ascii_lowercase();
    let prefix = "activate only if you control ";
    if !lower.starts_with(prefix) {
        return None;
    }

    let raw = &lower[prefix.len()..];
    let mut subtypes = Vec::new();
    for part in raw.split(" or ") {
        let token = part
            .trim()
            .trim_start_matches("a ")
            .trim_start_matches("an ")
            .trim();
        let subtype = match token {
            "plains" => "Plains",
            "island" => "Island",
            "swamp" => "Swamp",
            "mountain" => "Mountain",
            "forest" => "Forest",
            _ => return None,
        };
        if !subtypes.contains(&subtype) {
            subtypes.push(subtype);
        }
    }

    if subtypes.is_empty() {
        return None;
    }

    Some(Effect::Unimplemented {
        name: "activate_only_if_controls_land_subtype_any".to_string(),
        description: Some(subtypes.join("|")),
    })
}

pub(super) fn parse_mana_production_clause(text: &str) -> Option<ManaProduction> {
    if let Some(color_options) = parse_mana_color_set(text) {
        if color_options.len() > 1 {
            return Some(ManaProduction::AnyOneColor {
                count: QuantityExpr::Fixed { value: 1 },
                color_options,
            });
        }
    }

    if let Some((colors, remainder)) = parse_mana_production(text) {
        let remainder = remainder.trim().trim_end_matches(['.', '"']).trim();
        if remainder.is_empty() {
            return Some(ManaProduction::Fixed { colors });
        }
        // CR 106.1: "{color} for each [filter]" → dynamic mana count
        if let Some(for_each_clause) = remainder.strip_prefix("for each ") {
            let qty = super::super::oracle_quantity::parse_for_each_clause(for_each_clause)?;
            return Some(ManaProduction::AnyOneColor {
                count: QuantityExpr::Ref { qty },
                color_options: colors,
            });
        }
        // Unknown trailing text — don't silently discard it
        return None;
    }

    if let Some((count, remainder)) = parse_colorless_mana_production(text) {
        let remainder = remainder.trim().trim_end_matches(['.', '"']).trim();
        if remainder.is_empty() {
            return Some(ManaProduction::Colorless { count });
        }
        // CR 106.1: "{C} for each [filter]" → dynamic colorless mana count
        if let Some(for_each_clause) = remainder.strip_prefix("for each ") {
            let qty = super::super::oracle_quantity::parse_for_each_clause(for_each_clause)?;
            return Some(ManaProduction::Colorless {
                count: QuantityExpr::Ref { qty },
            });
        }
        return None;
    }

    None
}

pub(super) fn parse_colorless_mana_production(text: &str) -> Option<(QuantityExpr, &str)> {
    let mut rest = text.trim_start();
    let mut count = 0i32;

    while rest.starts_with('{') {
        let end = rest.find('}')?;
        let symbol = &rest[1..end];
        if !symbol.eq_ignore_ascii_case("C") {
            break;
        }
        count += 1;
        rest = rest[end + 1..].trim_start();
    }

    if count == 0 {
        return None;
    }

    Some((QuantityExpr::Fixed { value: count }, rest))
}

pub(super) fn parse_mana_count_prefix(text: &str) -> Option<(QuantityExpr, &str)> {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("X ") {
        return Some((
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            rest.trim_start(),
        ));
    }
    if let Some(rest) = trimmed.strip_prefix("x ") {
        return Some((
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            rest.trim_start(),
        ));
    }
    let (count, rest) = parse_number(trimmed)?;
    Some((
        QuantityExpr::Fixed {
            value: count as i32,
        },
        rest,
    ))
}

pub(super) fn apply_where_x_count_expression(
    count: QuantityExpr,
    where_x_expression: Option<&str>,
) -> QuantityExpr {
    match (&count, where_x_expression) {
        (
            QuantityExpr::Ref {
                qty: QuantityRef::Variable { ref name },
            },
            Some(expression),
        ) if name.eq_ignore_ascii_case("X") => {
            crate::parser::oracle_quantity::parse_cda_quantity(expression).unwrap_or_else(|| {
                QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: expression.to_string(),
                    },
                }
            })
        }
        _ => count,
    }
}

pub(super) fn parse_mana_color_set(text: &str) -> Option<Vec<ManaColor>> {
    let mut rest = text.trim().trim_end_matches(['.', '"']).trim();
    if rest.is_empty() {
        return None;
    }

    let mut colors = Vec::new();
    loop {
        let (parsed, after_symbol) = parse_mana_color_symbol(rest)?;
        for color in parsed {
            if !colors.contains(&color) {
                colors.push(color);
            }
        }

        let next = after_symbol.trim_start();
        if next.is_empty() {
            break;
        }

        if let Some(stripped) = next.strip_prefix("and/or ") {
            rest = stripped.trim_start();
            continue;
        }
        if let Some(stripped) = next.strip_prefix("or ") {
            rest = stripped.trim_start();
            continue;
        }
        if let Some(stripped) = next.strip_prefix("and ") {
            rest = stripped.trim_start();
            continue;
        }
        if let Some(stripped) = next.strip_prefix(',') {
            let stripped = stripped.trim_start();
            if let Some(after_or) = stripped.strip_prefix("or ") {
                rest = after_or.trim_start();
                continue;
            }
            if let Some(after_and_or) = stripped.strip_prefix("and/or ") {
                rest = after_and_or.trim_start();
                continue;
            }
            if let Some(after_and) = stripped.strip_prefix("and ") {
                rest = after_and.trim_start();
                continue;
            }
            rest = stripped;
            continue;
        }
        if let Some(stripped) = next.strip_prefix('/') {
            rest = stripped.trim_start();
            continue;
        }

        return None;
    }

    if colors.is_empty() {
        None
    } else {
        Some(colors)
    }
}

pub(super) fn parse_mana_color_symbol(text: &str) -> Option<(Vec<ManaColor>, &str)> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    let end = trimmed.find('}')?;
    let symbol = &trimmed[1..end];
    let colors = parse_mana_color_symbol_set(symbol)?;
    Some((colors, &trimmed[end + 1..]))
}

pub(super) fn parse_mana_color_symbol_set(symbol: &str) -> Option<Vec<ManaColor>> {
    fn parse_single(code: &str) -> Option<ManaColor> {
        match code {
            "W" => Some(ManaColor::White),
            "U" => Some(ManaColor::Blue),
            "B" => Some(ManaColor::Black),
            "R" => Some(ManaColor::Red),
            "G" => Some(ManaColor::Green),
            _ => None,
        }
    }

    let symbol = symbol.trim().to_ascii_uppercase();
    if let Some(color) = parse_single(&symbol) {
        return Some(vec![color]);
    }

    let mut colors = Vec::new();
    for part in symbol.split('/') {
        let color = parse_single(part.trim())?;
        if !colors.contains(&color) {
            colors.push(color);
        }
    }

    if colors.is_empty() {
        None
    } else {
        Some(colors)
    }
}

pub(super) fn all_mana_colors() -> Vec<ManaColor> {
    vec![
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ]
}

/// Parse a "Spend this mana only to cast..." clause into a `ManaSpendRestriction`.
///
/// Handles patterns like:
/// - "spend this mana only to cast creature spells" → SpellType("Creature")
/// - "spend this mana only to cast a creature spell of the chosen type" → ChosenCreatureType
/// - "spend this mana only to cast a creature spell of the chosen type, and that spell can't be countered" → ChosenCreatureType
pub(crate) fn parse_mana_spend_restriction(lower: &str) -> Option<ManaSpendRestriction> {
    let base = lower.strip_prefix("spend this mana only ")?;
    let base = base.trim_end_matches(['.', '"']);

    // "spend this mana only to activate abilities" — activation-only
    if base.starts_with("to activate abilities") {
        return Some(ManaSpendRestriction::ActivateOnly);
    }

    // "spend this mana only on costs that include {x}" — X-cost restriction
    if base.starts_with("on costs that include") {
        return Some(ManaSpendRestriction::XCostOnly);
    }

    let rest = base.strip_prefix("to cast ")?.trim();

    // Strip trailing ", and that spell can't be countered" or similar trailing clauses
    let rest = rest.split(", and ").next().unwrap_or(rest).trim();

    // CR 106.12: Check for "or activate abilities of [type]" suffix.
    // If present, emit a combined SpellTypeOrAbilityActivation restriction.
    let has_ability_activation = rest.contains(" or activate abilities");
    let spell_part = rest
        .split(" or activate abilities")
        .next()
        .unwrap_or(rest)
        .trim();

    if spell_part.contains("of the chosen type") {
        return Some(ManaSpendRestriction::ChosenCreatureType);
    }

    // "creature spells" / "a creature spell" / "artifact spells" etc.
    let spell_part = spell_part
        .strip_prefix("a ")
        .or_else(|| spell_part.strip_prefix("an "))
        .unwrap_or(spell_part);

    // Handle compound type: "instant or sorcery spells" → "Instant or Sorcery"
    // Check for "[type] or [type] spell(s)" pattern
    if let Some((first, second_with_spells)) = spell_part.split_once(" or ") {
        let second = second_with_spells
            .strip_suffix(" spells")
            .or_else(|| second_with_spells.strip_suffix(" spell"))
            .unwrap_or(second_with_spells);
        // Only treat as compound if second part is a single type word
        if !second.contains(' ') || second.ends_with("creature") {
            let compound = format!(
                "{} or {}",
                super::capitalize(first),
                super::capitalize(second)
            );
            if has_ability_activation {
                return Some(ManaSpendRestriction::SpellTypeOrAbilityActivation(compound));
            }
            return Some(ManaSpendRestriction::SpellType(compound));
        }
    }

    let type_word = spell_part.split_whitespace().next()?;
    let type_name = super::capitalize(type_word);

    if has_ability_activation {
        Some(ManaSpendRestriction::SpellTypeOrAbilityActivation(
            type_name,
        ))
    } else {
        Some(ManaSpendRestriction::SpellType(type_name))
    }
}

/// CR 106.1 / CR 106.3: Parse "an amount of {color} equal to [quantity]"
/// e.g. "an amount of {G} equal to ~'s power" → AnyOneColor { count: SelfPower, [Green] }
fn try_parse_amount_equal_to(clause: &str) -> Option<Effect> {
    let rest = clause.strip_prefix("an amount of ")?;

    // Parse the mana color symbol(s): "{G}", "{R}", etc.
    let (colors, after_color) = parse_mana_production(rest)?;
    if colors.is_empty() {
        return None;
    }

    // Expect "equal to [quantity]"
    let after_color = after_color.trim();
    let quantity_text = after_color.strip_prefix("equal to ")?;
    let quantity_text = quantity_text.trim().trim_end_matches(['.', '"']);

    let count = parse_cda_quantity(quantity_text)?;

    let color_options: Vec<ManaColor> = colors;
    Some(Effect::Mana {
        produced: ManaProduction::AnyOneColor {
            count,
            color_options,
        },
        restrictions: vec![],
        expiry: None,
    })
}
