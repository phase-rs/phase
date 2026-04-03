//! Quantity expression parsing from Oracle text.
//!
//! This module consolidates semantic quantity interpretation — mapping Oracle text
//! phrases like "the number of creatures you control" or "your life total" into
//! typed `QuantityRef` / `QuantityExpr` values. This is distinct from `oracle_util`,
//! which provides raw text extraction primitives (number parsing, mana symbol
//! counting, phrase matching).

use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::value;
use nom::Parser;
use nom_language::error::VerboseError;

use super::oracle_nom::quantity as nom_quantity;
use crate::parser::oracle_effect::counter::normalize_counter_type;
use crate::parser::oracle_target::{parse_target, parse_type_phrase};
use crate::parser::oracle_util::parse_number;
use crate::types::ability::{
    AggregateFunction, CountScope, ObjectProperty, PlayerFilter, QuantityExpr, QuantityRef,
    TargetFilter, TypeFilter, ZoneRef,
};
use crate::types::mana::ManaColor;

/// Map a quantity phrase to a dynamic QuantityRef.
///
/// Delegates to `oracle_nom::quantity::parse_quantity_ref` for simple exact-match
/// patterns (life total, hand size, graveyard size, self P/T, life lost/gained,
/// starting life total), then falls through to complex patterns (counters,
/// aggregates, object counts, devotion, etc.) that nom doesn't yet cover.
pub(crate) fn parse_quantity_ref(text: &str) -> Option<QuantityRef> {
    let trimmed = text.trim().trim_end_matches('.');

    // Try nom combinator first for simple exact-match patterns.
    if let Ok((rest, qty)) = nom_quantity::parse_quantity_ref.parse(trimmed) {
        if rest.is_empty() {
            return Some(qty);
        }
    }

    // Complex patterns requiring type phrase parsing or counter normalization.

    // "[counter type] counter(s) on ~" / "[counter type] counter(s) on it"
    // Handles both plural ("counters on ~") and singular ("counter on ~") forms.
    if let Some(rest) = trimmed
        .strip_suffix(" counters on ~")
        .or_else(|| trimmed.strip_suffix(" counters on it"))
        .or_else(|| trimmed.strip_suffix(" counter on ~"))
        .or_else(|| trimmed.strip_suffix(" counter on it"))
    {
        let raw_type = tag::<_, _, VerboseError<&str>>("the number of ")
            .parse(rest)
            .map_or(rest, |(r, _)| r)
            .trim();
        let counter_type = normalize_counter_type(raw_type);
        if !counter_type.is_empty() {
            return Some(QuantityRef::CountersOnSelf { counter_type });
        }
    }

    // "[counter type] counter(s) on that creature/permanent" — anaphoric reference
    // to a previously targeted object, not self. Distinct from CountersOnSelf.
    if let Some(rest) = trimmed
        .strip_suffix(" counters on that creature")
        .or_else(|| trimmed.strip_suffix(" counters on that permanent"))
        .or_else(|| trimmed.strip_suffix(" counter on that creature"))
        .or_else(|| trimmed.strip_suffix(" counter on that permanent"))
    {
        let raw_type = tag::<_, _, VerboseError<&str>>("the number of ")
            .parse(rest)
            .map_or(rest, |(r, _)| r)
            .trim();
        let counter_type = normalize_counter_type(raw_type);
        if !counter_type.is_empty() {
            return Some(QuantityRef::CountersOnTarget { counter_type });
        }
    }

    // Aggregate patterns: "the greatest X among" / "the total power of"
    if let Ok((rest, (func, prop))) = alt((
        value(
            (AggregateFunction::Max, ObjectProperty::Power),
            tag::<_, _, VerboseError<&str>>("the greatest power among "),
        ),
        value(
            (AggregateFunction::Max, ObjectProperty::Toughness),
            tag("the greatest toughness among "),
        ),
        value(
            (AggregateFunction::Max, ObjectProperty::ManaValue),
            tag("the greatest mana value among "),
        ),
        value(
            (AggregateFunction::Sum, ObjectProperty::Power),
            tag("the total power of "),
        ),
    ))
    .parse(trimmed)
    {
        let (filter, _) = parse_type_phrase(rest);
        if !matches!(filter, TargetFilter::Any) {
            return Some(QuantityRef::Aggregate {
                function: func,
                property: prop,
                filter,
            });
        }
    }

    // "the number of {type} you control" → ObjectCount { filter }
    // "the number of opponents you have" → PlayerCount { Opponent }
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("the number of ").parse(trimmed) {
        if rest == "opponents you have" || rest == "opponent you have" {
            return Some(QuantityRef::PlayerCount {
                filter: PlayerFilter::Opponent,
            });
        }
        let (filter, _) = parse_type_phrase(rest);
        if !matches!(filter, TargetFilter::Any) {
            return Some(QuantityRef::ObjectCount { filter });
        }
    }
    // "your devotion to {color}" / "your devotion to {color} and {color}"
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("your devotion to ").parse(trimmed) {
        let colors = parse_devotion_colors(rest);
        if !colors.is_empty() {
            return Some(QuantityRef::Devotion { colors });
        }
    }
    None
}

