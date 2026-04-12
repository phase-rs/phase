use nom::bytes::complete::tag;
use nom::Parser;
use nom_language::error::VerboseError;

use super::super::oracle_nom::bridge::nom_on_lower;
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::quantity as nom_quantity;
use super::super::oracle_target::{parse_mana_value_suffix, parse_type_phrase};
use super::super::oracle_util::{
    contains_possessive, infer_core_type_for_subtype, split_around, strip_after,
};
use super::types::{SearchLibraryDetails, SeekDetails};
use super::{capitalize, scan_contains_phrase};
use crate::parser::oracle_warnings::push_warning;
use crate::types::ability::{
    ControllerRef, FilterProp, QuantityExpr, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::zones::Zone;

/// Scan `lower` at word boundaries for `tag_prefix`, then apply `combinator` to the
/// remainder. Returns `(parsed_value, byte_offset_in_lower_of_tail)` on first match.
///
/// Prefer this over `strip_after` + nom for composable multi-position parsing —
/// matches start-of-string, spaces, commas, or semicolons as word boundaries.
fn scan_preceded<'a, T>(
    lower: &'a str,
    tag_prefix: &'static str,
    mut combinator: impl FnMut(&'a str) -> Result<(&'a str, T), nom::Err<VerboseError<&'a str>>>,
) -> Option<(T, usize)> {
    let mut search_from = 0;
    while search_from <= lower.len() {
        let idx = lower[search_from..]
            .find(tag_prefix)
            .map(|i| search_from + i)?;
        // Word-boundary check: must be at start or preceded by whitespace/punctuation.
        let at_boundary = idx == 0
            || matches!(
                lower.as_bytes()[idx - 1],
                b' ' | b',' | b';' | b'(' | b'.' | b'\n' | b'\t'
            );
        if at_boundary {
            let after_prefix = &lower[idx + tag_prefix.len()..];
            if let Ok((rest, val)) = combinator(after_prefix) {
                let offset = lower.len() - rest.len();
                return Some((val, offset));
            }
        }
        search_from = idx + 1;
    }
    None
}

pub(super) fn parse_search_library_details(lower: &str) -> SearchLibraryDetails {
    let reveal = scan_contains_phrase(lower, "reveal");

    // CR 701.23a: Detect "search target opponent's/player's library" patterns.
    // These target a player, searching that player's library instead of the controller's.
    let target_player = parse_search_target_player(lower);

    // Extract count from "up to N" / "up to X" (must be done before filter extraction
    // since "for up to five creature cards" needs to skip the count to find the type).
    // CR 107.3a + CR 601.2b: X resolves to the caster's announced value at cast time.
    let up_to_match = scan_preceded(lower, "up to ", nom_quantity::parse_quantity_expr_number);

    // Fallback: "for N cards" / "for X cards" without "up to".
    let for_match = if up_to_match.is_none() {
        scan_preceded(lower, "for ", nom_quantity::parse_quantity_expr_number)
            // Require a word break after the number (" cards" / " creature ...").
            // Guards against matching "for a", "for an", etc. where parse_number fails
            // (good) but also avoids partial matches like "for the".
            .filter(|(_, off)| lower.as_bytes().get(*off).is_some_and(|b| *b == b' '))
    } else {
        None
    };

    let (count, count_end_in_for) = match (up_to_match, for_match) {
        (Some((expr, off)), _) => (expr, Some(off)),
        (None, Some((expr, _))) => (expr, None),
        (None, None) => (QuantityExpr::Fixed { value: 1 }, None),
    };

    // Extract the type filter from after "for a/an" or from the tail after "up to N".
    let filter = if let Some(after_for) = strip_after(lower, "for a ") {
        parse_search_filter(after_for)
    } else if let Some(after_for) = strip_after(lower, "for an ") {
        parse_search_filter(after_for)
    } else if let Some(type_start) = count_end_in_for {
        // "for up to five creature cards" — type text starts after the number
        parse_search_filter(&lower[type_start..])
    } else {
        TargetFilter::Any
    };

    SearchLibraryDetails {
        filter,
        count,
        reveal,
        target_player,
    }
}

