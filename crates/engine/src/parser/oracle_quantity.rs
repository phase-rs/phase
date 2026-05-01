//! Quantity expression parsing from Oracle text.
//!
//! This module consolidates semantic quantity interpretation — mapping Oracle text
//! phrases like "the number of creatures you control" or "your life total" into
//! typed `QuantityRef` / `QuantityExpr` values. This is distinct from `oracle_util`,
//! which provides raw text extraction primitives (number parsing, mana symbol
//! counting, phrase matching).

use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::{all_consuming, value};
use nom::sequence::terminated;
use nom::Parser;
use nom_language::error::VerboseError;

use super::oracle_nom::primitives as nom_primitives;
use super::oracle_nom::quantity as nom_quantity;
use super::oracle_nom::target as nom_target;
use crate::parser::oracle_effect::counter::normalize_counter_type;
use crate::parser::oracle_target::parse_type_phrase;
use crate::types::ability::{
    AggregateFunction, ControllerRef, CountScope, DevotionColors, FilterProp, ObjectProperty,
    ObjectScope, PlayerFilter, PlayerRelation, PlayerScope, QuantityExpr, QuantityRef,
    TargetFilter, TypeFilter, TypedFilter, ZoneRef,
};
use crate::types::events::PlayerActionKind;
use crate::types::mana::ManaColor;
use crate::types::zones::Zone;

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
            return Some(canonicalize_quantity_ref(qty));
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
            return Some(QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: Some(counter_type),
            });
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
            return Some(QuantityRef::CountersOn {
                scope: ObjectScope::Target,
                counter_type: Some(counter_type),
            });
        }
    }

    // "the number of [counter type] counters on [filter]" — total counters across
    // all matching objects, distinct from object count.
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("the number of ").parse(trimmed) {
        for suffix in [
            " counters on ",
            " counter on ",
            " counters among ",
            " counter among ",
        ] {
            let Ok((after_suffix, counter_text)) =
                take_until::<_, _, VerboseError<&str>>(suffix).parse(rest)
            else {
                continue;
            };
            let Ok((after_filter, _)) = tag::<_, _, VerboseError<&str>>(suffix).parse(after_suffix)
            else {
                continue;
            };
            let counter_type = normalize_counter_type(counter_text.trim());
            if counter_type.is_empty() {
                continue;
            }
            let (filter, remainder) = parse_type_phrase(after_filter);
            if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
                return Some(QuantityRef::CountersOnObjects {
                    counter_type: Some(counter_type),
                    filter,
                });
            }
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
    // "your devotion to that color" / "your devotion to {color}" /
    // "your devotion to {color} and {color}"
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("your devotion to ").parse(trimmed) {
        if tag::<_, _, VerboseError<&str>>("that color")
            .parse(rest)
            .is_ok()
        {
            return Some(QuantityRef::Devotion {
                colors: DevotionColors::ChosenColor,
            });
        }
        let colors = parse_devotion_colors(rest);
        if !colors.is_empty() {
            return Some(QuantityRef::Devotion {
                colors: DevotionColors::Fixed(colors),
            });
        }
    }
    None
}

pub(crate) fn canonicalize_quantity_ref(qty: QuantityRef) -> QuantityRef {
    match qty {
        QuantityRef::ZoneCardCount {
            zone: ZoneRef::Hand,
            card_types,
            scope: CountScope::Controller,
        } if card_types.is_empty() => QuantityRef::HandSize {
            player: PlayerScope::Controller,
        },
        QuantityRef::ZoneCardCount {
            zone: ZoneRef::Graveyard,
            card_types,
            scope: CountScope::Controller,
        } if card_types.is_empty() => QuantityRef::GraveyardSize,
        other => other,
    }
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

    // CR 604.3: "N plus [inner]" / "N minus [inner]" generalized offset pattern.
    // Negative form uses Offset with a Multiply-by-(-1) inner, composing cleanly
    // over existing types without introducing new variants.
    if let Ok((rest, (n, sign))) = (
        nom_primitives::parse_number,
        alt((
            value(1i32, tag::<_, _, VerboseError<&str>>(" plus ")),
            value(-1i32, tag(" minus ")),
        )),
    )
        .parse(text)
    {
        if let Some(inner) = parse_cda_quantity(rest) {
            let inner_expr = if sign < 0 {
                QuantityExpr::Multiply {
                    factor: -1,
                    inner: Box::new(inner),
                }
            } else {
                inner
            };
            return Some(QuantityExpr::Offset {
                inner: Box::new(inner_expr),
                offset: n as i32,
            });
        }
    }

    if let Ok((rest, qty)) = nom_quantity::parse_quantity_ref.parse(text) {
        if rest.is_empty() {
            return Some(QuantityExpr::Ref {
                qty: canonicalize_quantity_ref(qty),
            });
        }
    }

    // "the number of card types among cards in all graveyards"
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
/// Parse event-context quantity references from Oracle text fragments.
/// Returns None for unrecognized patterns (caller falls back to Variable).
pub(crate) fn parse_event_context_quantity(text: &str) -> Option<QuantityExpr> {
    let lower = text.to_lowercase();
    let lower = lower.trim();
    // CR 609.3: "the life/damage lost/dealt this way" — numeric result from preceding effect.
    // Must check before "that much" to avoid false match on "this way" vs. "this turn".
    if lower.ends_with("this way")
        && (lower.contains("life lost")
            || lower.contains("damage dealt")
            || lower.contains("life paid"))
    {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::PreviousEffectAmount,
        });
    }

    // CR 615.5 + CR 609.7: "[the] damage prevented this way" — same shape as
    // the bare form already recognized by `parse_quantity_ref`, but as a
    // complete quantity expression (e.g. "draws cards equal to the damage
    // prevented this way" — Swans of Bryn Argoll). Resolves via
    // `EventContextAmount`, which the prevention applier stamps into
    // `last_effect_count`. Single combinator: optional "the " determiner
    // composed via `nom::combinator::opt` over the bare phrase tag.
    if nom::combinator::all_consuming(nom::sequence::preceded(
        nom::combinator::opt(tag::<_, _, VerboseError<&str>>("the ")),
        tag::<_, _, VerboseError<&str>>("damage prevented this way"),
    ))
    .parse(lower)
    .is_ok()
    {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        });
    }

    match lower {
        // allow-noncombinator: dispatching on already-classified pre-trimmed phrase
        "that much" | "that many" => {
            return Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            })
        }
        // CR 706.2: "the result" of a coin flip / die roll — the result amount
        // is exposed via the same EventContextAmount channel that "that much" /
        // "that many" use (Adorable Kitten "You gain life equal to the result"
        // after roll-a-die). Both compile to the same runtime resolver.
        // allow-noncombinator: dispatching on already-classified pre-trimmed phrase
        "the result" => {
            return Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            });
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

    // CR 601.2h: "the amount of mana spent to cast <subject>" — dynamic amount
    // referring to the actual paid cost of a spell. `this spell` / `it` / `~`
    // resolve against the ability's source object (Molten Note); `that spell`
    // resolves against the triggering event's source (Adamant family,
    // Expressive Firedancer conditional rider).
    if let Some(qty) = parse_mana_spent_to_cast_amount(lower) {
        return Some(QuantityExpr::Ref { qty });
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

    // CR 604.3: Composite quantity expressions ("N plus/minus [inner]", "twice [inner]")
    // delegate to parse_cda_quantity — the single authority for offset/multiply grammar.
    // Limited to composite variants so atomic refs still flow through the
    // TargetPower/TargetLifeTotal exclusion in the fallback below.
    if let Some(qty @ (QuantityExpr::Offset { .. } | QuantityExpr::Multiply { .. })) =
        parse_cda_quantity(lower)
    {
        return Some(qty);
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
        if !matches!(
            qty,
            QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Target
            } | QuantityRef::LifeTotal {
                player: PlayerScope::Target
            }
        ) {
            return Some(QuantityExpr::Ref { qty });
        }
    }

    None
}