/// Parse color names from a devotion phrase like "black", "black and red".
fn parse_devotion_colors(text: &str) -> Vec<ManaColor> {
    text.split(" and ")
        .filter_map(|word| {
            let capitalized = capitalize_first(word.trim());
            ManaColor::from_str(&capitalized).ok()
        })
        .collect()
}

/// Capitalize the first letter of a word (for ManaColor::from_str).
pub(crate) fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Parse a CDA quantity phrase into a `QuantityExpr`.
/// Handles patterns like:
/// - "the number of creatures you control"
/// - "the number of cards in your hand"
/// - "your life total"
/// - "the number of creature cards in your graveyard"
/// - "the number of card types among cards in all graveyards"
/// - "the number of basic land types among lands you control"
/// - "N plus the number of X"
pub(crate) fn parse_cda_quantity(text: &str) -> Option<QuantityExpr> {
    let text = text.trim().trim_end_matches('.');

    // "twice [inner]" or "three times [inner]" → Multiply { factor, inner }
    if let Ok((rest, factor)) = alt((
        value(2i32, tag::<_, _, VerboseError<&str>>("twice ")),
        value(3, tag("three times ")),
    ))
    .parse(text)
    {
        if let Some(inner) = parse_cda_quantity(rest) {
            return Some(QuantityExpr::Multiply {
                factor,
                inner: Box::new(inner),
            });
        }
    }

    // "N plus [inner]" generalized offset pattern
    if let Some((prefix, rest)) = text.split_once(" plus ") {
        if let Some((n, _)) = parse_number(prefix) {
            if let Some(inner) = parse_cda_quantity(rest) {
                return Some(QuantityExpr::Offset {
                    inner: Box::new(inner),
                    offset: n as i32,
                });
            }
        }
    }

    // "the number of card types among cards in all graveyards"
    if text.contains("card types among cards in all graveyards") {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::CardTypesInGraveyards {
                scope: CountScope::All,
            },
        });
    }
    if text.contains("card types among cards in your graveyard") {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::CardTypesInGraveyards {
                scope: CountScope::Controller,
            },
        });
    }

    // "the number of basic land types among lands you control" (Domain)
    if text.contains("basic land types among lands you control") {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::BasicLandTypeCount,
        });
    }

    // "the number of cards in your hand"
    if text.contains("cards in your hand") {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::HandSize,
        });
    }

    // CR 103.4: "your starting life total" — must precede "your life total" check
    // because contains("your life total") would also match "your starting life total".
    if text.contains("your starting life total") {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::StartingLifeTotal,
        });
    }

    // "your life total"
    if text.contains("your life total") {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::LifeTotal,
        });
    }

    if text.contains("your speed") {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::Speed,
        });
    }

    // "the number of {type} cards in your graveyard" — MUST be checked before the
    // untyped "cards in your graveyard" pattern to avoid matching "instant and sorcery
    // cards in your graveyard" as untyped.
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("the number of ").parse(text) {
        if let Some(type_text) = rest.strip_suffix(" cards in your graveyard") {
            if let Some(card_types) = parse_cda_type_filters(type_text) {
                return Some(QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Graveyard,
                        card_types,
                        scope: CountScope::Controller,
                    },
                });
            }
        }
        if let Some(type_text) = rest.strip_suffix(" cards in all graveyards") {
            if let Some(card_types) = parse_cda_type_filters(type_text) {
                return Some(QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Graveyard,
                        card_types,
                        scope: CountScope::All,
                    },
                });
            }
        }
    }

    // "the number of cards in your graveyard" (untyped — all cards)
    if text == "the number of cards in your graveyard" || text.contains("cards in your graveyard") {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::ZoneCardCount {
                zone: ZoneRef::Graveyard,
                card_types: vec![],
                scope: CountScope::Controller,
            },
        });
    }

    // "the number of cards in your opponents' graveyards" / "cards in opponents' graveyards"
    if text.contains("cards in your opponents' graveyards")
        || text.contains("cards in opponents' graveyards")
    {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::ZoneCardCount {
                zone: ZoneRef::Graveyard,
                card_types: vec![],
                scope: CountScope::Opponents,
            },
        });
    }

    // "the number of noncreature spells they've cast this turn"
    // "the number of spells they've cast this turn"
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("the number of ").parse(text) {
        // Note: "this turn" may already be stripped by strip_trailing_duration at the clause
        // level, so we also match the bare " they've cast" / " that player has cast" suffixes.
        if let Some(spell_part) = rest
            .strip_suffix(" they've cast this turn")
            .or_else(|| rest.strip_suffix(" that player has cast this turn"))
            .or_else(|| rest.strip_suffix(" you've cast this turn"))
            .or_else(|| rest.strip_suffix(" you cast this turn"))
            .or_else(|| rest.strip_suffix(" they've cast"))
            .or_else(|| rest.strip_suffix(" that player has cast"))
            .or_else(|| rest.strip_suffix(" you've cast"))
            .or_else(|| rest.strip_suffix(" you cast"))
        {
            let spell_part = spell_part.trim();
            let filter = if spell_part == "spells" || spell_part == "spell" {
                None
            } else {
                let qualifier = spell_part
                    .strip_suffix(" spells")
                    .or_else(|| spell_part.strip_suffix(" spell"))
                    .unwrap_or(spell_part)
                    .trim();
                let (filter, remainder) = parse_type_phrase(qualifier);
                if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
                    Some(filter)
                } else {
                    None
                }
            };
            return Some(QuantityExpr::Ref {
                qty: QuantityRef::SpellsCastThisTurn { filter },
            });
        }
    }

    // Delegate to existing parse_quantity_ref for patterns like
    // "the number of {type} you control", "your devotion to X"
    if let Some(qty) = parse_quantity_ref(text) {
        return Some(QuantityExpr::Ref { qty });
    }

    None
}

