use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::value;
use nom::Parser;

use crate::parser::oracle_nom::error::OracleResult;
use crate::types::ability::{Effect, FilterProp, PtValue, QuantityExpr, QuantityRef, TargetFilter};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;

use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_target::parse_target;
use super::super::oracle_util::{parse_number, strip_reminder_text, TextPair};
use super::types::*;

/// Bridge: run a nom combinator on a lowercase copy, mapping the consumed length
/// back to the original-case text to compute the correct remainder.
fn nom_on_lower<'a, T, F>(text: &'a str, lower: &str, mut parser: F) -> Option<(T, &'a str)>
where
    F: FnMut(&str) -> OracleResult<'_, T>,
{
    let (rest, result) = parser(lower).ok()?;
    let consumed = lower.len() - rest.len();
    Some((result, &text[consumed..]))
}

pub(super) fn try_parse_token(_lower: &str, text: &str) -> Option<Effect> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();

    // "create a token that's a copy of {target}"
    if lower.contains("token that's a copy of") || lower.contains("token thats a copy of") {
        let tp = TextPair::new(&text, &lower);
        let after_copy_tp = tp.strip_after("copy of ").unwrap_or(tp);
        // Handle "another target ..." -- strip "another" prefix and add FilterProp::Another
        let has_another = nom_on_lower(after_copy_tp.original, after_copy_tp.lower, |i| {
            value((), tag("another ")).parse(i)
        })
        .is_some();
        let target_text = if has_another {
            after_copy_tp.strip_prefix("another ").unwrap().original
        } else {
            after_copy_tp.original
        };
        let (mut target, _) = parse_target(target_text);
        if has_another {
            if let TargetFilter::Typed(ref mut typed) = target {
                if !typed.properties.contains(&FilterProp::Another) {
                    typed.properties.push(FilterProp::Another);
                }
            }
        }
        return Some(Effect::CopyTokenOf {
            target,
            enters_attacking: false,
            tapped: false,
        });
    }

    let after = nom_on_lower(&text, &lower, |i| value((), tag("create ")).parse(i))
        .map(|(_, rest)| rest)
        .unwrap_or(&text)
        .trim();
    let token = parse_token_description(after)?;
    Some(Effect::Token {
        name: token.name,
        power: token.power.unwrap_or(PtValue::Fixed(0)),
        toughness: token.toughness.unwrap_or(PtValue::Fixed(0)),
        types: token.types,
        colors: token.colors,
        keywords: token.keywords,
        tapped: token.tapped,
        count: token.count,
        owner: TargetFilter::Controller,
        attach_to: token.attach_to,
        enters_attacking: false,
    })
}