/// CR 701.23a: Detect player-targeting search patterns like "search target opponent's library"
/// or "search target player's library". Returns a TargetFilter for the player.
fn parse_search_target_player(lower: &str) -> Option<TargetFilter> {
    use nom::branch::alt;
    use nom::combinator::value;
    use nom::sequence::preceded;

    let (filter, _rest) = nom_on_lower(lower, lower, |i| {
        preceded(
            tag("search "),
            alt((
                value(
                    TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                    tag("target opponent's library"),
                ),
                value(TargetFilter::Player, tag("target player's library")),
                value(
                    TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                    tag("an opponent's library"),
                ),
            )),
        )
        .parse(i)
    })?;
    Some(filter)
}

/// Parse "seek [count] [filter] card(s) [and put onto battlefield [tapped]]".
/// Seek grammar is simpler than search: no "your library", no "for", no shuffle.
pub(super) fn parse_seek_details(lower: &str) -> SeekDetails {
    let after_seek = tag::<_, _, VerboseError<&str>>("seek ")
        .parse(lower)
        .map(|(rest, _)| rest)
        .unwrap_or(lower);

    // Extract destination clause before filter parsing, so it doesn't pollute the filter.
    let (filter_text, destination, enter_tapped) = {
        let put_idx = after_seek
            .find(" and put")
            .or_else(|| after_seek.find(", put"));
        if let Some(idx) = put_idx {
            let dest_clause = &after_seek[idx..];
            let dest = parse_search_destination(dest_clause);
            let tapped = scan_contains_phrase(dest_clause, "battlefield tapped");
            (&after_seek[..idx], dest, tapped)
        } else {
            (after_seek, Zone::Hand, false)
        }
    };

    // Extract count: "two nonland cards" → (2, "nonland cards"); "x cards" → (X, "cards").
    // CR 107.3a + CR 601.2b: X resolves to the caster's announced value at cast time.
    let (count, remaining) =
        if let Ok((rest, expr)) = nom_quantity::parse_quantity_expr_number(filter_text) {
            (expr, rest.trim_start())
        } else {
            (QuantityExpr::Fixed { value: 1 }, filter_text)
        };

    // Strip leading article "a "/"an "
    let remaining = nom_primitives::parse_article
        .parse(remaining)
        .map(|(rest, _)| rest)
        .unwrap_or(remaining);

    let filter = parse_search_filter(remaining);

    SeekDetails {
        filter,
        count,
        destination,
        enter_tapped,
    }
}

/// Parse the card type filter from search text like "basic land card, ..."
/// or "creature card with ..." into a TargetFilter.
pub(super) fn parse_search_filter(text: &str) -> TargetFilter {
    let type_text = text.trim();

    let (parsed_filter, remainder) = parse_type_phrase(type_text);
    if !matches!(parsed_filter, TargetFilter::Any) {
        let mut suffix_properties = vec![];
        parse_search_filter_suffixes(remainder, &mut suffix_properties);
        return apply_search_suffix_properties(
            normalize_search_filter(parsed_filter),
            &suffix_properties,
        );
    }

    let type_text = strip_search_card_suffix(type_text);

    // Intentional: "a card" means any card type — no warning needed.
    if type_text == "card" || type_text.is_empty() {
        return TargetFilter::Any;
    }

    let (is_basic, clean) = if let Some(rest) = type_text.strip_prefix("basic ") {
        (true, rest)
    } else {
        (false, type_text)
    };
    let (type_word, suffix_text) = split_search_type_word_and_suffix(clean);

    parse_search_filter_fallback(type_word, suffix_text, is_basic)
}