/// CR 601.2h: Recognize "the amount of mana [you] spent to cast <subject>" /
/// "the amount of mana spent to cast <subject>" and map the subject phrase to
/// the correct `QuantityRef`.
///
/// - `this spell` / `it` / `~` / `this creature` → self-scoped spent-mana ref (spell
///   resolution reading its own cost; Molten Note).
/// - `that spell` / `that creature` → triggering-spell spent-mana ref (trigger
///   effect reading the triggering spell's cost; Wildgrowth Archaic,
///   Expressive Firedancer rider, Mana Sculpt rider).
fn parse_mana_spent_to_cast_amount(input: &str) -> Option<QuantityRef> {
    // Consume optional leading "the ".
    let rest = tag::<_, _, VerboseError<&str>>("the ")
        .parse(input)
        .map_or(input, |(r, _)| r);
    // Consume the core phrase. Accept both "mana you spent" and "mana spent".
    let rest = alt((
        value(
            (),
            tag::<_, _, VerboseError<&str>>("amount of mana you spent to cast "),
        ),
        value((), tag("amount of mana spent to cast ")),
    ))
    .parse(rest)
    .ok()?
    .0;
    // Dispatch on subject: self-referential vs triggering-spell anaphora.
    alt((
        value(
            QuantityRef::ManaSpentToCast {
                scope: crate::types::ability::CastManaObjectScope::SelfObject,
                metric: crate::types::ability::CastManaSpentMetric::Total,
            },
            alt((
                tag::<_, _, VerboseError<&str>>("this spell"),
                tag("this creature"),
                tag("it"),
                tag("~"),
            )),
        ),
        value(
            QuantityRef::ManaSpentToCast {
                scope: crate::types::ability::CastManaObjectScope::TriggeringSpell,
                metric: crate::types::ability::CastManaSpentMetric::Total,
            },
            alt((tag("that spell"), tag("that creature"))),
        ),
    ))
    .parse(rest)
    .ok()
    .map(|(_, qty)| qty)
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

/// CR 400.7 + CR 608.2c: Match "<noun> exiled from <possessive> hand this way"
/// — used by Deadly Cover-Up's "draws a card for each card exiled from their
/// hand this way." Tries the `exiled from <possessive> hand` combinator at
/// every word boundary and returns `Some(())` on the first match.
fn try_parse_exiled_from_hand_this_way(lower: &str) -> Option<()> {
    crate::parser::oracle_nom::primitives::scan_at_word_boundaries(lower, |input| {
        let (rest, _) = tag::<_, _, VerboseError<&str>>("exiled from ").parse(input)?;
        let (rest, _) = alt((
            value((), tag::<_, _, VerboseError<&str>>("their hand")),
            value((), tag("its owner's hand")),
            value((), tag("that player's hand")),
        ))
        .parse(rest)?;
        Ok((rest, ()))
    })
}

/// CR 609.3 + CR 122.1: Detect "counter[s] removed this way" — the for-each
/// quantifier shape produced by cards that drain self-counters and reference
/// the count in a downstream effect (Coalition Relic, Storage Counter cycle).
///
/// We accept the singular and plural forms with or without a leading
/// counter-type word. The combinator is run at every word boundary so the
/// surrounding clause can be either "counter removed this way",
/// "counters removed this way", or "<type> counter[s] removed this way".
/// The counter-type word, when present, is intentionally NOT extracted —
/// the resolved quantity is whatever the parent `Effect::RemoveCounter`
/// removed, and the parent already restricts by counter type.
fn try_parse_counters_removed_this_way(lower: &str) -> bool {
    crate::parser::oracle_nom::primitives::scan_at_word_boundaries(lower, |input| {
        let (rest, _) = alt((
            value((), tag::<_, _, VerboseError<&str>>("counters")),
            value((), tag::<_, _, VerboseError<&str>>("counter")),
        ))
        .parse(input)?;
        let (rest, _) = tag(" removed this way").parse(rest)?;
        Ok((rest, ()))
    })
    .is_some()
}

/// Parse the clause after "for each" into a `QuantityExpr`, supporting
/// the conjunction form "X and each Y" by emitting `QuantityExpr::Sum`.
/// A single-segment clause delegates to `parse_for_each_clause` and
/// returns a bare `Ref` to avoid a degenerate `Sum` with one element.
///
/// Class: A-Alrund ("+1/+1 for each card in your hand and each foretold
/// card you own in exile") and ~21 similar cards in the database.
pub(crate) fn parse_for_each_clause_expr(clause: &str) -> Option<QuantityExpr> {
    use nom::branch::alt;
    use nom::bytes::complete::{tag, take_until};
    use nom::combinator::rest;
    use nom::multi::separated_list1;

    let clause = clause.trim().trim_end_matches('.');

    if let Ok((rest, expr)) = parse_target_hand_type_or_color_clause(clause) {
        if rest.is_empty() {
            return Some(expr);
        }
    }

    if let Ok((rest, expr)) = parse_for_each_beyond_first_clause_expr(clause) {
        if rest.is_empty() {
            return Some(expr);
        }
    }

    fn segment(i: &str) -> nom::IResult<&str, &str, VerboseError<&str>> {
        alt((take_until(" and each "), rest)).parse(i)
    }
    let mut split = separated_list1(tag::<_, _, VerboseError<&str>>(" and each "), segment);
    let segments: Vec<&str> = split
        .parse(clause)
        .map(|(_, v)| v)
        .unwrap_or_else(|_| vec![clause]);

    let refs: Option<Vec<QuantityRef>> = segments
        .iter()
        .map(|s| parse_for_each_clause(s.trim()))
        .collect();
    let mut exprs: Vec<QuantityExpr> = refs?
        .into_iter()
        .map(|qty| QuantityExpr::Ref { qty })
        .collect();
    if exprs.len() == 1 {
        return exprs.pop();
    }
    Some(QuantityExpr::Sum { exprs })
}

/// CR 702.23a: "for each [object] beyond the first" composes a
/// normal object-count quantity with an offset of -1. This preserves the
/// shared `for each` grammar and keeps "beyond the first" as an expression
/// modifier rather than adding a leaf-level `QuantityRef` variant.
fn parse_for_each_beyond_first_clause_expr(
    input: &str,
) -> nom::IResult<&str, QuantityExpr, VerboseError<&str>> {
    let (input, base_clause) =
        terminated(take_until(" beyond the first"), tag(" beyond the first")).parse(input)?;
    let (_, qty) = all_consuming(nom_quantity::parse_for_each_clause_ref).parse(base_clause)?;
    Ok((
        input,
        QuantityExpr::Offset {
            inner: Box::new(QuantityExpr::Ref { qty }),
            offset: -1,
        },
    ))
}

/// CR 109.4 + CR 400.1 + CR 608.2c: Parse an anaphoric hand-card union like
/// "Mountain and red card in it" after "target opponent reveals their hand".
/// The pronoun "it" refers to the targeted player's hand; the two filter atoms
/// are a disjunction, so a red Mountain is counted once.
fn parse_target_hand_type_or_color_clause(
    input: &str,
) -> nom::IResult<&str, QuantityExpr, VerboseError<&str>> {
    let (input, type_filter) = nom_target::parse_type_filter_word(input)?;
    let (input, _) = tag(" and ").parse(input)?;
    let (input, color) = nom_primitives::parse_color(input)?;
    let (input, _) = tag(" card").parse(input)?;
    let (input, _) = nom::combinator::opt(tag("s")).parse(input)?;
    let (input, _) = tag(" in it").parse(input)?;

    Ok((
        input,
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Or {
                    filters: vec![
                        target_hand_card_filter(vec![type_filter], Vec::new()),
                        target_hand_card_filter(
                            vec![TypeFilter::Card],
                            vec![FilterProp::HasColor { color }],
                        ),
                    ],
                },
            },
        },
    ))
}