/// Map a type phrase to one or more `TypeFilter` entries for CDA zone card counting.
/// Returns a Vec to support combined types like "instant and sorcery".
fn parse_cda_type_filters(text: &str) -> Option<Vec<TypeFilter>> {
    match text.trim() {
        "creature" => Some(vec![TypeFilter::Creature]),
        "instant" => Some(vec![TypeFilter::Instant]),
        "sorcery" => Some(vec![TypeFilter::Sorcery]),
        "land" => Some(vec![TypeFilter::Land]),
        "artifact" => Some(vec![TypeFilter::Artifact]),
        "enchantment" => Some(vec![TypeFilter::Enchantment]),
        "planeswalker" => Some(vec![TypeFilter::Planeswalker]),
        "instant and sorcery" | "instant or sorcery" => {
            Some(vec![TypeFilter::Instant, TypeFilter::Sorcery])
        }
        _ => None,
    }
}

/// Parse event-context quantity references from Oracle text fragments.
/// Returns None for unrecognized patterns (caller falls back to Variable).
pub(crate) fn parse_event_context_quantity(text: &str) -> Option<QuantityExpr> {
    let lower = text.to_lowercase();
    let lower = lower.trim();
    match lower {
        "that much" | "that many" => {
            return Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            })
        }
        "its power" => {
            return Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextSourcePower,
            })
        }
        "its toughness" => {
            return Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextSourceToughness,
            })
        }
        "its mana value" | "its converted mana cost" => {
            return Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextSourceManaValue,
            })
        }
        _ => {}
    }

    // CR 603.7c: Decompose possessive noun phrases: "{referent}'s {property}"
    if let Some((prefix, suffix)) = lower.split_once("'s ") {
        let suffix = suffix.trim();
        let qty = match suffix {
            "power" => Some(QuantityRef::EventContextSourcePower),
            "toughness" => Some(QuantityRef::EventContextSourceToughness),
            "mana value" | "converted mana cost" => Some(QuantityRef::EventContextSourceManaValue),
            _ => None,
        };
        if let Some(qty) = qty {
            let prefix = prefix.trim();
            if is_event_context_referent(prefix) {
                return Some(QuantityExpr::Ref { qty });
            }
        }
    }

    // Fall back to parse_quantity_ref for named quantity patterns
    // (e.g., "the life you've lost this turn" → LifeLostThisTurn).
    // Strip leading "the " article before matching.
    // Exclude target-referent variants (TargetPower, TargetLifeTotal) — these
    // reference a targeting selection, not an event-context source object.
    let stripped = tag::<_, _, VerboseError<&str>>("the ")
        .parse(lower)
        .map_or(lower, |(r, _)| r);
    if let Some(qty) = parse_quantity_ref(stripped) {
        if !matches!(qty, QuantityRef::TargetPower | QuantityRef::TargetLifeTotal) {
            return Some(QuantityExpr::Ref { qty });
        }
    }

    None
}