pub(super) fn parse_token_description(text: &str) -> Option<TokenDescription> {
    let text = text.trim().trim_end_matches('.');
    let lower = text.to_lowercase();

    // CR 303.7: Strip "attached to [target]" suffix and capture the attachment target.
    let tp = TextPair::new(text, &lower);
    let (text, attach_to) = if let Some((before, after)) = tp.split_around(" attached to ") {
        let (target, _) = parse_target(after.original);
        (before.original, Some(target))
    } else {
        (text, None)
    };

    let (mut count, leading_name, mut rest) =
        if let Some((count, rest)) = parse_token_count_prefix(text) {
            (count, None, rest)
        } else if let Some((name, rest)) = parse_named_token_preamble(text) {
            (QuantityExpr::Fixed { value: 1 }, Some(name), rest)
        } else {
            return None;
        };
    let mut tapped = false;

    loop {
        let trimmed = rest.trim_start();
        let trimmed_lower = trimmed.to_lowercase();
        if let Some((_, after)) = nom_on_lower(trimmed, &trimmed_lower, |i| {
            value((), tag("tapped ")).parse(i)
        }) {
            tapped = true;
            rest = after;
            continue;
        }
        if let Some((_, after)) = nom_on_lower(trimmed, &trimmed_lower, |i| {
            value((), tag("untapped ")).parse(i)
        }) {
            rest = after;
            continue;
        }
        break;
    }

    rest = strip_token_supertypes(rest);

    let (mut power, mut toughness, rest) =
        if let Some((power, toughness, rest)) = parse_token_pt_prefix(rest) {
            (Some(power), Some(toughness), rest)
        } else {
            (None, None, rest)
        };

    let (colors, rest) = parse_token_color_prefix(rest);
    let (descriptor, suffix) = split_token_head(rest)?;
    let (name_override, suffix) = parse_token_name_clause(suffix);
    let keywords = parse_token_keyword_clause(suffix);
    let (mut name, types) = parse_token_identity(descriptor)?;

    if let Some(name_override) = leading_name.or(name_override) {
        name = name_override;
    }

    if let Some(where_expression) = extract_token_where_x_expression(suffix) {
        if matches!(&count, QuantityExpr::Ref { qty: QuantityRef::Variable { ref name } } if name == "X")
        {
            count = crate::parser::oracle_quantity::parse_cda_quantity(&where_expression)
                .unwrap_or_else(|| QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: where_expression.clone(),
                    },
                });
        }
        if matches!(&power, Some(PtValue::Variable(alias)) if alias == "X") {
            power = Some(
                crate::parser::oracle_quantity::parse_cda_quantity(&where_expression)
                    .map(PtValue::Quantity)
                    .unwrap_or_else(|| PtValue::Variable(where_expression.clone())),
            );
        }
        if matches!(&toughness, Some(PtValue::Variable(alias)) if alias == "X") {
            toughness = Some(
                crate::parser::oracle_quantity::parse_cda_quantity(&where_expression)
                    .map(PtValue::Quantity)
                    .unwrap_or_else(|| PtValue::Variable(where_expression)),
            );
        }
    }

    if let Some(count_expression) = extract_token_count_expression(suffix) {
        if matches!(&count, QuantityExpr::Ref { qty: QuantityRef::Variable { ref name } } if name == "count")
        {
            count = crate::parser::oracle_quantity::parse_cda_quantity(&count_expression)
                .unwrap_or(QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: count_expression,
                    },
                });
        }
    }

    // CR 609.3: "for each [thing] this way" -- count from preceding zone moves.
    // Matches "for each card put into a graveyard this way", "for each creature
    // exiled this way", etc.
    {
        let suffix_lower = suffix.to_lowercase();
        if suffix_lower.contains("for each") && suffix_lower.contains("this way") {
            count = QuantityExpr::Ref {
                qty: QuantityRef::TrackedSetSize,
            };
        }
    }

    if power.is_none() || toughness.is_none() {
        if let Some(pt_expression) = extract_token_pt_expression(suffix) {
            let parsed = crate::parser::oracle_quantity::parse_cda_quantity(&pt_expression);
            power = Some(
                parsed
                    .clone()
                    .map(PtValue::Quantity)
                    .unwrap_or_else(|| PtValue::Variable(pt_expression.clone())),
            );
            toughness = Some(
                parsed
                    .map(PtValue::Quantity)
                    .unwrap_or_else(|| PtValue::Variable(pt_expression)),
            );
        }
    }

    let is_creature = types.iter().any(|token_type| token_type == "Creature");
    if is_creature && (power.is_none() || toughness.is_none()) {
        return None;
    }

    Some(TokenDescription {
        name,
        power,
        toughness,
        types,
        colors,
        keywords,
        tapped,
        count,
        attach_to,
    })
}

fn parse_token_count_prefix(text: &str) -> Option<(QuantityExpr, &str)> {
    let trimmed = text.trim_start();
    let lower = trimmed.to_lowercase();

    // "X " / "x " -> Variable X
    if let Some((_, rest)) = nom_on_lower(trimmed, &lower, |i| value((), tag("x ")).parse(i)) {
        return Some((
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            rest,
        ));
    }
    // "that many " -> EventContextAmount
    if let Some((_, rest)) =
        nom_on_lower(trimmed, &lower, |i| value((), tag("that many ")).parse(i))
    {
        return Some((
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            rest,
        ));
    }
    // "a number of " -> deferred count
    if let Some((_, rest)) =
        nom_on_lower(trimmed, &lower, |i| value((), tag("a number of ")).parse(i))
    {
        return Some((
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "count".to_string(),
                },
            },
            rest,
        ));
    }
    let (count, rest) = parse_number(trimmed)?;
    if count == 0 && trimmed.starts_with(['x', 'X']) {
        return None;
    }
    Some((
        QuantityExpr::Fixed {
            value: count as i32,
        },
        rest,
    ))
}

fn parse_named_token_preamble(text: &str) -> Option<(String, &str)> {
    let comma = text.find(',')?;
    let name = text[..comma].trim().trim_matches('"');
    if name.is_empty() {
        return None;
    }

    let after_comma = text[comma + 1..].trim_start();
    let after_lower = after_comma.to_lowercase();
    let (_, rest) = nom_on_lower(after_comma, &after_lower, |i| {
        alt((value((), tag("a ")), value((), tag("an ")))).parse(i)
    })?;
    Some((name.to_string(), rest))
}