fn parse_search_filter_fallback(
    type_word: &str,
    suffix_text: &str,
    is_basic: bool,
) -> TargetFilter {
    let properties = build_search_suffix_properties(suffix_text, is_basic);

    match type_word {
        "land" => TargetFilter::Typed(TypedFilter::new(TypeFilter::Land).properties(properties)),
        "creature" => {
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature).properties(properties))
        }
        "artifact" => {
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).properties(properties))
        }
        "enchantment" => {
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment).properties(properties))
        }
        "instant" => {
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant).properties(properties))
        }
        "sorcery" => {
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery).properties(properties))
        }
        "planeswalker" => {
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Planeswalker).properties(properties))
        }
        "instant or sorcery" => TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Instant).properties(properties.clone()),
                ),
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery).properties(properties)),
            ],
        },
        other => parse_search_specialized_type_word(other, properties),
    }
}

fn parse_search_specialized_type_word(
    type_word: &str,
    properties: Vec<FilterProp>,
) -> TargetFilter {
    let negated_types: &[(&str, TypeFilter)] = &[
        ("noncreature", TypeFilter::Creature),
        ("nonland", TypeFilter::Land),
        ("nonartifact", TypeFilter::Artifact),
        ("nonenchantment", TypeFilter::Enchantment),
    ];
    for &(prefix, ref inner) in negated_types {
        if type_word == prefix {
            return TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Non(Box::new(inner.clone()))).properties(properties),
            );
        }
    }

    let land_subtypes = ["plains", "island", "swamp", "mountain", "forest"];
    if land_subtypes.contains(&type_word) {
        return TargetFilter::Typed(
            TypedFilter::land()
                .subtype(capitalize(type_word))
                .properties(properties),
        );
    }
    if type_word == "equipment" {
        return TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Artifact)
                .subtype("Equipment".to_string())
                .properties(properties),
        );
    }
    if type_word == "aura" {
        return TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Enchantment)
                .subtype("Aura".to_string())
                .properties(properties),
        );
    }
    if type_word == "card" && !properties.is_empty() {
        return TargetFilter::Typed(TypedFilter::default().properties(properties));
    }
    if !type_word.is_empty()
        && type_word != "card"
        && type_word != "permanent"
        && type_word.chars().all(|c| c.is_alphabetic())
    {
        return TargetFilter::Typed(
            TypedFilter::default()
                .subtype(capitalize(type_word))
                .properties(properties),
        );
    }

    let (filter, _) = parse_type_phrase(type_word);
    if !matches!(filter, TargetFilter::Any) {
        return apply_search_suffix_properties(filter, &properties);
    }

    push_warning(format!(
        "target-fallback: unrecognized search filter '{}'",
        type_word
    ));
    TargetFilter::Any
}

fn strip_search_card_suffix(text: &str) -> &str {
    text.strip_suffix(" cards")
        .or_else(|| text.strip_suffix(" card"))
        .unwrap_or(text)
        .trim()
}

fn split_search_type_word_and_suffix(clean: &str) -> (&str, &str) {
    if let Some((type_word, _)) = split_around(clean, " with ") {
        (
            strip_search_card_suffix(type_word.trim()),
            &clean[type_word.len()..],
        )
    } else {
        (clean.trim(), "")
    }
}

fn build_search_suffix_properties(suffix_text: &str, is_basic: bool) -> Vec<FilterProp> {
    let mut properties = vec![];
    if is_basic {
        properties.push(FilterProp::HasSupertype {
            value: crate::types::card_type::Supertype::Basic,
        });
    }
    parse_search_filter_suffixes(suffix_text, &mut properties);
    properties
}

fn normalize_search_filter(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(typed_filter) => {
            TargetFilter::Typed(normalize_search_typed_filter(typed_filter))
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.into_iter().map(normalize_search_filter).collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters.into_iter().map(normalize_search_filter).collect(),
        },
        other => other,
    }
}

