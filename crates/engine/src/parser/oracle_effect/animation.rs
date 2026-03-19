use std::str::FromStr;

use super::token::{
    map_token_keyword, push_unique_string, split_token_keyword_list, title_case_word,
};
use super::types::*;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;

pub(super) fn parse_animation_spec(text: &str) -> Option<AnimationSpec> {
    let lower = text.to_lowercase();
    if lower.contains(" copy of ")
        || lower.contains(" of your choice")
        || lower.contains(" all activated abilities ")
        || lower.contains(" loses all other card types ")
        || lower.contains(" all colors")
    {
        return None;
    }

    let mut spec = AnimationSpec::default();
    let mut rest = text.trim().trim_end_matches('.');

    for suffix in [
        " and loses all other abilities",
        " and it loses all other abilities",
        " and loses all abilities",
    ] {
        if rest.to_lowercase().ends_with(suffix) {
            let end = rest.len() - suffix.len();
            rest = rest[..end].trim_end_matches(',').trim();
            spec.remove_all_abilities = true;
            break;
        }
    }

    if let Some(stripped) = rest.strip_prefix("a ") {
        rest = stripped;
    } else if let Some(stripped) = rest.strip_prefix("an ") {
        rest = stripped;
    }

    if let Some((power, toughness, after_pt)) = parse_fixed_become_pt_prefix(rest) {
        spec.power = Some(power);
        spec.toughness = Some(toughness);
        rest = after_pt;
    }

    if let Some((descriptor, power, toughness)) = split_animation_base_pt_clause(rest) {
        spec.power = Some(power);
        spec.toughness = Some(toughness);
        rest = descriptor;
    }

    let (descriptor, keywords) = split_animation_keyword_clause(rest);
    spec.keywords = keywords;
    rest = descriptor;

    if let Some((colors, after_colors)) = parse_animation_color_prefix(rest) {
        spec.colors = Some(colors);
        rest = after_colors;
    }

    spec.types = parse_animation_types(rest, spec.power.is_some() || spec.toughness.is_some());

    if spec.power.is_none()
        && spec.toughness.is_none()
        && spec.colors.is_none()
        && spec.keywords.is_empty()
        && spec.types.is_empty()
        && !spec.remove_all_abilities
    {
        None
    } else {
        Some(spec)
    }
}

pub(super) fn animation_modifications(
    spec: &AnimationSpec,
) -> Vec<crate::types::ability::ContinuousModification> {
    use crate::types::ability::ContinuousModification;
    use crate::types::card_type::CoreType;

    let mut modifications = Vec::new();

    if let Some(power) = spec.power {
        modifications.push(ContinuousModification::SetPower { value: power });
    }
    if let Some(toughness) = spec.toughness {
        modifications.push(ContinuousModification::SetToughness { value: toughness });
    }
    if let Some(colors) = &spec.colors {
        modifications.push(ContinuousModification::SetColor {
            colors: colors.clone(),
        });
    }
    if spec.remove_all_abilities {
        modifications.push(ContinuousModification::RemoveAllAbilities);
    }
    for keyword in &spec.keywords {
        modifications.push(ContinuousModification::AddKeyword {
            keyword: keyword.clone(),
        });
    }
    for type_name in &spec.types {
        if let Ok(core_type) = CoreType::from_str(type_name) {
            modifications.push(ContinuousModification::AddType { core_type });
        } else {
            modifications.push(ContinuousModification::AddSubtype {
                subtype: type_name.clone(),
            });
        }
    }

    modifications
}

fn parse_animation_color_prefix(text: &str) -> Option<(Vec<ManaColor>, &str)> {
    let mut rest = text.trim_start();
    let mut saw_color = false;
    let mut colors = Vec::new();

    loop {
        if let Some(stripped) = strip_prefix_word(rest, "colorless") {
            saw_color = true;
            rest = stripped;
        } else if let Some(stripped) = strip_prefix_word(rest, "white") {
            saw_color = true;
            colors.push(ManaColor::White);
            rest = stripped;
        } else if let Some(stripped) = strip_prefix_word(rest, "blue") {
            saw_color = true;
            colors.push(ManaColor::Blue);
            rest = stripped;
        } else if let Some(stripped) = strip_prefix_word(rest, "black") {
            saw_color = true;
            colors.push(ManaColor::Black);
            rest = stripped;
        } else if let Some(stripped) = strip_prefix_word(rest, "red") {
            saw_color = true;
            colors.push(ManaColor::Red);
            rest = stripped;
        } else if let Some(stripped) = strip_prefix_word(rest, "green") {
            saw_color = true;
            colors.push(ManaColor::Green);
            rest = stripped;
        } else {
            break;
        }

        if let Some(stripped) = rest.strip_prefix("and ") {
            rest = stripped;
            continue;
        }
        break;
    }

    saw_color.then_some((colors, rest.trim_start()))
}

