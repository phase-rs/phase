use crate::types::ability::{CountValue, Effect, ManaProduction, ManaSpendRestriction};
use crate::types::mana::ManaColor;

use super::super::oracle_util::{parse_mana_production, parse_number};

pub(super) fn try_parse_add_mana_effect(text: &str) -> Option<Effect> {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    if !lower.starts_with("add ") {
        return None;
    }

    let clause = trimmed[4..].trim();
    let (without_where_x, where_x_expression) = super::strip_trailing_where_x(clause);
    let clause = without_where_x.trim().trim_end_matches(['.', '"']);

    if let Some(produced) = parse_mana_production_clause(clause) {
        return Some(Effect::Mana {
            produced,
            restrictions: vec![],
        });
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
            });
        }

        if rest_lower.starts_with("mana in any combination of colors") {
            return Some(Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count,
                    color_options: all_mana_colors(),
                },
                restrictions: vec![],
            });
        }

        if rest_lower.starts_with("mana of the chosen color")
            || rest_lower.starts_with("mana of that color")
        {
            return Some(Effect::Mana {
                produced: ManaProduction::ChosenColor { count },
                restrictions: vec![],
            });
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
                });
            }
        }
    }

    let clause_lower = clause.to_lowercase();
    let fallback_count = parse_mana_count_prefix(clause)
        .map(|(count, _)| count)
        .unwrap_or(CountValue::Fixed(1));
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
        });
    }

    if clause_lower.contains("mana in any combination of colors") {
        return Some(Effect::Mana {
            produced: ManaProduction::AnyCombination {
                count: fallback_count,
                color_options: all_mana_colors(),
            },
            restrictions: vec![],
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
                count: CountValue::Fixed(1),
                color_options,
            });
        }
    }

    if let Some((colors, _)) = parse_mana_production(text) {
        return Some(ManaProduction::Fixed { colors });
    }

    if let Some((count, _)) = parse_colorless_mana_production(text) {
        return Some(ManaProduction::Colorless { count });
    }

    None
}

pub(super) fn parse_colorless_mana_production(text: &str) -> Option<(CountValue, &str)> {
    let mut rest = text.trim_start();
    let mut count = 0u32;

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

    Some((CountValue::Fixed(count), rest))
}

pub(super) fn parse_mana_count_prefix(text: &str) -> Option<(CountValue, &str)> {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("X ") {
        return Some((CountValue::Variable("X".to_string()), rest.trim_start()));
    }
    if let Some(rest) = trimmed.strip_prefix("x ") {
        return Some((CountValue::Variable("X".to_string()), rest.trim_start()));
    }
    let (count, rest) = parse_number(trimmed)?;
    Some((CountValue::Fixed(count), rest))
}

pub(super) fn apply_where_x_count_expression(
    count: CountValue,
    where_x_expression: Option<&str>,
) -> CountValue {
    match (count, where_x_expression) {
        (CountValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("X") => {
            CountValue::Variable(expression.to_string())
        }
        (count, _) => count,
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
pub(super) fn parse_mana_spend_restriction(lower: &str) -> Option<ManaSpendRestriction> {
    let rest = lower
        .strip_prefix("spend this mana only to cast ")?
        .trim_end_matches(['.', '"']);

    // Strip trailing ", and that spell can't be countered" or similar trailing clauses
    let rest = rest.split(", and ").next().unwrap_or(rest).trim();

    if rest.contains("of the chosen type") {
        return Some(ManaSpendRestriction::ChosenCreatureType);
    }

    // "creature spells" / "a creature spell" / "artifact spells" etc.
    let rest = rest
        .strip_prefix("a ")
        .or_else(|| rest.strip_prefix("an "))
        .unwrap_or(rest);
    let type_word = rest.split_whitespace().next()?;
    let type_name = super::capitalize(type_word);
    Some(ManaSpendRestriction::SpellType(type_name))
}