fn normalize_search_typed_filter(mut typed_filter: TypedFilter) -> TypedFilter {
    let inferred_type = typed_filter.type_filters.iter().find_map(|type_filter| {
        let TypeFilter::Subtype(subtype) = type_filter else {
            return None;
        };
        infer_core_type_for_subtype(subtype).map(|core_type| match core_type {
            CoreType::Artifact => TypeFilter::Artifact,
            CoreType::Enchantment => TypeFilter::Enchantment,
            CoreType::Land => TypeFilter::Land,
            _ => TypeFilter::Creature,
        })
    });

    if let Some(inferred_type) = inferred_type {
        let already_present = typed_filter.type_filters.contains(&inferred_type);
        if !already_present {
            typed_filter.type_filters.insert(0, inferred_type);
        }
    }

    typed_filter
}

fn apply_search_suffix_properties(
    filter: TargetFilter,
    suffix_properties: &[FilterProp],
) -> TargetFilter {
    if suffix_properties.is_empty() {
        return filter;
    }

    match filter {
        TargetFilter::Any => {
            TargetFilter::Typed(TypedFilter::default().properties(suffix_properties.to_vec()))
        }
        TargetFilter::Typed(mut typed_filter) => {
            for property in suffix_properties {
                if !typed_filter
                    .properties
                    .iter()
                    .any(|existing| existing.same_kind(property))
                {
                    typed_filter.properties.push(property.clone());
                }
            }
            TargetFilter::Typed(typed_filter)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|branch| apply_search_suffix_properties(branch, suffix_properties))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|branch| apply_search_suffix_properties(branch, suffix_properties))
                .collect(),
        },
        other => other,
    }
}

/// Parse property suffixes from search filter text ("with mana value ...", "with a different name ...").
/// Reuses the existing suffix parsers from oracle_target.
fn parse_search_filter_suffixes(text: &str, properties: &mut Vec<FilterProp>) {
    let lower = text.to_lowercase();
    let mut remaining = lower.as_str();

    while !remaining.is_empty() {
        remaining = remaining.trim_start();
        if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("and ").parse(remaining) {
            remaining = rest.trim_start();
        }

        if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("with that name").parse(remaining) {
            properties.push(FilterProp::SameName);
            remaining = rest.trim_start();
            continue;
        }

        if let Some((prop, consumed)) = parse_mana_value_suffix(remaining) {
            properties.push(prop);
            remaining = remaining[consumed..].trim_start();
            continue;
        }

        if let Ok((rest, _)) =
            tag::<_, _, VerboseError<&str>>("with a different name than each ").parse(remaining)
        {
            let end = rest
                .find(" you control")
                .unwrap_or_else(|| rest.find(',').unwrap_or(rest.len()));
            let inner_type = rest[..end].trim();
            let inner_filter = match inner_type {
                "aura" => TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Enchantment).subtype("Aura".to_string()),
                ),
                "creature" => TargetFilter::Typed(TypedFilter::creature()),
                "enchantment" => TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment)),
                "artifact" => TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                _ => {
                    push_warning(format!(
                        "target-fallback: unrecognized inner type '{}' in different-name filter",
                        inner_type
                    ));
                    TargetFilter::Any
                }
            };
            properties.push(FilterProp::DifferentNameFrom {
                filter: Box::new(inner_filter),
            });
            let skip = rest
                .find(" you control")
                .map_or(end, |position| position + " you control".len());
            remaining = rest[skip..].trim_start();
            continue;
        }

        // Dispatch-loop diagnostic: unmatched trailing text indicates a parser gap
        // (e.g., novel "with …" suffix phrasing). Emit a warning so gaps surface
        // in coverage output instead of silently dropping filter constraints.
        push_warning(format!(
            "target-fallback: search-filter-suffix unmatched: '{}'",
            remaining
        ));
        break;
    }
}