fn strip_prefix_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(word)?;
    if rest.is_empty() {
        Some(rest)
    } else if rest.starts_with(' ') {
        Some(rest.trim_start())
    } else {
        None
    }
}

pub(super) fn parse_fixed_become_pt_prefix(text: &str) -> Option<(i32, i32, &str)> {
    let (power, toughness, rest) = parse_token_pt_prefix(text)?;
    match (power, toughness) {
        (
            crate::types::ability::PtValue::Fixed(power),
            crate::types::ability::PtValue::Fixed(toughness),
        ) => Some((power, toughness, rest)),
        _ => None,
    }
}

fn parse_token_pt_prefix(
    text: &str,
) -> Option<(
    crate::types::ability::PtValue,
    crate::types::ability::PtValue,
    &str,
)> {
    let text = text.trim_start();
    let word_end = text.find(char::is_whitespace).unwrap_or(text.len());
    let token = &text[..word_end];
    let slash = token.find('/')?;
    let power = token[..slash].trim();
    let toughness = token[slash + 1..].trim();
    let power = parse_token_pt_component(power)?;
    let toughness = parse_token_pt_component(toughness)?;
    Some((power, toughness, text[word_end..].trim_start()))
}

fn parse_token_pt_component(text: &str) -> Option<crate::types::ability::PtValue> {
    if text.eq_ignore_ascii_case("x") {
        return Some(crate::types::ability::PtValue::Variable("X".to_string()));
    }
    text.parse::<i32>()
        .ok()
        .map(crate::types::ability::PtValue::Fixed)
}

fn split_animation_base_pt_clause(text: &str) -> Option<(&str, i32, i32)> {
    let lower = text.to_lowercase();
    let pos = lower.find(" with base power and toughness ")?;
    let descriptor = text[..pos].trim_end_matches(',').trim();
    let pt_text = text[pos + " with base power and toughness ".len()..].trim();
    let (power, toughness, _) = parse_fixed_become_pt_prefix(pt_text)?;
    Some((descriptor, power, toughness))
}

fn parse_animation_types(text: &str, infer_creature: bool) -> Vec<String> {
    let descriptor = text
        .trim()
        .trim_end_matches(',')
        .trim_end_matches(" in addition to its other types")
        .trim();
    if descriptor.is_empty() {
        return Vec::new();
    }

    let mut core_types = Vec::new();
    let mut subtypes = Vec::new();

    for word in descriptor.split_whitespace() {
        match word.to_lowercase().as_str() {
            "artifact" => push_unique_string(&mut core_types, "Artifact"),
            "creature" => push_unique_string(&mut core_types, "Creature"),
            "enchantment" => push_unique_string(&mut core_types, "Enchantment"),
            "land" => push_unique_string(&mut core_types, "Land"),
            "planeswalker" => push_unique_string(&mut core_types, "Planeswalker"),
            "legendary" | "basic" | "snow" | "" => {}
            other => subtypes.push(title_case_word(other)),
        }
    }

    if core_types.is_empty() && subtypes.is_empty() {
        return Vec::new();
    }
    if core_types.is_empty() && infer_creature {
        push_unique_string(&mut core_types, "Creature");
    }

    let mut types = core_types;
    for subtype in subtypes {
        push_unique_string(&mut types, subtype);
    }

    types
}

fn split_animation_keyword_clause(text: &str) -> (&str, Vec<Keyword>) {
    let lower = text.to_lowercase();
    let Some(pos) = lower.find(" with ") else {
        return (text, Vec::new());
    };

    let prefix = text[..pos].trim_end_matches(',').trim();
    let keyword_text = text[pos + 6..]
        .split('"')
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches('.');
    let keywords = split_token_keyword_list(keyword_text)
        .into_iter()
        .filter_map(map_token_keyword)
        .collect();
    (prefix, keywords)
}