fn target_hand_card_filter(
    type_filters: Vec<TypeFilter>,
    mut properties: Vec<FilterProp>,
) -> TargetFilter {
    properties.push(FilterProp::InZone { zone: Zone::Hand });
    properties.push(FilterProp::Owned {
        controller: ControllerRef::TargetPlayer,
    });
    TargetFilter::Typed(TypedFilter {
        type_filters,
        controller: None,
        properties,
    })
}

/// CR 608.2c + CR 109.5: Recognize "opponent who searched their library this
/// way" as a player-action quantity. The runtime accumulator is keyed by
/// `GameEvent::PlayerPerformedAction`, not by zone changes, so it still counts
/// a player who searched and failed to find.
fn parse_opponent_searched_library_this_way(
    input: &str,
) -> nom::IResult<&str, (), VerboseError<&str>> {
    let (input, _) = tag("opponent who ").parse(input)?;
    let (input, _) = alt((tag("searches"), tag("searched"))).parse(input)?;
    let (input, _) = tag(" ").parse(input)?;
    let (input, _) = alt((tag("a "), tag("their "))).parse(input)?;
    let (input, _) = tag("library this way").parse(input)?;
    Ok((input, ()))
}

/// Parse the clause after "for each" into a QuantityRef.
pub(crate) fn parse_for_each_clause(clause: &str) -> Option<QuantityRef> {
    let clause = clause.trim().trim_end_matches('.');

    if let Some(qty) = parse_for_each_kicker_count(clause) {
        return Some(qty);
    }

    if let Ok((rest, qty)) = nom_quantity::parse_for_each_clause_ref.parse(clause) {
        if rest.is_empty() {
            return Some(qty);
        }
    }

    // CR 106.1 + CR 109.1: "color among [type-phrase]" — distinct colors among
    // matching objects. Used by Faeburrow Elder's "+1/+1 for each color among
    // permanents you control" and by the Converge mechanic adjacent class.
    if let Ok((after_among, _)) = tag::<_, _, VerboseError<&str>>("color among ").parse(clause) {
        let (filter, remainder) = parse_type_phrase(after_among);
        if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
            return Some(QuantityRef::DistinctColorsAmongPermanents { filter });
        }
    }

    // "card put into a graveyard this way" / "creature card exiled this way" / etc.
    // "this way" references objects from the preceding effect's tracked set.
    if clause.contains("this way") {
        // CR 400.7 + CR 608.2c: "card exiled from [possessive] hand this way" —
        // hand-origin exiles only (Deadly Cover-Up). Resolves against the
        // dedicated per-resolution counter populated by `ChangeZoneAll`.
        let lower = clause.to_ascii_lowercase();
        if try_parse_exiled_from_hand_this_way(&lower).is_some() {
            return Some(QuantityRef::ExiledFromHandThisResolution);
        }
        // CR 615.5: "1 damage prevented this way" — the post-replacement
        // follow-up references the prevented amount. The prevention applier
        // emits `GameEvent::DamagePrevented` and stamps `last_effect_count`
        // with the prevented amount; both feed `EventContextAmount`. Class:
        // Phyrexian Hydra, Vigor, Stormwild Capridor, Hostility.
        if lower == "1 damage prevented this way" || lower == "damage prevented this way" {
            return Some(QuantityRef::EventContextAmount);
        }
        // CR 608.2c + CR 109.5: "opponent who searches/searched [a/their] library
        // this way" — Tempting Offer cycle's bonus-tutor-per-accepting-opponent
        // step. A single nom combinator handles all four (verb tense × article)
        // permutations, returning a player-count quantity rather than the
        // object-count `TrackedSetSize` fallback below. Must be tried before that
        // fallback because every "opponent who … this way" clause does contain
        // "this way".
        if let Ok((rest, ())) = parse_opponent_searched_library_this_way(lower.as_str()) {
            if rest.is_empty() {
                return Some(QuantityRef::PlayerCount {
                    filter: PlayerFilter::PerformedActionThisWay {
                        relation: PlayerRelation::Opponent,
                        action: PlayerActionKind::SearchedLibrary,
                    },
                });
            }
        }
        // CR 609.3 + CR 122.1: "[counter-type] counter[s] removed this way" — the
        // numeric amount of counters removed by the preceding `Effect::RemoveCounter`
        // in the sub-ability chain. The parent-effect-aware scan in
        // `effects/mod.rs` reads `GameEvent::CounterRemoved` for RemoveCounter
        // parents and stamps `state.last_effect_amount`, which
        // `PreviousEffectAmount` reads.
        //
        // Class: Coalition Relic ("you may remove all charge counters from ~. If
        // you do, add one mana of any color for each charge counter removed this
        // way."), the Ice Age Storage Counter cycle (Saprazzan Cove, Dwarven
        // Hold, Hollow Trees, Mercadian Bazaar), and any future card that
        // references the count of counters removed by a preceding effect.
        //
        // We intentionally do NOT extract the counter-type word: `last_effect_amount`
        // is the count of whatever counter type the parent removed. The English
        // restatement of the type is a redundant gloss, not a quantity-shape
        // distinction. If a future card needs type-discriminated "removed this
        // way" quantities, this is the right place to extend.
        if try_parse_counters_removed_this_way(&lower) {
            return Some(QuantityRef::PreviousEffectAmount);
        }
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

    // CR 109.1 + CR 122.1: "[type] you control with a [counter] counter on it" —
    // objects matching a type filter AND bearing at least one counter of the given
    // type. The filter is the type-phrase plus a `FilterProp::CountersGE { count: 1 }`.
    // This must be checked BEFORE the self-counter fallback below, which would
    // otherwise misroute any clause containing "counter on" to CountersOnSelf and
    // discard the subject type phrase (Inspiring Call bug: "creature you control
    // with a +1/+1 counter on it" → CountersOnSelf{ "creature you control with a +1/+1" }).
    if let Ok((_, type_part)) = take_until::<_, _, VerboseError<&str>>(" with ").parse(clause) {
        let suffix_part = &clause[type_part.len() + 1..]; // starts at "with "
        if let Some((counter_prop, consumed)) =
            crate::parser::oracle_target::parse_counter_suffix(suffix_part)
        {
            // The counter suffix must consume the rest of the clause (possibly with
            // trailing whitespace / punctuation already stripped by trim_end_matches).
            if suffix_part[consumed..].trim().is_empty() {
                let (filter, type_rest) = parse_type_phrase(type_part);
                if type_rest.trim().is_empty() {
                    // Compose: attach the counter property onto the typed filter.
                    // parse_type_phrase always emits TargetFilter::Typed for non-Any
                    // returns, so the other branch is defensive.
                    if let TargetFilter::Typed(typed) = filter {
                        let mut props = typed.properties.clone();
                        props.push(counter_prop);
                        return Some(QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(typed.properties(props)),
                        });
                    }
                }
            }
        }
    }

    // "[counter type] counter on ~" / "[counter type] counter on it"
    if clause.contains("counter on") {
        let raw_type = clause.split("counter").next().unwrap_or("").trim();
        if !raw_type.is_empty() {
            return Some(QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: Some(normalize_counter_type(raw_type)),
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
    // Direct dispatch before type-phrase fallback to handle spell-casting quantity patterns.
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

    // CR 603.10a + CR 603.6e: "[Aura|Equipment] you controlled that was attached to it"
    // — look-back count on a leaving object's attachment snapshot. Used by
    // Hateful Eidolon's "draw a card for each Aura you controlled that was attached
    // to it". Recognize only this specific non-compositional pattern; controller is
    // "you" (the clause past-tense "controlled" with "you" — parallel to Oracle's
    // convention that the dying enchanted creature's Auras are yours).
    {
        use crate::types::ability::{AttachmentKind, ControllerRef};
        let lower_clause = clause.to_ascii_lowercase();
        let attach_pairs: &[(&str, AttachmentKind)] = &[
            (
                "aura you controlled that was attached to it",
                AttachmentKind::Aura,
            ),
            (
                "equipment you controlled that was attached to it",
                AttachmentKind::Equipment,
            ),
        ];
        for (pat, kind) in attach_pairs {
            if lower_clause == *pat {
                return Some(QuantityRef::AttachmentsOnLeavingObject {
                    kind: kind.clone(),
                    controller: Some(ControllerRef::You),
                });
            }
        }
    }

    if let Some(qty) = parse_for_each_target_controlled_type(clause) {
        return Some(qty);
    }

    // "creature you control", "artifact you control", etc.
    // Use parse_type_phrase (not parse_target) to avoid generating spurious
    // target-fallback warnings for quantity text that isn't a target clause.
    let (filter, remainder) = parse_type_phrase(clause);
    if !matches!(filter, TargetFilter::Any) && remainder.trim().is_empty() {
        return Some(QuantityRef::ObjectCount { filter });
    }

    None
}

fn parse_for_each_kicker_count(clause: &str) -> Option<QuantityRef> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("time ")
        .parse(clause)
        .ok()?;
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>("it was kicked"),
        tag("this spell was kicked"),
    ))
    .parse(rest)
    .ok()?;
    rest.is_empty().then_some(QuantityRef::KickerCount)
}