fn parse_token_pt_prefix(text: &str) -> Option<(PtValue, PtValue, &str)> {
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

fn parse_token_pt_component(text: &str) -> Option<PtValue> {
    if text.eq_ignore_ascii_case("x") {
        return Some(PtValue::Variable("X".to_string()));
    }
    text.parse::<i32>().ok().map(PtValue::Fixed)
}

fn strip_token_supertypes(mut text: &str) -> &str {
    loop {
        let trimmed = text.trim_start();
        let trimmed_lower = trimmed.to_lowercase();
        let Some((_, stripped)) = nom_on_lower(trimmed, &trimmed_lower, |i| {
            alt((
                value((), tag("legendary ")),
                value((), tag("snow ")),
                value((), tag("basic ")),
            ))
            .parse(i)
        }) else {
            return trimmed;
        };
        text = stripped;
    }
}

fn parse_token_color_prefix(mut text: &str) -> (Vec<ManaColor>, &str) {
    let mut colors = Vec::new();

    loop {
        let trimmed = text.trim_start();
        let Some((color, rest)) = strip_color_word(trimmed) else {
            break;
        };
        if let Some(color) = color {
            colors.push(color);
        }
        text = rest;

        let trimmed = text.trim_start();
        let trimmed_lower = trimmed.to_lowercase();
        if let Some((_, rest)) = nom_on_lower(trimmed, &trimmed_lower, |i| {
            alt((value((), tag("and ")), value((), tag(", ")))).parse(i)
        }) {
            text = rest;
            continue;
        }
        break;
    }

    (colors, text.trim_start())
}

/// Strip a lowercase color word from the start of text, returning the parsed
/// color and remainder.
///
/// Delegates to `nom_primitives::parse_color` for the five MTG colors, with a
/// manual "colorless" check (which maps to `None` since it's not a `ManaColor`).
/// Note: only matches lowercase color words (matching the original behavior)
/// since token descriptions preserve Oracle casing.
fn strip_color_word(text: &str) -> Option<(Option<ManaColor>, &str)> {
    // "colorless" is not a ManaColor -- handle before delegating to nom
    let text_lower = text.to_lowercase();
    if let Some((_, rest)) =
        nom_on_lower(text, &text_lower, |i| value((), tag("colorless")).parse(i))
    {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return Some((None, rest.trim_start()));
        }
    }
    // Delegate the five named colors to nom combinator.
    // nom's parse_color expects lowercase, and we match only lowercase here
    // (Oracle text preserves original casing in token descriptions).
    if let Ok((rest, color)) = nom_primitives::parse_color.parse(text) {
        // Word boundary: color word must be followed by whitespace or end
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return Some((Some(color), rest.trim_start()));
        }
    }
    None
}

fn split_token_head(text: &str) -> Option<(&str, &str)> {
    let lower = text.to_lowercase();
    let pos = lower.find(" token")?;
    let head = text[..pos].trim();
    let mut suffix = &text[pos + " token".len()..];
    // Strip plural 's' suffix
    if suffix.starts_with('s') {
        suffix = &suffix[1..];
    }
    if head.is_empty() {
        return None;
    }
    Some((head, suffix.trim()))
}

fn parse_token_name_clause(text: &str) -> (Option<String>, &str) {
    let trimmed = text.trim_start();
    let trimmed_lower = trimmed.to_lowercase();
    let Some((_, after_named)) = nom_on_lower(trimmed, &trimmed_lower, |i| {
        value((), tag("named ")).parse(i)
    }) else {
        return (None, trimmed);
    };

    let after_named_lower = after_named.to_lowercase();
    let after_named_tp = TextPair::new(after_named, &after_named_lower);
    let mut end = after_named.len();
    for needle in [" with ", " attached ", ",", "."] {
        if let Some(pos) = after_named_tp.find(needle) {
            end = end.min(pos);
        }
    }

    let name = after_named[..end].trim().trim_matches('"');
    let rest = after_named[end..].trim_start();
    if name.is_empty() {
        (None, rest)
    } else {
        (Some(name.to_string()), rest)
    }
}

fn extract_token_where_x_expression(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    Some(
        tp.strip_after("where x is ")?
            .original
            .trim()
            .trim_end_matches('.')
            .to_string(),
    )
}

fn extract_token_count_expression(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    Some(
        tp.strip_after("equal to ")?
            .original
            .trim()
            .trim_end_matches('.')
            .to_string(),
    )
}

fn extract_token_pt_expression(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    for needle in [
        "power and toughness are each equal to ",
        "power and toughness is each equal to ",
    ] {
        if let Some(after) = tp.strip_after(needle) {
            return Some(
                after
                    .original
                    .trim()
                    .trim_matches('"')
                    .trim_end_matches('.')
                    .to_string(),
            );
        }
    }
    None
}