/// CR 603.7c: Check if a possessive prefix refers to the triggering event's source object.
/// Matches event-context anaphoric referents like "the sacrificed creature", "that spell", etc.
fn is_event_context_referent(prefix: &str) -> bool {
    let event_adjectives = [
        "sacrificed",
        "destroyed",
        "exiled",
        "discarded",
        "countered",
        "returned",
        "targeted",
        "revealed",
        "drawn",
        "copied",
    ];
    if prefix.starts_with("that ") || prefix.starts_with("the ") {
        let rest = prefix.split_once(' ').map_or("", |x| x.1);
        // "the sacrificed creature", "the exiled card" — [adjective] [type]
        if event_adjectives.iter().any(|adj| rest.starts_with(adj)) {
            return true;
        }
        // "that creature", "that spell", "the creature" — bare anaphoric
        let bare_types = [
            "creature",
            "spell",
            "card",
            "permanent",
            "artifact",
            "enchantment",
            "planeswalker",
            "land",
        ];
        if bare_types.contains(&rest) {
            return true;
        }
    }
    false
}

/// Parse the clause after "for each" into a QuantityRef.
pub(crate) fn parse_for_each_clause(clause: &str) -> Option<QuantityRef> {
    let clause = clause.trim().trim_end_matches('.');

    // "card put into a graveyard this way" / "creature card exiled this way" / etc.
    // "this way" references objects from the preceding effect's tracked set.
    if clause.contains("this way") {
        return Some(QuantityRef::TrackedSetSize);
    }

    // "opponent who lost life this turn"
    if clause.contains("opponent") && clause.contains("lost life") {
        return Some(QuantityRef::PlayerCount {
            filter: PlayerFilter::OpponentLostLife,
        });
    }

    // "opponent who gained life this turn"
    if clause.contains("opponent") && clause.contains("gained life") {
        return Some(QuantityRef::PlayerCount {
            filter: PlayerFilter::OpponentGainedLife,
        });
    }

    // "opponent"
    if clause == "opponent" || clause == "opponent you have" {
        return Some(QuantityRef::PlayerCount {
            filter: PlayerFilter::Opponent,
        });
    }

    // "[counter type] counter(s) on that creature/permanent" — anaphoric, must check
    // before the wildcard "counter on" guard below which would misroute to CountersOnSelf.
    if clause.contains("counter on that") {
        if let Some(qty) = parse_quantity_ref(clause) {
            return Some(qty);
        }
    }

    // "[counter type] counter on ~" / "[counter type] counter on it"
    if clause.contains("counter on") {
        let raw_type = clause.split("counter").next().unwrap_or("").trim();
        if !raw_type.is_empty() {
            return Some(QuantityRef::CountersOnSelf {
                counter_type: normalize_counter_type(raw_type),
            });
        }
    }

    // Compose with parse_quantity_ref for named quantity patterns like
    // "card in your hand" (→ HandSize), "life you gained this turn", etc.
    // "for each" strips the quantifier, so the clause may be singular or have
    // slightly different phrasing. Try both as-is and with "s" appended.
    if let Some(qty) = parse_quantity_ref(clause) {
        return Some(qty);
    }
    // Handle singular → plural: "card in your hand" → "cards in your hand"
    if let Some((first_word, rest)) = clause.split_once(' ') {
        let pluralized = format!("{first_word}s {rest}");
        if let Some(qty) = parse_quantity_ref(&pluralized) {
            return Some(qty);
        }
    }

    // "spell you've cast this turn" / "spells you've cast this turn"
    // Direct dispatch before parse_target to handle spell-casting quantity patterns.
    if let Some(spell_part) = clause
        .strip_suffix(" you've cast this turn")
        .or_else(|| clause.strip_suffix(" you cast this turn"))
        .or_else(|| clause.strip_suffix(" you've cast"))
        .or_else(|| clause.strip_suffix(" you cast"))
    {
        let spell_part = spell_part.trim();
        let filter = if spell_part == "spells"
            || spell_part == "spell"
            || spell_part == "time"
            || spell_part.is_empty()
        {
            None
        } else {
            let qualifier = spell_part
                .strip_suffix(" spells")
                .or_else(|| spell_part.strip_suffix(" spell"))
                .unwrap_or(spell_part)
                .trim();
            let (f, remainder) = parse_type_phrase(qualifier);
            if remainder.trim().is_empty() && !matches!(f, TargetFilter::Any) {
                Some(f)
            } else {
                None
            }
        };
        return Some(QuantityRef::SpellsCastThisTurn { filter });
    }

    // "creature you control", "artifact you control", etc.
    let (filter, _) = parse_target(clause);
    if !matches!(filter, TargetFilter::Any) {
        return Some(QuantityRef::ObjectCount { filter });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::mana::ManaColor;

    #[test]
    fn for_each_counter_on_self_normalized() {
        let qty = parse_for_each_clause("+1/+1 counter on ~").unwrap();
        match qty {
            QuantityRef::CountersOnSelf { counter_type } => assert_eq!(counter_type, "P1P1"),
            other => panic!("Expected CountersOnSelf, got {other:?}"),
        }
    }

    #[test]
    fn for_each_singular_counter_on_self() {
        // Singular "counter on ~" (not "counters on ~")
        let qty = parse_for_each_clause("blight counter on it").unwrap();
        assert!(
            matches!(qty, QuantityRef::CountersOnSelf { ref counter_type } if counter_type == "blight"),
            "singular counter form should produce CountersOnSelf"
        );
    }

    #[test]
    fn for_each_counter_on_that_creature() {
        let qty = parse_for_each_clause("+1/+1 counter on that creature").unwrap();
        assert!(
            matches!(qty, QuantityRef::CountersOnTarget { ref counter_type } if counter_type == "P1P1"),
            "counter on that creature should produce CountersOnTarget, not CountersOnSelf"
        );
    }

    #[test]
    fn for_each_this_way_produces_tracked_set_size() {
        let qty = parse_for_each_clause("card put into a graveyard this way").unwrap();
        assert_eq!(qty, QuantityRef::TrackedSetSize);
    }

    #[test]
    fn quantity_ref_counters_on_target() {
        let qty = parse_quantity_ref("+1/+1 counters on that creature").unwrap();
        assert!(
            matches!(qty, QuantityRef::CountersOnTarget { ref counter_type } if counter_type == "P1P1"),
            "counters on that creature should produce CountersOnTarget"
        );
    }

    #[test]
    fn quantity_ref_singular_counter_on_target() {
        let qty = parse_quantity_ref("charge counter on that permanent").unwrap();
        assert!(
            matches!(qty, QuantityRef::CountersOnTarget { ref counter_type } if counter_type == "charge"),
            "singular counter on that permanent should produce CountersOnTarget"
        );
    }

    #[test]
    fn parse_quantity_ref_object_count() {
        let qty = parse_quantity_ref("the number of creatures you control").unwrap();
        assert!(
            matches!(qty, QuantityRef::ObjectCount { .. }),
            "Expected ObjectCount, got {qty:?}"
        );
    }

    #[test]
    fn parse_quantity_ref_subtype_count() {
        let qty = parse_quantity_ref("the number of Allies you control").unwrap();
        assert!(
            matches!(qty, QuantityRef::ObjectCount { .. }),
            "Expected ObjectCount, got {qty:?}"
        );
    }

    #[test]
    fn parse_quantity_ref_devotion_single() {
        let qty = parse_quantity_ref("your devotion to black").unwrap();
        match qty {
            QuantityRef::Devotion { colors } => {
                assert_eq!(colors, vec![ManaColor::Black]);
            }
            other => panic!("Expected Devotion, got {other:?}"),
        }
    }

    #[test]
    fn parse_quantity_ref_devotion_multi() {
        let qty = parse_quantity_ref("your devotion to black and red").unwrap();
        match qty {
            QuantityRef::Devotion { colors } => {
                assert_eq!(colors.len(), 2);
                assert!(colors.contains(&ManaColor::Black));
                assert!(colors.contains(&ManaColor::Red));
            }
            other => panic!("Expected Devotion, got {other:?}"),
        }
    }

    #[test]
    fn cda_quantity_self_power() {
        let qty = parse_cda_quantity("~'s power").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::SelfPower
            }
        ));
    }

    #[test]
    fn cda_quantity_self_toughness() {
        let qty = parse_cda_quantity("this creature's toughness").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::SelfToughness
            }
        ));
    }

    #[test]
    fn cda_quantity_opponents() {
        let qty = parse_cda_quantity("the number of opponents you have").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::Opponent
                }
            }
        ));
    }

    #[test]
    fn cda_quantity_counters_on_self() {
        let qty = parse_cda_quantity("the number of +1/+1 counters on ~").unwrap();
        match qty {
            QuantityExpr::Ref {
                qty: QuantityRef::CountersOnSelf { counter_type },
            } => assert_eq!(counter_type, "P1P1"),
            other => panic!("Expected CountersOnSelf, got {other:?}"),
        }
    }

    #[test]
    fn cda_quantity_greatest_power() {
        let qty = parse_cda_quantity("the greatest power among creatures you control").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Max,
                    property: ObjectProperty::Power,
                    ..
                }
            }
        ));
    }

    #[test]
    fn cda_quantity_greatest_toughness() {
        let qty = parse_cda_quantity("the greatest toughness among creatures you control").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Max,
                    property: ObjectProperty::Toughness,
                    ..
                }
            }
        ));
    }

    #[test]
    fn cda_quantity_greatest_mana_value() {
        let qty =
            parse_cda_quantity("the greatest mana value among creatures you control").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Max,
                    property: ObjectProperty::ManaValue,
                    ..
                }
            }
        ));
    }

    #[test]
    fn cda_quantity_greatest_mana_value_in_exile() {
        let qty = parse_cda_quantity("the greatest mana value among cards in exile").unwrap();
        match &qty {
            QuantityExpr::Ref {
                qty:
                    QuantityRef::Aggregate {
                        function: AggregateFunction::Max,
                        property: ObjectProperty::ManaValue,
                        filter,
                    },
            } => {
                // Filter should contain InZone(Exile), not be Any
                assert!(
                    !matches!(filter, TargetFilter::Any),
                    "expected non-Any filter for 'cards in exile', got {filter:?}"
                );
            }
            other => panic!("Expected Aggregate(Max, ManaValue), got {other:?}"),
        }
    }

    #[test]
    fn cda_quantity_total_power() {
        let qty = parse_cda_quantity("the total power of creatures you control").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Sum,
                    property: ObjectProperty::Power,
                    ..
                }
            }
        ));
    }

    #[test]
    fn cda_quantity_twice() {
        let qty = parse_cda_quantity("twice the number of creatures you control").unwrap();
        match qty {
            QuantityExpr::Multiply { factor, inner } => {
                assert_eq!(factor, 2);
                assert!(matches!(
                    *inner,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { .. }
                    }
                ));
            }
            other => panic!("Expected Multiply, got {other:?}"),
        }
    }

    #[test]
    fn cda_quantity_n_plus_inner() {
        let qty = parse_cda_quantity("1 plus the number of creatures you control").unwrap();
        match qty {
            QuantityExpr::Offset { inner, offset } => {
                assert_eq!(offset, 1);
                assert!(matches!(
                    *inner,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { .. }
                    }
                ));
            }
            other => panic!("Expected Offset, got {other:?}"),
        }
    }

    #[test]
    fn parse_event_context_quantity_that_much() {
        let result = parse_event_context_quantity("that much");
        assert_eq!(
            result,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            })
        );
    }

    #[test]
    fn parse_event_context_quantity_its_power() {
        assert_eq!(
            parse_event_context_quantity("its power"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextSourcePower
            })
        );
    }

    #[test]
    fn parse_event_context_quantity_its_toughness() {
        assert_eq!(
            parse_event_context_quantity("its toughness"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextSourceToughness
            })
        );
    }

    #[test]
    fn parse_event_context_quantity_its_mana_value() {
        assert_eq!(
            parse_event_context_quantity("its mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextSourceManaValue
            })
        );
    }

    #[test]
    fn parse_event_context_quantity_spell_mana_value() {
        assert_eq!(
            parse_event_context_quantity("that spell's mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextSourceManaValue
            })
        );
    }

    #[test]
    fn parse_event_context_quantity_unrecognized_returns_none() {
        assert_eq!(
            parse_event_context_quantity("the number of creatures you control"),
            None
        );
    }

    #[test]
    fn parse_event_context_quantity_life_lost_this_turn() {
        assert_eq!(
            parse_event_context_quantity("the life you've lost this turn"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::LifeLostThisTurn
            })
        );
    }

    #[test]
    fn parse_event_context_quantity_life_gained_this_turn() {
        assert_eq!(
            parse_event_context_quantity("the life you gained this turn"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn
            })
        );
    }

    #[test]
    fn parse_event_context_possessive_sacrificed_creature_power() {
        assert_eq!(
            parse_event_context_quantity("the sacrificed creature's power"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextSourcePower
            })
        );
    }

    #[test]
    fn parse_event_context_possessive_that_creature_toughness() {
        assert_eq!(
            parse_event_context_quantity("that creature's toughness"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextSourceToughness
            })
        );
    }

    #[test]
    fn parse_event_context_possessive_exiled_card_mana_value() {
        assert_eq!(
            parse_event_context_quantity("the exiled card's mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextSourceManaValue
            })
        );
    }

    #[test]
    fn parse_event_context_possessive_destroyed_creature_power() {
        assert_eq!(
            parse_event_context_quantity("the destroyed creature's power"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextSourcePower
            })
        );
    }

    #[test]
    fn parse_event_context_possessive_rejects_target() {
        // "target creature" is a targeting referent, not event context
        assert_eq!(
            parse_event_context_quantity("target creature's power"),
            None
        );
    }

    #[test]
    fn parse_event_context_possessive_rejects_player() {
        // Player possessives are not event context
        assert_eq!(
            parse_event_context_quantity("each opponent's life total"),
            None
        );
    }

    #[test]
    fn for_each_card_in_hand_via_quantity_ref() {
        let qty = parse_for_each_clause("card in your hand").unwrap();
        assert!(
            matches!(qty, QuantityRef::HandSize),
            "Expected HandSize, got {qty:?}"
        );
    }

    #[test]
    fn for_each_card_in_graveyard() {
        let qty = parse_for_each_clause("card in your graveyard").unwrap();
        assert!(
            matches!(qty, QuantityRef::GraveyardSize),
            "Expected GraveyardSize, got {qty:?}"
        );
    }

    #[test]
    fn for_each_creature_still_works() {
        let qty = parse_for_each_clause("creature you control").unwrap();
        assert!(
            matches!(qty, QuantityRef::ObjectCount { .. }),
            "Expected ObjectCount, got {qty:?}"
        );
    }

    #[test]
    fn parse_event_context_life_lost_this_turn() {
        // With "this turn" suffix (before duration stripping)
        assert_eq!(
            parse_event_context_quantity("the life you've lost this turn"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::LifeLostThisTurn
            })
        );
        // Without "this turn" suffix (after duration stripping)
        assert_eq!(
            parse_event_context_quantity("the life you've lost"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::LifeLostThisTurn
            })
        );
    }

    #[test]
    fn parse_event_context_life_gained_this_turn() {
        assert_eq!(
            parse_event_context_quantity("the life you've gained this turn"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn
            })
        );
        assert_eq!(
            parse_event_context_quantity("the life you've gained"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn
            })
        );
    }

    #[test]
    fn parse_quantity_ref_life_lost() {
        assert_eq!(
            parse_quantity_ref("life you've lost"),
            Some(QuantityRef::LifeLostThisTurn)
        );
    }

    #[test]
    fn cda_instant_and_sorcery_graveyard_count() {
        let result =
            parse_cda_quantity("the number of instant and sorcery cards in your graveyard");
        let qty = result.expect("Should parse instant and sorcery CDA");
        match qty {
            QuantityExpr::Ref {
                qty:
                    QuantityRef::ZoneCardCount {
                        zone,
                        card_types,
                        scope,
                    },
            } => {
                assert_eq!(zone, ZoneRef::Graveyard);
                assert_eq!(card_types.len(), 2, "Should have both Instant and Sorcery");
                assert!(card_types.contains(&TypeFilter::Instant));
                assert!(card_types.contains(&TypeFilter::Sorcery));
                assert_eq!(scope, CountScope::Controller);
            }
            other => panic!("Expected ZoneCardCount, got {other:?}"),
        }
    }

    #[test]
    fn cda_untyped_graveyard_count_still_works() {
        let result = parse_cda_quantity("the number of cards in your graveyard");
        let qty = result.expect("Should parse untyped graveyard count");
        match qty {
            QuantityExpr::Ref {
                qty:
                    QuantityRef::ZoneCardCount {
                        card_types, zone, ..
                    },
            } => {
                assert_eq!(zone, ZoneRef::Graveyard);
                assert!(
                    card_types.is_empty(),
                    "Untyped should have empty card_types"
                );
            }
            other => panic!("Expected ZoneCardCount, got {other:?}"),
        }
    }
}