/// Parse the destination zone from search Oracle text.
/// Looks for "put it into your hand", "put it onto the battlefield", etc.
pub(super) fn parse_search_destination(lower: &str) -> Zone {
    if scan_contains_phrase(lower, "onto the battlefield") {
        Zone::Battlefield
    } else if contains_possessive(lower, "into", "hand") {
        Zone::Hand
    } else if contains_possessive(lower, "on top of", "library") {
        Zone::Library
    } else if contains_possessive(lower, "into", "graveyard") {
        Zone::Graveyard
    } else {
        Zone::Hand
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_target_opponent_library() {
        let details = parse_search_library_details(
            "search target opponent's library for a creature card and put that card onto the battlefield under your control",
        );
        assert!(details.target_player.is_some());
        let tp = details.target_player.unwrap();
        match tp {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::Opponent));
            }
            other => panic!("expected Typed with Opponent controller, got {other:?}"),
        }
        // Filter should be creature
        match details.filter {
            TargetFilter::Typed(tf) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            }
            other => panic!("expected creature filter, got {other:?}"),
        }
    }

    #[test]
    fn search_target_player_library() {
        let details =
            parse_search_library_details("search target player's library for a card and exile it");
        assert!(details.target_player.is_some());
        assert_eq!(details.target_player.unwrap(), TargetFilter::Player);
    }

    #[test]
    fn search_target_player_library_for_three() {
        // Jester's Cap: "search target player's library for three cards and exile them"
        let details = parse_search_library_details(
            "search target player's library for three cards and exile them",
        );
        assert!(details.target_player.is_some());
        assert_eq!(details.count, QuantityExpr::Fixed { value: 3 });
    }

    #[test]
    fn search_your_library_no_target_player() {
        let details = parse_search_library_details(
            "search your library for a basic land card, reveal it, put it into your hand",
        );
        assert!(details.target_player.is_none());
        assert!(details.reveal);
    }

    #[test]
    fn search_up_to_x_cards_emits_variable_count() {
        // CR 107.3a + CR 601.2b: `up to X` emits `QuantityRef::Variable` so the
        // resolver can pick up the caster's announced X at effect time.
        use crate::types::ability::QuantityRef;
        let details =
            parse_search_library_details("search your library for up to x creature cards");
        assert_eq!(
            details.count,
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string()
                }
            }
        );
    }

    #[test]
    fn search_for_three_cards_emits_fixed_count_regression() {
        // Regression: numeric word counts still parse as `Fixed` — this is the
        // pre-widening behavior the switch to nom + `parse_quantity_expr_number`
        // must preserve.
        let details =
            parse_search_library_details("search your library for three cards and exile them");
        assert_eq!(details.count, QuantityExpr::Fixed { value: 3 });
    }

    #[test]
    fn strip_search_card_suffix_removes_card_wording() {
        assert_eq!(strip_search_card_suffix("creature cards"), "creature");
        assert_eq!(strip_search_card_suffix("artifact card"), "artifact");
        assert_eq!(strip_search_card_suffix("Aura"), "Aura");
    }

    #[test]
    fn split_search_type_word_and_suffix_splits_with_clause() {
        let (type_word, suffix) =
            split_search_type_word_and_suffix("basic creature cards with mana value 3 or less");
        assert_eq!(type_word, "basic creature");
        assert_eq!(suffix, " with mana value 3 or less");
    }

    #[test]
    fn build_search_suffix_properties_includes_basic_and_same_name() {
        let properties = build_search_suffix_properties(" with that name", true);
        assert!(properties.iter().any(|property| matches!(
            property,
            FilterProp::HasSupertype {
                value: crate::types::card_type::Supertype::Basic
            }
        )));
        assert!(properties
            .iter()
            .any(|property| matches!(property, FilterProp::SameName)));
    }

    #[test]
    fn parse_search_filter_fallback_handles_basic_card_same_name() {
        let filter = parse_search_filter_fallback("card", " with that name", true);
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::HasSupertype {
                value: crate::types::card_type::Supertype::Basic
            }
        )));
        assert!(typed
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::SameName)));
    }

    #[test]
    fn parse_search_specialized_type_word_handles_unknown_alphabetic_subtype() {
        let filter = parse_search_specialized_type_word("elf", vec![]);
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert_eq!(typed.get_subtype(), Some("Elf"));
    }
}