fn parse_token_identity(descriptor: &str) -> Option<(String, Vec<String>)> {
    let mut core_types = Vec::new();
    let mut subtypes = Vec::new();

    for word in descriptor.split_whitespace() {
        match word.to_lowercase().as_str() {
            "artifact" => push_unique_string(&mut core_types, "Artifact"),
            "creature" => push_unique_string(&mut core_types, "Creature"),
            "enchantment" => push_unique_string(&mut core_types, "Enchantment"),
            "land" => push_unique_string(&mut core_types, "Land"),
            "snow" | "legendary" | "basic" => {}
            _ => subtypes.push(title_case_word(word)),
        }
    }

    if core_types.is_empty() {
        return known_named_token_identity(descriptor);
    }

    let name = if subtypes.is_empty() {
        "Token".to_string()
    } else {
        subtypes.join(" ")
    };

    let mut types = core_types;
    for subtype in subtypes {
        push_unique_string(&mut types, subtype);
    }

    Some((name, types))
}

fn known_named_token_identity(descriptor: &str) -> Option<(String, Vec<String>)> {
    let lower = descriptor.trim().to_lowercase();

    // CR 303.7: Role tokens are Enchantment -- Aura Role tokens.
    if let Some(identity) = known_role_token_identity(&lower) {
        return Some(identity);
    }

    let name = match lower.as_str() {
        "treasure" => "Treasure",
        "food" => "Food",
        "clue" => "Clue",
        "blood" => "Blood",
        "map" => "Map",
        "powerstone" => "Powerstone",
        "junk" => "Junk",
        "shard" => "Shard",
        "gold" => "Gold",
        "lander" => "Lander",
        "mutagen" => "Mutagen",
        _ => return None,
    };

    Some((
        name.to_string(),
        vec!["Artifact".to_string(), name.to_string()],
    ))
}

/// CR 303.7: Role tokens are predefined Enchantment -- Aura Role tokens with
/// "enchant creature you control". Each Role type grants fixed abilities to the
/// enchanted creature.
fn known_role_token_identity(descriptor: &str) -> Option<(String, Vec<String>)> {
    let name = match descriptor {
        "cursed role" => "Cursed Role",
        "monster role" => "Monster Role",
        "royal role" => "Royal Role",
        "sorcerer role" => "Sorcerer Role",
        "wicked role" => "Wicked Role",
        "young hero role" => "Young Hero Role",
        "virtuous role" => "Virtuous Role",
        "huntsman role" => "Huntsman Role",
        "chef role" => "Chef Role",
        "questing role" => "Questing Role",
        _ => return None,
    };

    Some((
        name.to_string(),
        vec![
            "Enchantment".to_string(),
            "Aura".to_string(),
            "Role".to_string(),
        ],
    ))
}

pub(super) fn parse_token_keyword_clause(text: &str) -> Vec<Keyword> {
    let trimmed = text.trim_start();
    let trimmed_lower = trimmed.to_lowercase();
    let Some((_, after_with)) = nom_on_lower(trimmed, &trimmed_lower, |i| {
        value((), tag("with ")).parse(i)
    }) else {
        return Vec::new();
    };

    let raw_clause = after_with
        .split('"')
        .next()
        .unwrap_or(after_with)
        .split(" where ")
        .next()
        .unwrap_or(after_with)
        .split(" attached ")
        .next()
        .unwrap_or(after_with)
        .trim()
        .trim_end_matches('.')
        .trim_end_matches(',')
        .trim_end_matches(" and")
        .trim();

    split_token_keyword_list(raw_clause)
        .into_iter()
        .filter_map(map_token_keyword)
        .collect()
}

pub(super) fn split_token_keyword_list(text: &str) -> Vec<&str> {
    text.split(", and ")
        .flat_map(|chunk| chunk.split(" and "))
        .flat_map(|sub| sub.split(", "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

pub(super) fn map_token_keyword(text: &str) -> Option<Keyword> {
    let trimmed = text.trim();
    if trimmed.eq_ignore_ascii_case("all creature types") {
        return Some(Keyword::Changeling);
    }
    match Keyword::from_str(trimmed) {
        Ok(Keyword::Unknown(_)) => None,
        Ok(keyword) => Some(keyword),
        Err(_) => None,
    }
}

pub(super) fn title_case_word(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub(super) fn push_unique_string(values: &mut Vec<String>, value: impl Into<String> + AsRef<str>) {
    if !values.iter().any(|existing| existing == value.as_ref()) {
        values.push(value.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_clause_with_trailing_comma_before_where() {
        // "with flying, where X is..." -- comma must not poison the keyword
        let kws = parse_token_keyword_clause("with flying, where X is that spell's mana value");
        assert_eq!(kws, vec![Keyword::Flying]);
    }

    #[test]
    fn keyword_clause_multiple_with_where() {
        let kws =
            parse_token_keyword_clause("with flying and haste, where X is that spell's mana value");
        assert_eq!(kws, vec![Keyword::Flying, Keyword::Haste]);
    }

    #[test]
    fn keyword_clause_no_where() {
        let kws = parse_token_keyword_clause("with flying");
        assert_eq!(kws, vec![Keyword::Flying]);
    }
}