fn parse_for_each_target_controlled_type(clause: &str) -> Option<QuantityRef> {
    let (rest, type_text) = alt((
        terminated(
            take_until::<_, _, VerboseError<&str>>(" target opponent controls"),
            tag(" target opponent controls"),
        ),
        terminated(
            take_until::<_, _, VerboseError<&str>>(" target player controls"),
            tag(" target player controls"),
        ),
    ))
    .parse(clause)
    .ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    let (filter, remainder) = parse_type_phrase(type_text);
    if remainder.trim().is_empty() {
        with_target_player_controller(filter).map(|filter| QuantityRef::ObjectCount { filter })
    } else {
        None
    }
}

fn with_target_player_controller(filter: TargetFilter) -> Option<TargetFilter> {
    match filter {
        TargetFilter::Typed(mut typed) => {
            typed.controller = Some(ControllerRef::TargetPlayer);
            Some(TargetFilter::Typed(typed))
        }
        TargetFilter::Or { filters } => filters
            .into_iter()
            .map(with_target_player_controller)
            .collect::<Option<Vec<_>>>()
            .map(|filters| TargetFilter::Or { filters }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        CardTypeSetSource, ControllerRef, FilterProp, TypeFilter, TypedFilter,
    };
    use crate::types::mana::ManaColor;

    #[test]
    fn for_each_counter_on_self_normalized() {
        let qty = parse_for_each_clause("+1/+1 counter on ~").unwrap();
        match qty {
            QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: Some(counter_type),
            } => assert_eq!(counter_type, "P1P1"),
            other => panic!("Expected CountersOn{{Source, P1P1}}, got {other:?}"),
        }
    }

    #[test]
    fn for_each_singular_counter_on_self() {
        // Singular "counter on ~" (not "counters on ~")
        let qty = parse_for_each_clause("blight counter on it").unwrap();
        assert!(
            matches!(qty, QuantityRef::CountersOn { scope: ObjectScope::Source, counter_type: Some(ref counter_type) } if counter_type == "blight"),
            "singular counter form should produce CountersOnSelf"
        );
    }

    #[test]
    fn for_each_time_it_was_kicked_maps_to_kicker_count() {
        assert_eq!(
            parse_for_each_clause("time it was kicked"),
            Some(QuantityRef::KickerCount)
        );
        assert_eq!(
            parse_for_each_clause("time this spell was kicked"),
            Some(QuantityRef::KickerCount)
        );
    }

    #[test]
    fn for_each_counter_on_that_creature() {
        let qty = parse_for_each_clause("+1/+1 counter on that creature").unwrap();
        assert!(
            matches!(qty, QuantityRef::CountersOn { scope: ObjectScope::Target, counter_type: Some(ref counter_type) } if counter_type == "P1P1"),
            "counter on that creature should produce CountersOnTarget, not CountersOnSelf"
        );
    }

    #[test]
    fn for_each_this_way_produces_tracked_set_size() {
        let qty = parse_for_each_clause("card put into a graveyard this way").unwrap();
        assert_eq!(qty, QuantityRef::TrackedSetSize);
    }

    /// CR 609.3 + CR 122.1: "[type] counter[s] removed this way" must dispatch
    /// to `PreviousEffectAmount` so the resolver picks up the actual count of
    /// counters removed by the parent `Effect::RemoveCounter`. Coalition Relic
    /// and the Storage Counter cycle depend on this dispatch — without it, the
    /// generic `TrackedSetSize` fallback returns the count of *objects* affected
    /// (always 1 for a self-counter-removal), which is wrong.
    #[test]
    fn for_each_charge_counter_removed_this_way_is_previous_effect_amount() {
        let qty = parse_for_each_clause("charge counter removed this way").unwrap();
        assert_eq!(qty, QuantityRef::PreviousEffectAmount);
    }

    #[test]
    fn for_each_charge_counters_removed_this_way_is_previous_effect_amount() {
        // Plural variant — same dispatch.
        let qty = parse_for_each_clause("charge counters removed this way").unwrap();
        assert_eq!(qty, QuantityRef::PreviousEffectAmount);
    }

    #[test]
    fn for_each_counter_removed_this_way_is_previous_effect_amount() {
        // Untyped (no leading counter-type word). The runtime amount is whatever
        // the parent removed; the omitted English type word is informational.
        let qty = parse_for_each_clause("counter removed this way").unwrap();
        assert_eq!(qty, QuantityRef::PreviousEffectAmount);
    }

    #[test]
    fn for_each_storage_counter_removed_this_way_is_previous_effect_amount() {
        // Storage Counter cycle (Saprazzan Cove etc.) — same shape, different
        // counter type. Must produce the same dispatch.
        let qty = parse_for_each_clause("storage counter removed this way").unwrap();
        assert_eq!(qty, QuantityRef::PreviousEffectAmount);
    }

    #[test]
    fn for_each_creature_attacking_you_counts_attacking_controller() {
        let qty = parse_for_each_clause("creature attacking you").unwrap();
        assert_eq!(
            qty,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(
                    TypedFilter::creature().properties(vec![FilterProp::AttackingController])
                ),
            },
        );
    }

    #[test]
    fn for_each_creature_on_the_battlefield_counts_battlefield_creatures() {
        let qty = parse_for_each_clause("creature on the battlefield").unwrap();
        assert_eq!(
            qty,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature()),
            },
        );
    }

    #[test]
    fn quantity_ref_counters_on_target() {
        let qty = parse_quantity_ref("+1/+1 counters on that creature").unwrap();
        assert!(
            matches!(qty, QuantityRef::CountersOn { scope: ObjectScope::Target, counter_type: Some(ref counter_type) } if counter_type == "P1P1"),
            "counters on that creature should produce CountersOnTarget"
        );
    }

    #[test]
    fn quantity_ref_singular_counter_on_target() {
        let qty = parse_quantity_ref("charge counter on that permanent").unwrap();
        assert!(
            matches!(qty, QuantityRef::CountersOn { scope: ObjectScope::Target, counter_type: Some(ref counter_type) } if counter_type == "charge"),
            "singular counter on that permanent should produce CountersOnTarget"
        );
    }

    #[test]
    fn quantity_ref_counters_on_objects() {
        let qty = parse_quantity_ref("the number of +1/+1 counters on lands you control").unwrap();
        match qty {
            QuantityRef::CountersOnObjects {
                counter_type,
                filter,
            } => {
                assert_eq!(counter_type, Some("P1P1".to_string()));
                assert!(
                    !matches!(filter, TargetFilter::Any),
                    "expected a concrete land filter, got {filter:?}"
                );
            }
            other => panic!("Expected CountersOnObjects, got {other:?}"),
        }
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
                assert_eq!(colors, DevotionColors::Fixed(vec![ManaColor::Black]));
            }
            other => panic!("Expected Devotion, got {other:?}"),
        }
    }

    #[test]
    fn parse_quantity_ref_devotion_chosen_color() {
        let qty = parse_quantity_ref("your devotion to that color").unwrap();
        assert_eq!(
            qty,
            QuantityRef::Devotion {
                colors: DevotionColors::ChosenColor
            }
        );
    }

    #[test]
    fn parse_quantity_ref_devotion_multi() {
        let qty = parse_quantity_ref("your devotion to black and red").unwrap();
        match qty {
            QuantityRef::Devotion { colors } => {
                let DevotionColors::Fixed(colors) = colors else {
                    panic!("expected fixed devotion colors");
                };
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
                qty: QuantityRef::Power {
                    scope: crate::types::ability::ObjectScope::Source
                }
            }
        ));
    }

    #[test]
    fn cda_quantity_self_toughness() {
        let qty = parse_cda_quantity("this creature's toughness").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::Toughness {
                    scope: crate::types::ability::ObjectScope::Source
                }
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
    fn cda_quantity_total_cards_in_all_players_hands() {
        let qty = parse_cda_quantity("the total number of cards in all players' hands").unwrap();
        assert_eq!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::HandSize {
                    player: PlayerScope::AllPlayers {
                        aggregate: AggregateFunction::Sum,
                    },
                },
            }
        );
    }

    #[test]
    fn cda_quantity_counters_on_self() {
        let qty = parse_cda_quantity("the number of +1/+1 counters on ~").unwrap();
        match qty {
            QuantityExpr::Ref {
                qty:
                    QuantityRef::CountersOn {
                        scope: ObjectScope::Source,
                        counter_type: Some(counter_type),
                    },
            } => assert_eq!(counter_type, "P1P1"),
            other => panic!("Expected CountersOn{{Source, P1P1}}, got {other:?}"),
        }
    }

    #[test]
    fn cda_quantity_counters_on_objects() {
        let qty = parse_cda_quantity("the number of +1/+1 counters on lands you control").unwrap();
        match qty {
            QuantityExpr::Ref {
                qty:
                    QuantityRef::CountersOnObjects {
                        counter_type,
                        filter,
                    },
            } => {
                assert_eq!(counter_type, Some("P1P1".to_string()));
                assert!(
                    !matches!(filter, TargetFilter::Any),
                    "expected a concrete land filter, got {filter:?}"
                );
            }
            other => panic!("Expected CountersOnObjects, got {other:?}"),
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
    fn cda_quantity_mana_value_of_the_exiled_card_uses_linked_exile_aggregate() {
        let qty = parse_cda_quantity("the mana value of the exiled card").unwrap();
        match qty {
            QuantityExpr::Ref {
                qty:
                    QuantityRef::Aggregate {
                        function: AggregateFunction::Sum,
                        property: ObjectProperty::ManaValue,
                        filter: TargetFilter::And { filters },
                    },
            } => {
                assert!(
                    filters
                        .iter()
                        .any(|filter| matches!(filter, TargetFilter::ExiledBySource)),
                    "expected ExiledBySource filter, got {filters:?}"
                );
                assert!(filters.iter().any(|filter| matches!(
                    filter,
                    TargetFilter::Typed(typed)
                        if typed.properties
                            == vec![FilterProp::Owned {
                                controller: ControllerRef::You,
                            }]
                )));
            }
            other => panic!(
                "expected Aggregate(Sum, ManaValue) for linked-exile owner quantity, got {other:?}"
            ),
        }
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
                qty: QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Controller
                }
            })
        );
    }

    #[test]
    fn parse_event_context_quantity_life_gained_this_turn() {
        assert_eq!(
            parse_event_context_quantity("the life you gained this turn"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn {
                    player: PlayerScope::Controller
                }
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
        assert_eq!(
            qty,
            QuantityRef::ZoneCardCount {
                zone: ZoneRef::Hand,
                card_types: vec![],
                scope: CountScope::Controller,
            }
        );
    }

    #[test]
    fn for_each_card_in_graveyard() {
        let qty = parse_for_each_clause("card in your graveyard").unwrap();
        assert_eq!(
            qty,
            QuantityRef::ZoneCardCount {
                zone: ZoneRef::Graveyard,
                card_types: vec![],
                scope: CountScope::Controller,
            }
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
    fn for_each_tapped_creature_target_opponent_controls() {
        let qty = parse_for_each_clause("tapped creature target opponent controls").unwrap();
        match qty {
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(typed),
            } => {
                assert_eq!(typed.controller, Some(ControllerRef::TargetPlayer));
                assert!(
                    typed
                        .type_filters
                        .iter()
                        .any(|type_filter| matches!(type_filter, TypeFilter::Creature)),
                    "expected Creature type filter, got {:?}",
                    typed.type_filters
                );
                assert!(
                    typed
                        .properties
                        .iter()
                        .any(|property| matches!(property, FilterProp::Tapped)),
                    "expected Tapped property, got {:?}",
                    typed.properties
                );
            }
            other => panic!("Expected ObjectCount over Typed filter, got {other:?}"),
        }
    }

    /// CR 608.2c + CR 109.5: Tempt with Discovery's
    /// bonus-tutor-per-accepting-opponent step parses as a player-action count.
    /// Verb tense (searches/searched) and article (a/their) variants produce
    /// the same typed quantity.
    #[test]
    fn for_each_opponent_who_searched_library_this_way_present_their() {
        let qty = parse_for_each_clause("opponent who searches their library this way").unwrap();
        assert_eq!(
            qty,
            QuantityRef::PlayerCount {
                filter: PlayerFilter::PerformedActionThisWay {
                    relation: PlayerRelation::Opponent,
                    action: PlayerActionKind::SearchedLibrary,
                },
            }
        );
    }

    #[test]
    fn for_each_opponent_who_searched_library_this_way_past_a() {
        let qty = parse_for_each_clause("opponent who searched a library this way").unwrap();
        assert_eq!(
            qty,
            QuantityRef::PlayerCount {
                filter: PlayerFilter::PerformedActionThisWay {
                    relation: PlayerRelation::Opponent,
                    action: PlayerActionKind::SearchedLibrary,
                },
            }
        );
    }

    #[test]
    fn for_each_opponent_who_searched_library_this_way_past_their() {
        let qty = parse_for_each_clause("opponent who searched their library this way").unwrap();
        assert_eq!(
            qty,
            QuantityRef::PlayerCount {
                filter: PlayerFilter::PerformedActionThisWay {
                    relation: PlayerRelation::Opponent,
                    action: PlayerActionKind::SearchedLibrary,
                },
            }
        );
    }

    #[test]
    fn for_each_opponent_who_searched_library_this_way_present_a() {
        let qty = parse_for_each_clause("opponent who searches a library this way").unwrap();
        assert_eq!(
            qty,
            QuantityRef::PlayerCount {
                filter: PlayerFilter::PerformedActionThisWay {
                    relation: PlayerRelation::Opponent,
                    action: PlayerActionKind::SearchedLibrary,
                },
            }
        );
    }

    /// CR 106.1 + CR 109.1: "for each color among permanents you control" must
    /// lower to `DistinctColorsAmongPermanents`, not `ObjectCount` over a bogus
    /// "color" subject. Faeburrow Elder class.
    #[test]
    fn for_each_color_among_permanents() {
        let qty = parse_for_each_clause("color among permanents you control").unwrap();
        match qty {
            QuantityRef::DistinctColorsAmongPermanents { filter } => {
                assert!(
                    matches!(filter, TargetFilter::Typed(_)),
                    "expected Typed filter, got {filter:?}"
                );
            }
            other => panic!("Expected DistinctColorsAmongPermanents, got {other:?}"),
        }
    }

    /// CR 109.1 + CR 122.1: "[type] you control with a [counter] counter on it"
    /// lowers to `ObjectCount` over a filter that includes `FilterProp::CountersGE`,
    /// not `CountersOnSelf` over a bogus counter-type string. Inspiring Call class.
    #[test]
    fn for_each_creature_with_counter_on_it() {
        let qty = parse_for_each_clause("creature you control with a +1/+1 counter on it").unwrap();
        match qty {
            QuantityRef::ObjectCount { filter } => match filter {
                TargetFilter::Typed(typed) => {
                    assert_eq!(typed.controller, Some(ControllerRef::You));
                    assert!(
                        typed.properties.iter().any(|p| matches!(
                            p,
                            FilterProp::CountersGE { counter_type, .. }
                                if counter_type == &crate::types::counter::CounterType::Plus1Plus1
                        )),
                        "expected CountersGE(P1P1), got properties {:?}",
                        typed.properties
                    );
                }
                other => panic!("expected Typed filter, got {other:?}"),
            },
            other => panic!("Expected ObjectCount, got {other:?}"),
        }
    }

    #[test]
    fn parse_event_context_life_lost_this_turn() {
        // With "this turn" suffix (before duration stripping)
        assert_eq!(
            parse_event_context_quantity("the life you've lost this turn"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Controller
                }
            })
        );
        // Without "this turn" suffix (after duration stripping)
        assert_eq!(
            parse_event_context_quantity("the life you've lost"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Controller
                }
            })
        );
    }

    #[test]
    fn parse_event_context_life_gained_this_turn() {
        assert_eq!(
            parse_event_context_quantity("the life you've gained this turn"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn {
                    player: PlayerScope::Controller
                }
            })
        );
        assert_eq!(
            parse_event_context_quantity("the life you've gained"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn {
                    player: PlayerScope::Controller
                }
            })
        );
    }

    #[test]
    fn parse_quantity_ref_life_lost() {
        assert_eq!(
            parse_quantity_ref("life you've lost"),
            Some(QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller
            })
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
        assert_eq!(
            result,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::GraveyardSize,
            })
        );
    }

    #[test]
    fn cda_distinct_card_types_in_hand() {
        let result = parse_cda_quantity("the number of card types among cards in your hand");
        assert_eq!(
            result,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::DistinctCardTypes {
                    source: CardTypeSetSource::Zone {
                        zone: ZoneRef::Hand,
                        scope: CountScope::Controller,
                    },
                },
            })
        );
    }

    #[test]
    fn cda_distinct_card_types_among_other_nonland_permanents_you_control() {
        let result = parse_cda_quantity(
            "the number of card types among other nonland permanents you control",
        )
        .unwrap();
        let QuantityExpr::Ref {
            qty:
                QuantityRef::DistinctCardTypes {
                    source:
                        CardTypeSetSource::Objects {
                            filter: TargetFilter::Typed(filter),
                        },
                },
        } = result
        else {
            panic!("expected object-scoped DistinctCardTypes, got {result:?}");
        };
        assert_eq!(filter.controller, Some(ControllerRef::You));
        assert!(filter
            .type_filters
            .iter()
            .any(|type_filter| matches!(type_filter, TypeFilter::Permanent)));
        assert!(filter
            .type_filters
            .iter()
            .any(|type_filter| matches!(type_filter, TypeFilter::Non(inner) if **inner == TypeFilter::Land)));
        assert!(filter
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::Another)));
    }

    /// CR 601.2h: "the amount of mana spent to cast this spell" in a spell
    /// effect context → self-scoped spent-mana ref. Used by Molten Note.
    #[test]
    fn mana_spent_self_this_spell() {
        let result = parse_event_context_quantity("the amount of mana spent to cast this spell");
        assert_eq!(
            result,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: crate::types::ability::CastManaObjectScope::SelfObject,
                    metric: crate::types::ability::CastManaSpentMetric::Total
                }
            })
        );
    }

    /// CR 601.2h: "the amount of mana spent to cast that spell" (anaphoric to
    /// the triggering spell) → triggering-spell spent-mana ref.
    #[test]
    fn mana_spent_that_spell_is_triggering_ref() {
        let result = parse_event_context_quantity("the amount of mana spent to cast that spell");
        assert_eq!(
            result,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: crate::types::ability::CastManaObjectScope::TriggeringSpell,
                    metric: crate::types::ability::CastManaSpentMetric::Total
                }
            })
        );
    }

    /// CR 601.2h: "the amount of mana you spent to cast it" — "you spent"
    /// variant with bare "it" anaphora resolves to self for spell effects.
    #[test]
    fn mana_spent_you_spent_it() {
        let result = parse_event_context_quantity("the amount of mana you spent to cast it");
        assert_eq!(
            result,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: crate::types::ability::CastManaObjectScope::SelfObject,
                    metric: crate::types::ability::CastManaSpentMetric::Total
                }
            })
        );
    }

    // ── parse_for_each_clause_expr — conjunction support ──────────────────

    #[test]
    fn for_each_single_segment_returns_bare_ref() {
        let result = parse_for_each_clause_expr("card in your hand");
        assert!(
            matches!(
                result,
                Some(QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Hand,
                        ..
                    }
                })
            ),
            "expected bare Ref{{ZoneCardCount{{Hand,..}}}}, got {result:?}"
        );
    }

    #[test]
    fn for_each_beyond_the_first_offsets_base_count() {
        let result = parse_for_each_clause_expr("creature blocking it beyond the first");
        let Some(QuantityExpr::Offset { inner, offset }) = result else {
            panic!("expected Offset, got {result:?}");
        };
        assert_eq!(offset, -1);
        match inner.as_ref() {
            QuantityExpr::Ref {
                qty:
                    QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(filter),
                    },
            } => {
                assert_eq!(filter.type_filters, vec![TypeFilter::Creature]);
                assert_eq!(filter.controller, None);
                assert_eq!(filter.properties, vec![FilterProp::BlockingSource]);
            }
            other => panic!("expected blocking-source ObjectCount, got {other:?}"),
        }
    }

    #[test]
    fn for_each_conjunction_returns_sum_of_refs() {
        // Conjunction infrastructure: two segments that BOTH parse on their
        // own should compose into a Sum.
        let result =
            parse_for_each_clause_expr("card in your hand and each card in your graveyard");
        let Some(QuantityExpr::Sum { exprs }) = result else {
            panic!("expected Sum, got {result:?}");
        };
        assert_eq!(exprs.len(), 2, "expected two summed exprs, got {exprs:?}");
        assert!(
            matches!(
                exprs[0],
                QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Hand,
                        ..
                    }
                }
            ),
            "expected ZoneCardCount{{Hand}} for first segment, got {:?}",
            exprs[0]
        );
        assert!(
            matches!(
                exprs[1],
                QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Graveyard,
                        ..
                    }
                }
            ),
            "expected ZoneCardCount{{Graveyard}} for second segment, got {:?}",
            exprs[1]
        );
    }

    #[test]
    fn for_each_conjunction_alrund_shape_returns_sum_of_refs() {
        let result =
            parse_for_each_clause_expr("card in your hand and each foretold card you own in exile");
        let Some(QuantityExpr::Sum { exprs }) = result else {
            panic!("expected Sum, got {result:?}");
        };
        assert_eq!(exprs.len(), 2, "expected two summed exprs, got {exprs:?}");
        assert!(
            matches!(
                exprs[0],
                QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Hand,
                        scope: CountScope::Controller,
                        ..
                    }
                }
            ),
            "expected controller hand count for first segment, got {:?}",
            exprs[0]
        );
        match &exprs[1] {
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            } => match filter {
                TargetFilter::Typed(TypedFilter { properties, .. }) => {
                    assert!(properties.iter().any(|prop| prop == &FilterProp::Foretold));
                    assert!(properties.iter().any(|prop| prop
                        == &FilterProp::Owned {
                            controller: ControllerRef::You,
                        }));
                    assert!(properties.iter().any(|prop| prop
                        == &FilterProp::InZone {
                            zone: crate::types::zones::Zone::Exile,
                        }));
                }
                other => panic!("expected Typed filter, got {other:?}"),
            },
            other => panic!("expected ObjectCount ref, got {other:?}"),
        }
    }

    #[test]
    fn for_each_mountain_and_red_card_in_it_counts_target_hand_union() {
        let result = parse_for_each_clause_expr("mountain and red card in it");
        let Some(QuantityExpr::Ref {
            qty:
                QuantityRef::ObjectCount {
                    filter: TargetFilter::Or { filters },
                },
        }) = result
        else {
            panic!("expected ObjectCount Or quantity, got {result:?}");
        };
        assert_eq!(filters.len(), 2);

        match &filters[0] {
            TargetFilter::Typed(TypedFilter {
                type_filters,
                properties,
                ..
            }) => {
                assert_eq!(
                    type_filters,
                    &vec![TypeFilter::Subtype("Mountain".to_string())]
                );
                assert!(properties
                    .iter()
                    .any(|prop| prop == &FilterProp::InZone { zone: Zone::Hand }));
                assert!(properties.iter().any(|prop| prop
                    == &FilterProp::Owned {
                        controller: ControllerRef::TargetPlayer,
                    }));
            }
            other => panic!("expected typed Mountain filter, got {other:?}"),
        }
        match &filters[1] {
            TargetFilter::Typed(TypedFilter {
                type_filters,
                properties,
                ..
            }) => {
                assert_eq!(type_filters, &vec![TypeFilter::Card]);
                assert!(properties.iter().any(|prop| prop
                    == &FilterProp::HasColor {
                        color: ManaColor::Red,
                    }));
                assert!(properties
                    .iter()
                    .any(|prop| prop == &FilterProp::InZone { zone: Zone::Hand }));
                assert!(properties.iter().any(|prop| prop
                    == &FilterProp::Owned {
                        controller: ControllerRef::TargetPlayer,
                    }));
            }
            other => panic!("expected typed red-card filter, got {other:?}"),
        }
    }

    #[test]
    fn for_each_forest_and_green_cards_in_it_accepts_plural_card() {
        assert!(matches!(
            parse_for_each_clause_expr("forest and green cards in it"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { .. },
            })
        ));
    }

    #[test]
    fn for_each_conjunction_with_unparseable_segment_returns_none() {
        // If either side fails to parse, the whole conjunction must fail —
        // no partial-credit Sum that would silently undercount.
        let result = parse_for_each_clause_expr("card in your hand and each blorgon you control");
        assert_eq!(result, None);
    }
}
