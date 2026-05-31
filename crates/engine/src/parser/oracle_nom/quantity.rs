//! Quantity expression combinators for Oracle text parsing.
//!
//! Parses quantity expressions from Oracle text: fixed numbers, dynamic references
//! like "the number of creatures you control", "its power", "your life total",
//! "equal to" phrases, and "for each" phrases.

use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until, take_while1};
use nom::combinator::{map, opt, value};
use nom::sequence::{pair, preceded, terminated};
use nom::Parser;

use super::context::ParseContext;
use super::error::OracleResult;
use super::primitives::{parse_article, parse_counter_type_typed, parse_number};
use super::target::parse_type_filter_word;
use crate::parser::oracle_target::{
    parse_shared_quality, parse_shared_quality_clause, parse_type_phrase,
};
use crate::parser::oracle_util::parse_subtype;
use crate::types::ability::{
    AggregateFunction, CardTypeSetSource, CastManaObjectScope, CastManaSpentMetric, ControllerRef,
    CountScope, DevotionColors, FilterProp, ObjectProperty, ObjectScope, PlayerScope, QuantityExpr,
    QuantityRef, RoundingMode, SharedQuality, TargetFilter, TypeFilter, TypedFilter, ZoneRef,
};
use crate::types::counter::CounterMatch;
use crate::types::player::PlayerCounterKind;
use crate::types::zones::Zone;

/// Parse a quantity expression: either a fractional expression, a dynamic reference,
/// or a fixed number. Fractional forms ("half X, rounded up/down") compose over the
/// same `parse_quantity_ref` / `parse_number` primitives used for plain quantities.
pub fn parse_quantity(input: &str) -> OracleResult<'_, QuantityExpr> {
    alt((
        parse_fraction_rounded,
        map(parse_quantity_ref, |qty| QuantityExpr::Ref { qty }),
        map(parse_number, |n| QuantityExpr::Fixed { value: n as i32 }),
    ))
    .parse(input)
}

/// CR 107.1a: Parse "half <inner>, rounded up/down" fractional expressions.
///
/// The inner expression is any quantity this module can recognize — either a
/// standard [`parse_quantity_ref`] (e.g. `"its power"`, `"your life total"`) or
/// a possessive reference resolved against the current target (e.g.
/// `"their library"` → `TargetZoneCardCount { zone: Library }`). The parser
/// accepts an optional `, rounded up` / `, rounded down` / `, round up` /
/// `, round down` suffix. If absent, the expression defaults to
/// [`RoundingMode::Down`] as a safe fallback — CR 107.1a requires Oracle text
/// to specify rounding explicitly, so an unspecified suffix indicates either
/// non-standard text or an upstream strip (duration, trailing punctuation).
///
/// Composes over existing refs only — does NOT introduce new QuantityRef
/// variants. New fractional patterns are unlocked by extending
/// [`parse_half_rounded_inner`], not by adding bespoke refs.
pub fn parse_fraction_rounded(input: &str) -> OracleResult<'_, QuantityExpr> {
    let (rest, divisor) = parse_fraction_divisor(input)?;
    let (rest, _) = opt(tag("of ")).parse(rest)?;
    let (rest, inner) = parse_half_rounded_inner(rest)?;
    let (rest, rounding) = parse_rounding_suffix(rest)?;
    Ok((
        rest,
        QuantityExpr::DivideRounded {
            inner: Box::new(inner),
            divisor,
            rounding,
        },
    ))
}

pub fn parse_half_rounded(input: &str) -> OracleResult<'_, QuantityExpr> {
    parse_fraction_rounded(input)
}

fn parse_fraction_divisor(input: &str) -> OracleResult<'_, u32> {
    alt((
        value(2, tag("half ")),
        value(3, alt((tag("a third "), tag("one third "), tag("third ")))),
        value(10, alt((tag("a tenth "), tag("one tenth "), tag("tenth ")))),
    ))
    .parse(input)
}

/// Inner expression of "half ...": a full quantity ref, a possessive ref
/// resolving against the current target ("their library"/"their life"), the
/// spell-cost variable X ("half X damage"), or a literal number ("half 10
/// damage" is vanishingly rare but parses cleanly).
///
/// Delegates to existing combinators — does NOT introduce new refs.
fn parse_half_rounded_inner(input: &str) -> OracleResult<'_, QuantityExpr> {
    alt((
        map(parse_possessive_quantity_ref, |qty| QuantityExpr::Ref {
            qty,
        }),
        // CR 107.1a: "half the cards in their hand" — explicit phrasing of
        // the possessive zone count that `parse_possessive_quantity_ref`
        // covers as "their hand". Tried before the generic `parse_quantity_ref`
        // so the "the cards in" prefix doesn't get consumed by a more
        // aggressive matcher.
        map(parse_cards_in_possessive_zone, |qty| QuantityExpr::Ref {
            qty,
        }),
        // CR 107.1a: "half the permanents they control" — possessive object
        // count phrasing reachable from fractional expressions (Pox Plague:
        // "sacrifices half the permanents they control"). Tried before the
        // generic `parse_quantity_ref` so `parse_the_number_of` doesn't
        // swallow the "the" without the expected "number of" connector.
        map(parse_possessive_objects_they_control, |qty| {
            QuantityExpr::Ref { qty }
        }),
        map(parse_quantity_ref, |qty| QuantityExpr::Ref { qty }),
        parse_quantity_expr_number,
    ))
    .parse(input)
}

/// Parse possessive-pronoun quantity phrases: "their library", "their hand",
/// "their life total", "their life", "his or her life", "its power",
/// "its toughness", "your hand", "your graveyard", "your library".
///
/// These are context-dependent — "their" refers to a player target in scope,
/// "its" refers to the effect's source/subject, "your" refers to the effect's
/// controller. The mapped `QuantityRef` variant carries that distinction:
///
/// | Possessive | Quantity | Maps to |
/// |------------|----------|---------|
/// | "their"    | library/hand/graveyard | `TargetZoneCardCount { zone }` |
/// | "their"    | life total / life      | `TargetLifeTotal` |
/// | "his or her" | life total / life    | `TargetLifeTotal` |
/// | "your"     | library/hand/graveyard | `ZoneCardCount` (Controller scope) |
/// | "your"     | life total / life      | `LifeTotal` |
/// | "its"      | power                  | `SelfPower` |
/// | "its"      | toughness              | `SelfToughness` |
///
/// CR 107.1a: These are the base references that half-rounded expressions
/// compose over. A new possessive quantity extends this combinator — do NOT
/// inline string matching for possessive patterns in effect parsers.
pub fn parse_possessive_quantity_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        parse_their_quantity_ref,
        parse_his_or_her_quantity_ref,
        parse_your_possessive_quantity_ref,
    ))
    .parse(input)
}

/// "their <zone>" / "their life [total]" — resolves against the effect's
/// player target (CR 115.7: targeting phrases reference the matched target).
fn parse_their_quantity_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    preceded(tag("their "), parse_their_tail).parse(input)
}

fn parse_their_tail(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(
            QuantityRef::TargetZoneCardCount {
                zone: ZoneRef::Library,
            },
            tag("library"),
        ),
        value(
            QuantityRef::TargetZoneCardCount {
                zone: ZoneRef::Hand,
            },
            tag("hand"),
        ),
        value(
            QuantityRef::TargetZoneCardCount {
                zone: ZoneRef::Graveyard,
            },
            tag("graveyard"),
        ),
        // Life total before bare "life" (longer tag first).
        value(
            QuantityRef::LifeTotal {
                player: PlayerScope::Target,
            },
            tag("life total"),
        ),
        value(
            QuantityRef::LifeTotal {
                player: PlayerScope::Target,
            },
            tag("life"),
        ),
    ))
    .parse(input)
}

/// Legacy "his or her <life>" possessive — present in older Oracle text that
/// has not been re-worded to "their". Resolves identically to `parse_their_*`.
fn parse_his_or_her_quantity_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    preceded(
        tag("his or her "),
        alt((
            value(
                QuantityRef::LifeTotal {
                    player: PlayerScope::Target,
                },
                tag("life total"),
            ),
            value(
                QuantityRef::LifeTotal {
                    player: PlayerScope::Target,
                },
                tag("life"),
            ),
        )),
    )
    .parse(input)
}

/// "your <zone>" / "your life [total]" — resolves against the controller of
/// the effect (CR 109.5). Note: `parse_quantity_ref` already handles
/// "your life total" and "cards in your <zone>", but not the shorthand
/// "your library" / "your hand" / "your life" forms that appear inside
/// fractional expressions ("half your hand, rounded up").
fn parse_your_possessive_quantity_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    preceded(tag("your "), parse_your_tail).parse(input)
}

fn parse_your_tail(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(
            QuantityRef::ZoneCardCount {
                zone: ZoneRef::Library,
                card_types: Vec::new(),
                scope: CountScope::Controller,
            },
            tag("library"),
        ),
        value(
            QuantityRef::ZoneCardCount {
                zone: ZoneRef::Hand,
                card_types: Vec::new(),
                scope: CountScope::Controller,
            },
            tag("hand"),
        ),
        value(
            QuantityRef::ZoneCardCount {
                zone: ZoneRef::Graveyard,
                card_types: Vec::new(),
                scope: CountScope::Controller,
            },
            tag("graveyard"),
        ),
        value(
            QuantityRef::LifeTotal {
                player: PlayerScope::Controller,
            },
            tag("life total"),
        ),
        value(
            QuantityRef::LifeTotal {
                player: PlayerScope::Controller,
            },
            tag("life"),
        ),
    ))
    .parse(input)
}

/// CR 107.1a + CR 109.5: "the cards in their <zone>" / "the cards in your <zone>"
/// — fractional-expression phrasing of the possessive zone count (Pox Plague:
/// "discards half the cards in their hand"). Mirrors the shorthand
/// `parse_possessive_quantity_ref` but recognizes the more explicit
/// `"the cards in X <zone>"` form that appears inside `"half ..."` subjects
/// where brevity wasn't chosen. Composes the shared possessive prefixes
/// (`"their "` for target scope, `"your "` for controller scope) with the
/// existing `parse_zone_ref_singular` so every supported zone is reachable
/// under this form without duplicating the zone-word list.
fn parse_cards_in_possessive_zone(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("the cards in ").parse(input)?;
    alt((
        map(preceded(tag("their "), parse_zone_ref_singular), |zone| {
            QuantityRef::TargetZoneCardCount { zone }
        }),
        map(preceded(tag("your "), parse_zone_ref_singular), |zone| {
            QuantityRef::ZoneCardCount {
                zone,
                card_types: Vec::new(),
                scope: CountScope::Controller,
            }
        }),
    ))
    .parse(rest)
}

/// CR 107.1a + CR 109.5: "the <type> they control" / "the <type> you control"
/// — possessive object-count phrasing (Pox Plague: "sacrifices half the
/// permanents they control"). Mirrors `parse_number_of_controlled_type` but
/// drops the "the number of" prefix required there, so the combinator is
/// reachable from fractional expressions ("half the X they control"). The
/// `"they"` arm uses `ControllerRef::ScopedPlayer` because `player_scope`
/// iteration binds the affected player separately from the printed ability
/// controller. `"you"` remains `ControllerRef::You`.
fn parse_possessive_objects_they_control(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("the ").parse(input)?;
    let (rest, (type_phrase, controller)) = alt((
        map(
            terminated(take_until(" they control"), tag(" they control")),
            |type_phrase| (type_phrase, ControllerRef::ScopedPlayer),
        ),
        map(
            terminated(take_until(" you control"), tag(" you control")),
            |type_phrase| (type_phrase, ControllerRef::You),
        ),
    ))
    .parse(rest)?;
    let (mut filter, type_rest) = parse_type_phrase(type_phrase);
    if !type_rest.trim().is_empty() || !quantity_filter_has_meaningful_content(&filter) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    attach_controller_to_quantity_filter(&mut filter, controller);
    Ok((rest, QuantityRef::ObjectCount { filter }))
}

fn attach_controller_to_quantity_filter(filter: &mut TargetFilter, controller: ControllerRef) {
    match filter {
        TargetFilter::Typed(TypedFilter {
            controller: slot, ..
        }) if slot.is_none() => {
            *slot = Some(controller);
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            for filter in filters {
                attach_controller_to_quantity_filter(filter, controller.clone());
            }
        }
        TargetFilter::Not { filter } => attach_controller_to_quantity_filter(filter, controller),
        _ => {}
    }
}

fn quantity_filter_has_meaningful_content(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => !tf.type_filters.is_empty() || !tf.properties.is_empty(),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(quantity_filter_has_meaningful_content)
        }
        TargetFilter::Not { filter } => quantity_filter_has_meaningful_content(filter),
        _ => false,
    }
}

/// Parse an optional ", rounded up/down" / ", round up/down" suffix.
///
/// CR 107.1a: Oracle text must specify rounding direction for fractional
/// expressions. When absent (malformed text or upstream trimming), defaults
/// to `Down` — the more common direction in actual Magic cards and a safe
/// fallback for misparses.
fn parse_rounding_suffix(input: &str) -> OracleResult<'_, RoundingMode> {
    let (rest, rounding) = opt(alt((
        value(RoundingMode::Up, tag(", rounded up")),
        value(RoundingMode::Down, tag(", rounded down")),
        value(RoundingMode::Up, tag(", round up")),
        value(RoundingMode::Down, tag(", round down")),
    )))
    .parse(input)?;
    Ok((rest, rounding.unwrap_or(RoundingMode::Down)))
}

/// Parse a literal number OR the variable `X` in filter-threshold contexts.
///
/// CR 107.3a + CR 601.2b: When a spell/ability has `{X}` in its cost, the caster
/// announces the value of X as part of casting. While the spell is on the stack,
/// any X in its text takes that announced value. This combinator emits the
/// `QuantityRef::Variable { name: "X" }` shape that is later resolved at effect
/// time against `ResolvedAbility::chosen_x` via `resolve_quantity_with_targets`.
///
/// Use this for filter-property thresholds ("with mana value X or less",
/// "with power X or greater", "with X counters on it", "search for up to X
/// cards"). Narrower than [`parse_quantity`] — does not recognize dynamic
/// references like "the number of creatures you control".
pub fn parse_quantity_expr_number(input: &str) -> OracleResult<'_, QuantityExpr> {
    alt((
        map(tag("x"), |_| QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        }),
        map(parse_number, |n| QuantityExpr::Fixed { value: n as i32 }),
    ))
    .parse(input)
}

/// Parse a dynamic quantity reference from Oracle text.
///
/// Matches phrases like "the number of creatures you control", "its power",
/// "your life total", "cards in your hand", etc.
pub fn parse_quantity_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        parse_object_count_by_shared_quality,
        parse_the_number_of,
        parse_distinct_card_types_exiled_with_source,
        parse_linked_exile_mana_value_ref,
        parse_distinct_card_types_in_zone,
        parse_distinct_card_types_among_objects,
        // CR 406.6: "cards exiled with ~" — must precede `parse_cards_in_zone_ref`
        // so "cards exiled with …" wins over the generic "cards in …" zone phrase.
        parse_cards_exiled_with_source,
        parse_life_total_ref,
        // CR 700.8: party-size phrasings — must precede `parse_speed_ref`
        // and zone counts so the leading "your " possessive routes to the
        // dedicated party combinator instead of a generic zone fallback.
        parse_party_size_ref,
        parse_speed_ref,
        parse_cards_in_zone_ref,
        parse_self_power_ref,
        parse_self_toughness_ref,
        parse_life_lost_ref,
        parse_life_gained_ref,
        parse_starting_life_ref,
        parse_object_mana_value_ref,
        // CR 608.2k + CR 400.7j + CR 202.3: previously-referenced object's
        // mana value — must precede `parse_event_context_refs` so the
        // cost/effect referent resolver wins over the generic event-source
        // resolver for sacrificed/exiled/milled possessives (Food Chain, Burnt
        // Offering, Metamorphosis, Heed the Mists).
        parse_cost_paid_object_ref,
        parse_event_context_refs,
    ))
    .or(alt((
        parse_target_power_ref,
        parse_target_life_ref,
        parse_basic_land_type_count,
        // Bare suffix form — reachable when a parent combinator has already
        // consumed "there are N " (see `parse_there_are_conditions`).
        parse_basic_land_types_among_lands_controlled_by_ref,
        parse_devotion_ref,
        parse_counters_among_ref,
    )))
    .parse(input)
}

/// CR 109.3: Parse "the greatest/fewest/total number of <type-phrase> that
/// have/share [a] <quality> in common" into a grouped object-count quantity.
///
/// The "in common" wrapper is not a target predicate: it asks for the size of
/// quality buckets within the already-matched population. Keep it separate from
/// `FilterProp::SharesQuality`, which validates a chosen group against a
/// reference object/set.
fn parse_object_count_by_shared_quality(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("the ").parse(input)?;
    let (rest, aggregate) = alt((
        value(AggregateFunction::Max, tag("greatest")),
        value(AggregateFunction::Min, tag("fewest")),
        value(AggregateFunction::Sum, tag("total")),
    ))
    .parse(rest)?;
    let (rest, _) = tag(" number of ").parse(rest)?;
    let (rest, type_text) = take_until(" that ").parse(rest)?;
    let (rest, _) = tag(" that ").parse(rest)?;
    let (rest, _) = alt((tag("have "), tag("has "), tag("share "), tag("shares "))).parse(rest)?;
    let (rest, _) = opt(alt((tag("a "), tag("at least one ")))).parse(rest)?;
    let (rest, quality) = parse_shared_quality(rest)?;
    let (rest, _) = tag(" in common").parse(rest)?;

    let (filter, type_remainder) = parse_type_phrase(type_text.trim());
    if !type_remainder.trim().is_empty()
        || matches!(filter, TargetFilter::Any)
        || !quantity_filter_has_meaningful_content(&filter)
    {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }

    Ok((
        rest,
        QuantityRef::ObjectCountBySharedQuality {
            filter,
            quality,
            aggregate,
        },
    ))
}

/// CR 607.2a: Parse linked-exile mana-value phrases into the shared aggregate
/// building block. `ControllerRef::You` is intentional here: player-scope
/// resolution rebinds the acting controller per owner, so the aggregate reads
/// "cards exiled with source owned by the iterating player" without a
/// Skyclave-specific quantity variant.
fn parse_linked_exile_mana_value_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((
        tag("the mana value of the exiled card"),
        tag("the converted mana cost of the exiled card"),
        tag("the exiled card's mana value"),
        tag("the exiled card's converted mana cost"),
    ))
    .parse(input)?;
    Ok((
        rest,
        QuantityRef::Aggregate {
            function: AggregateFunction::Sum,
            property: ObjectProperty::ManaValue,
            filter: TargetFilter::And {
                filters: vec![
                    TargetFilter::ExiledBySource,
                    TargetFilter::Typed(TypedFilter::default().properties(vec![
                        FilterProp::Owned {
                            controller: ControllerRef::You,
                        },
                    ])),
                ],
            },
        },
    ))
}

/// CR 122.1: Parse "counters among [filter]" — sum across every counter type.
///
/// Used for phrases like "thirty or more counters among artifacts and creatures
/// you control" (Lux Artillery's intervening-if). The counter type is `None`
/// because the Oracle text does not restrict to any particular counter kind;
/// the resolver sums counters of every type on every matching object.
///
/// Composes with `parse_there_are_conditions` to form the full
/// "there are N or more counters among [filter]" condition.
fn parse_counters_among_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("counters among ").parse(input)?;
    let type_text = rest.trim_end_matches('.').trim_end_matches(',');
    let (filter, remainder) = parse_type_phrase(type_text);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    // Map remainder back to original input slice — parse_type_phrase may have
    // consumed from a trimmed copy, so use pointer arithmetic for the correct
    // byte offset.
    let consumed = remainder.as_ptr() as usize - input.as_ptr() as usize;
    Ok((
        &input[consumed..],
        QuantityRef::CountersOnObjects {
            counter_type: None,
            filter,
        },
    ))
}

/// Parse "the number of [type] you control" → ObjectCount.
fn parse_the_number_of(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((tag("the total number of "), tag("the number of "))).parse(input)?;
    parse_number_of_inner(rest)
}

/// Parse the inner part after "the number of".
fn parse_number_of_inner(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        parse_distinct_card_types_exiled_with_source,
        parse_distinct_card_types_in_zone,
        parse_distinct_card_types_among_objects,
        // CR 201.2 + CR 603.4: "differently named <type-phrase>" — distinct-by-
        // name population count. Must precede `parse_number_of_controlled_type`
        // so the adjective prefix is consumed before the generic typed-filter
        // fallback. Class: Gimbal, Audience with Trostani, Awakened Amalgam,
        // Sandsteppe War Riders, All-Fates Scroll, Fungal Colossus, Euroakus,
        // Neriv, Emil, and other "differently named X" counters.
        parse_distinct_named_objects,
        // CR 122.1: "[kind] counters <possessor>" must be tried BEFORE the
        // generic type-filter arm so the typed player-counter ref wins over a
        // "[typeword] you control" misread (no `TypeFilter` for counter kinds).
        parse_player_counter_ref_tail,
        // CR 700.8: "creatures in your party" must precede the generic
        // "<type> you control" arm — the trailing "in your party" is what
        // distinguishes party-size from a controlled-creature count.
        parse_creatures_in_your_party_tail,
        parse_entered_this_turn_ref,
        parse_tokens_created_this_turn_tail,
        parse_number_of_distinct_colors_among_permanents_tail,
        // CR 107.1 + CR 700.1: "[type] controlled by the player who controls
        // the fewest/most" — must precede `parse_number_of_controlled_type`,
        // whose " you control" suffix would otherwise not match but whose
        // type-word prefix overlaps.
        parse_controlled_by_extremum_player,
        parse_number_of_controlled_type,
        parse_cards_exiled_with_source,
        // CR 109.4 + CR 115.7: "cards in their <zone>" / "cards in that player's <zone>"
        // must be tried BEFORE the scoped-zone combinator so the target-referring
        // possessive routes to `TargetZoneCardCount` (resolves against the player
        // target in scope) instead of falling back to a controller-less
        // `InZone` filter that counts every player's cards.
        parse_number_of_cards_in_target_zone,
        parse_number_of_cards_in_all_players_hands,
        parse_number_of_cards_in_zone,
        parse_number_of_opponents,
    ))
    .or(alt((
        parse_speed_ref,
        // CR 309.7: "the number of dungeons you've completed"
        value(
            QuantityRef::DungeonsCompleted,
            tag("dungeons you've completed"),
        ),
        // CR 202.2 + CR 601.2h: "the number of colors of mana spent to cast
        // <self>" / "the amount of mana spent to cast <self>" / "the amount of
        // mana from <source> spent to cast <self>". Delegates to the shared
        // `parse_mana_spent_to_cast_ref` combinator that backs the "for each"
        // path so all three metrics (DistinctColors, Total, FromSource) and
        // every self-subject anaphor (`it`, `this spell`, `this creature`,
        // `this permanent`, `them`, `~`) are covered. Class: Converge
        // (Painful Truths, Bring to Light, Radiant Flames), Sunburst, and
        // related "X is the number of colors of mana spent to cast this spell"
        // riders.
        parse_mana_spent_to_cast_ref,
        parse_number_of_object_name_words_tail,
        parse_number_of_object_colors_tail,
    )))
    .parse(input)
}

/// Parse "colors among [filter]" after "the number of".
fn parse_number_of_distinct_colors_among_permanents_tail(
    input: &str,
) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("colors among ").parse(input)?;
    let (remainder, filter) = super::target::parse_type_phrase(rest)?;
    if !matches!(remainder.trim(), "" | "." | ",")
        || !quantity_filter_has_meaningful_content(&filter)
    {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok(("", QuantityRef::DistinctColorsAmongPermanents { filter }))
}

/// CR 122.1: Parse the iteration source "kind of counter on/among <filter>" →
/// `QuantityRef::DistinctCounterKindsAmong { filter }`. Counter-side analogue of
/// `parse_number_of_distinct_colors_among_permanents_tail`. Used by Bribe
/// Taker's "for each kind of counter on permanents you control" — the filter is
/// any controlled-permanent type phrase, so the combinator covers the whole
/// class, not one card. Both "on" and "among" surface forms are accepted.
fn parse_for_each_distinct_counter_kinds_among(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("kind of counter ").parse(input)?;
    let (rest, _) = alt((tag("on "), tag("among "))).parse(rest)?;
    let (filter, remainder) = parse_type_phrase(rest);
    if !remainder.trim().is_empty() || matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok(("", QuantityRef::DistinctCounterKindsAmong { filter }))
}

/// CR 201.2 + CR 603.4: Parse "differently named <type-phrase>" after
/// "the number of" → `QuantityRef::ObjectCountDistinct { filter, qualities: [Name] }`.
///
/// Composes by delegating the inner type phrase to the shared
/// `oracle_target::parse_type_phrase` so any combination of supertype, color,
/// negation, type words, "tokens" property suffix, and controller suffix
/// ("you control", "an opponent controls", etc.) flows through one parser —
/// no per-card phrasing arms. The remainder must be empty (or only trailing
/// punctuation) and the filter must carry meaningful content; otherwise the
/// combinator fails so a downstream alt() arm can re-try.
///
/// Examples:
/// - "differently named artifact tokens you control" (Gimbal, Gremlin Prodigy;
///   Sandsteppe War Riders) → `Typed(Artifact, You, [Token])` deduped by Name
/// - "differently named lands you control" (Awakened Amalgam, All-Fates
///   Scroll, Fungal Colossus, Euroakus, Emil) → `Typed(Land, You)` deduped by Name
/// - "differently named creature tokens you control" (Audience with Trostani)
///   → `Typed(Creature, You, [Token])` deduped by Name
/// - "differently named tokens you control" (Neriv) → `Typed(Any, You, [Token])`
///   deduped by Name
fn parse_distinct_named_objects(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("differently named ").parse(input)?;
    let type_text = rest.trim_end_matches('.').trim_end_matches(',');
    let (filter, remainder) = parse_type_phrase(type_text);
    if !remainder.trim().is_empty() || !quantity_filter_has_meaningful_content(&filter) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let consumed = remainder.as_ptr() as usize - input.as_ptr() as usize;
    Ok((
        &input[consumed..],
        QuantityRef::ObjectCountDistinct {
            filter,
            qualities: vec![SharedQuality::Name],
        },
    ))
}

/// CR 107.1 + CR 700.1: Parse "[type-phrase] controlled by the player who
/// controls the fewest" (and "… the most") after "the number of" →
/// `QuantityRef::ControlledByEachPlayer { filter, aggregate }`.
///
/// Used by Balance / Restore Balance / Balancing Act for the equalization
/// minimum ("a number of lands they control equal to the number of lands
/// controlled by the player who controls the fewest"). Battlefield-scoped: the
/// hand-zone analogue is `HandSize { AllPlayers { Min } }`, parsed elsewhere.
fn parse_controlled_by_extremum_player(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, filter) = super::target::parse_type_phrase(input)?;
    if !quantity_filter_has_meaningful_content(&filter) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let (rest, aggregate) = preceded(
        tag(" controlled by the player who controls the "),
        alt((
            value(AggregateFunction::Min, tag("fewest")),
            value(AggregateFunction::Max, tag("most")),
        )),
    )
    .parse(rest)?;
    Ok((
        rest,
        QuantityRef::ControlledByEachPlayer { filter, aggregate },
    ))
}

/// Parse "[type(s)] you control" after "the number of".
fn parse_number_of_controlled_type(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, head) = parse_type_filter_word(input)?;
    let (rest, _) = tag(" you control").parse(rest)?;
    // CR 205.2b: "<head> you control that are <t1> and/or <t2>" restricts the
    // controlled population to objects that have any of the listed card types.
    // CR 205.2b makes a multi-type object satisfy any of its types, so a
    // permanent that is both a creature and a Vehicle is counted once via the
    // `AnyOf` disjunction (Collision Course). When the relative clause names a
    // single type, that type alone replaces the head. A non-type "that are"
    // clause (e.g. "that are tapped") leaves the suffix unconsumed so a later
    // arm can handle it rather than mis-parsing it here.
    let (rest, type_filters) =
        match opt(preceded(tag(" that are "), parse_type_filter_list)).parse(rest)? {
            (r, Some(list)) if list.len() > 1 => (r, vec![TypeFilter::AnyOf(list)]),
            (r, Some(list)) => (r, list),
            (r, None) => (r, vec![head]),
        };
    Ok((
        rest,
        QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters,
                controller: Some(ControllerRef::You),
                properties: Vec::new(),
            }),
        },
    ))
}

/// Parse "cards in your graveyard" / "creature cards in your graveyard" after "the number of".
fn parse_number_of_cards_in_zone(input: &str) -> OracleResult<'_, QuantityRef> {
    parse_zone_card_count(input)
}

/// CR 109.4 + CR 115.7: Parse "cards in their <zone>" / "cards in that player's <zone>"
/// into `QuantityRef::TargetZoneCardCount`. The possessive refers to the enclosing
/// effect's player target (e.g., Sword of War and Peace's "deals damage to that
/// player equal to the number of cards in their hand"), so the count must resolve
/// against the first `TargetRef::Player` in `ability.targets`, not against a
/// zone-wide `InZone` filter.
///
/// Mirrors `parse_their_tail` but is reachable after a leading `"cards in "`
/// prefix — the compound form used by "the number of cards in ..." expressions.
fn parse_number_of_cards_in_target_zone(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("cards in ").parse(input)?;
    let (rest, _) = alt((tag("their "), tag("that player's "))).parse(rest)?;
    map(parse_zone_ref_singular, |zone| {
        QuantityRef::TargetZoneCardCount { zone }
    })
    .parse(rest)
}

fn parse_number_of_cards_in_all_players_hands(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((tag("cards"), tag("card"))).parse(input)?;
    let (rest, _) = tag(" in all players' hand").parse(rest)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    Ok((
        rest,
        QuantityRef::HandSize {
            player: PlayerScope::AllPlayers {
                aggregate: AggregateFunction::Sum,
                exclude: None,
            },
        },
    ))
}

/// CR 115.1 + CR 115.7: Parse "target opponent's <zone>" / "target player's <zone>"
/// possessive into a `TargetZoneCardCount`. Used as a target-bound branch of
/// `parse_zone_card_count` for "card in target opponent's hand" expressions
/// (Jeska's Will mode 1). Does not consume the leading "card in " — the caller
/// has already stripped that prefix and is positioned at the possessive.
fn parse_target_player_possessive_zone(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((tag("target opponent's "), tag("target player's "))).parse(input)?;
    let (rest, zone) = parse_zone_ref_singular(rest)?;
    Ok((rest, QuantityRef::TargetZoneCardCount { zone }))
}

/// CR 303.4m + CR 613.4c: Parse recipient-relative hand counts such as
/// "card in its controller's hand". In layer-evaluated Aura/Equipment statics,
/// "its" refers to the affected object ("enchanted creature"), not the Aura
/// source controller.
fn parse_recipient_controller_hand_count(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((
        tag("its controller's "),
        tag("their controller's "),
        tag("enchanted creature's controller's "),
        tag("equipped creature's controller's "),
        tag("that creature's controller's "),
        tag("that permanent's controller's "),
    ))
    .parse(input)?;
    let (rest, _) = tag("hand").parse(rest)?;
    Ok((
        rest,
        QuantityRef::HandSize {
            player: PlayerScope::RecipientController,
        },
    ))
}

fn parse_zone_card_count(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, card_types) = if let Ok((typed_rest, typed_filters)) = parse_type_filter_list(input)
    {
        if let Ok((rest, _)) = parse_card_word(typed_rest) {
            (rest, typed_filters)
        } else {
            let (rest, _) = parse_card_word(input)?;
            (rest, Vec::new())
        }
    } else {
        let (rest, _) = parse_card_word(input)?;
        (rest, Vec::new())
    };
    let (rest, _) = tag(" in ").parse(rest)?;
    // CR 115.1 + CR 115.7: "card in target opponent's <zone>" / "card in target
    // player's <zone>" — possessive references the spell's player target. Only
    // applies when no card-type filters were captured (target-bound counts are
    // type-agnostic over the targeted zone). Resolves dynamically via
    // `ability.targets`. Tried before `parse_scoped_zone_ref`, which has no
    // `target opponent's` arm and would otherwise fall through to the bare
    // singular zone (`CountScope::All`) and silently misroute the count.
    if card_types.is_empty() || card_types == vec![TypeFilter::Card] {
        if let Ok((after_zone, q)) = parse_recipient_controller_hand_count(rest) {
            return Ok((after_zone, q));
        }
    }
    if card_types.is_empty() {
        if let Ok((after_zone, q)) = parse_target_player_possessive_zone(rest) {
            return Ok((after_zone, q));
        }
    }
    let (rest, (zone, scope)) = parse_scoped_zone_ref(rest)?;
    Ok((
        rest,
        QuantityRef::ZoneCardCount {
            zone,
            card_types,
            scope,
        },
    ))
}

fn parse_cards_in_zone_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    parse_zone_card_count(input)
}

fn parse_distinct_card_types_in_zone(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("card type").parse(input)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    let (rest, _) = tag(" among cards in ").parse(rest)?;
    let (rest, (zone, scope)) = parse_scoped_zone_ref(rest)?;
    Ok((
        rest,
        QuantityRef::DistinctCardTypes {
            source: CardTypeSetSource::Zone { zone, scope },
        },
    ))
}

fn parse_distinct_card_types_exiled_with_source(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("card type").parse(input)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    let (rest, _) = tag(" among cards exiled with ").parse(rest)?;
    let (rest, _) = alt((
        tag("~"),
        tag("it"),
        preceded(
            tag("this "),
            take_while1(|c: char| c.is_ascii_alphabetic() || c == '-'),
        ),
    ))
    .parse(rest)?;
    Ok((
        rest,
        QuantityRef::DistinctCardTypes {
            source: CardTypeSetSource::ExiledBySource,
        },
    ))
}

fn parse_distinct_card_types_among_objects(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("card type").parse(input)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    let (rest, _) = tag(" among ").parse(rest)?;
    let type_text = rest.trim_end_matches('.').trim_end_matches(',');
    let (filter, remainder) = parse_type_phrase(type_text);
    if matches!(filter, TargetFilter::Any) || !remainder.trim().is_empty() {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let consumed = remainder.as_ptr() as usize - input.as_ptr() as usize;
    Ok((
        &input[consumed..],
        QuantityRef::DistinctCardTypes {
            source: CardTypeSetSource::Objects { filter },
        },
    ))
}

/// CR 406.6 + CR 607.1: Parse bare "cards exiled with ~" (or "cards exiled with this X")
/// → `QuantityRef::CardsExiledBySource`.
///
/// Reached after a parent combinator (typically `parse_there_are_conditions` after
/// "there are N [or more] ") has consumed the leading quantity. Composes with
/// `StaticCondition::QuantityComparison` to express thresholds over the source's
/// linked-exile pile (Veteran Survivor: "three or more cards exiled with ~").
fn parse_cards_exiled_with_source(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("cards exiled with ").parse(input)?;
    let (rest, _) = alt((
        tag("~"),
        tag("it"),
        preceded(
            tag("this "),
            take_while1(|c: char| c.is_ascii_alphabetic() || c == '-'),
        ),
    ))
    .parse(rest)?;
    Ok((rest, QuantityRef::CardsExiledBySource))
}

/// Parse "opponents" / "opponents you have" after "the number of".
fn parse_number_of_opponents(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("opponents").parse(input)?;
    Ok((
        rest,
        QuantityRef::PlayerCount {
            filter: crate::types::ability::PlayerFilter::Opponent,
        },
    ))
}

/// CR 119.3 + CR 700.1: Parse a "for each" opponent clause qualified by a
/// life-change predicate — "(of your) opponents who lost/gained life this
/// turn". Reached by the for-each clause path (Belbe, Corrupted Observer:
/// "{C}{C} for each of your opponents who lost life this turn"). The leading
/// "of your "/"of " is optional. Each qualifier is one `alt()` arm — no
/// permutation enumeration.
fn parse_for_each_opponents_life_change(input: &str) -> OracleResult<'_, QuantityRef> {
    use crate::types::ability::PlayerFilter;
    let (rest, _) = opt(alt((tag("of your "), tag("of ")))).parse(input)?;
    let (rest, _) = tag("opponents ").parse(rest)?;
    let (rest, filter) = alt((
        value(
            PlayerFilter::OpponentLostLife,
            tag("who lost life this turn"),
        ),
        value(
            PlayerFilter::OpponentGainedLife,
            tag("who gained life this turn"),
        ),
    ))
    .parse(rest)?;
    Ok((rest, QuantityRef::PlayerCount { filter }))
}

/// Parse "your life total".
fn parse_life_total_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    value(
        QuantityRef::LifeTotal {
            player: PlayerScope::Controller,
        },
        tag("your life total"),
    )
    .parse(input)
}

/// CR 700.8: Standalone "your party" phrasings → `QuantityRef::PartySize`.
///
/// Covers the surface forms used by ZNR Party cards as full-quantity
/// expressions (not the post-"for each" form, which is handled by
/// [`parse_creature_in_party_for_each`]):
/// - `"your party's size"` (Cleric of Life's Bond, Coveted Prize, Tazri…)
/// - `"the size of your party"` (rarer rewording)
///
/// Composes a single `tag` per phrasing under one `alt` — no permutation
/// enumeration. The possessive axis is intentionally limited to `your` here:
/// no printed card today reads "an opponent's party's size", so the
/// `PlayerScope::Opponent { .. }` branch is unlocked at the type layer
/// without needing dedicated parser surface.
fn parse_party_size_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    value(
        QuantityRef::PartySize {
            player: PlayerScope::Controller,
        },
        alt((tag("your party's size"), tag("the size of your party"))),
    )
    .parse(input)
}

/// CR 700.8: Inner form reached after `parse_the_number_of` has consumed
/// `"the number of "` — recognizes `"creatures in your party"` and the
/// (rare) singular `"creature in your party"`. Returns `PartySize`
/// (`PlayerScope::Controller`); see [`parse_party_size_ref`] for the
/// possessive-axis discussion.
fn parse_creatures_in_your_party_tail(input: &str) -> OracleResult<'_, QuantityRef> {
    value(
        QuantityRef::PartySize {
            player: PlayerScope::Controller,
        },
        alt((
            tag("creatures in your party"),
            tag("creature in your party"),
        )),
    )
    .parse(input)
}

/// CR 700.8: Reached after `for each ` has been consumed. Recognizes
/// `"creature in your party"` (singular per Oracle templating) and returns
/// the party-size ref so "for each creature in your party" composes to the
/// same scaling expression as "equal to your party's size".
fn parse_creature_in_party_for_each(input: &str) -> OracleResult<'_, QuantityRef> {
    value(
        QuantityRef::PartySize {
            player: PlayerScope::Controller,
        },
        alt((
            tag("creature in your party"),
            tag("creatures in your party"),
        )),
    )
    .parse(input)
}

pub(crate) fn parse_card_word(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((tag(" cards"), tag(" card"), tag("cards"), tag("card"))),
    )
    .parse(input)
}

/// Parse a list of type filters joined by `" and "`, `" or "`, or `" and/or "`.
///
/// CR 604.3: In zone-count contexts ("two or more instant and/or sorcery cards
/// in your graveyard"), the joining conjunction is semantically a disjunction
/// — a card matches if it has any of the listed types. The result
/// `Vec<TypeFilter>` is consumed by `matches_zone_card_filter`
/// (`game/quantity.rs:1151`), which uses `.iter().any(...)` (logical OR).
///
/// All three separators (`and`, `or`, `and/or`) are accepted so the combinator
/// covers the grammatical variants Wizards uses across templating eras
/// (e.g. "instant and/or sorcery", "instant or sorcery", "creatures and
/// artifacts"). The longest-prefix-first ordering (`and/or` before `and`) is
/// load-bearing — without it, `tag(" and ")` would consume the `" and "` head
/// of `" and/or "` and the `/or` tail would derail `parse_type_filter_word`.
pub(crate) fn parse_type_filter_list(input: &str) -> OracleResult<'_, Vec<TypeFilter>> {
    let (mut rest, first) = parse_type_filter_word(input)?;
    let mut filters = vec![first];
    loop {
        let sep = tag::<_, _, OracleError<'_>>(" and/or ")
            .parse(rest)
            .or_else(|_| tag::<_, _, OracleError<'_>>(" and ").parse(rest))
            .or_else(|_| tag::<_, _, OracleError<'_>>(" or ").parse(rest));
        let Ok((next_rest, _)) = sep else { break };
        let Ok((after_type, next)) = parse_type_filter_word(next_rest) else {
            break;
        };
        filters.push(next);
        rest = after_type;
    }
    Ok((rest, filters))
}

pub(crate) fn parse_zone_ref_singular(input: &str) -> OracleResult<'_, ZoneRef> {
    alt((
        value(ZoneRef::Graveyard, tag("graveyard")),
        value(ZoneRef::Exile, tag("exile")),
        value(ZoneRef::Library, tag("library")),
        value(ZoneRef::Hand, tag("hand")),
    ))
    .parse(input)
}

fn parse_zone_ref_plural(input: &str) -> OracleResult<'_, ZoneRef> {
    alt((
        value(ZoneRef::Graveyard, tag("graveyards")),
        value(ZoneRef::Exile, tag("exiles")),
        value(ZoneRef::Library, tag("libraries")),
        value(ZoneRef::Hand, tag("hands")),
    ))
    .parse(input)
}

fn parse_scoped_zone_ref(input: &str) -> OracleResult<'_, (ZoneRef, CountScope)> {
    alt((
        map(preceded(tag("your "), parse_zone_ref_singular), |zone| {
            (zone, CountScope::Controller)
        }),
        map(
            preceded(
                alt((tag("your opponents' "), tag("opponents' "))),
                parse_zone_ref_plural,
            ),
            |zone| (zone, CountScope::Opponents),
        ),
        map(preceded(tag("all "), parse_zone_ref_plural), |zone| {
            (zone, CountScope::All)
        }),
        map(parse_zone_ref_singular, |zone| (zone, CountScope::All)),
    ))
    .parse(input)
}

/// Parse "its power" / "~'s power" / "this creature's power" / "this card's power".
///
/// CR 400.7 + CR 208.3: Scavenge and other graveyard-activated effects reference
/// the source via "this card's power" because the source is a card (not a
/// creature) when the ability is activated. `SelfPower` is LKI-aware at
/// resolution time (see `game/quantity.rs`), so all four phrasings resolve
/// identically.
fn parse_self_power_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(
            QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Source,
            },
            tag("its power"),
        ),
        value(
            QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Source,
            },
            tag("~'s power"),
        ),
        value(
            QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Source,
            },
            tag("this creature's power"),
        ),
        value(
            QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Source,
            },
            tag("this card's power"),
        ),
    ))
    .parse(input)
}

/// Parse "its toughness" / "~'s toughness" / "this creature's toughness" /
/// "this card's toughness". See `parse_self_power_ref` for the card-vs-creature
/// rationale.
fn parse_self_toughness_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(
            QuantityRef::Toughness {
                scope: crate::types::ability::ObjectScope::Source,
            },
            tag("its toughness"),
        ),
        value(
            QuantityRef::Toughness {
                scope: crate::types::ability::ObjectScope::Source,
            },
            tag("~'s toughness"),
        ),
        value(
            QuantityRef::Toughness {
                scope: crate::types::ability::ObjectScope::Source,
            },
            tag("this creature's toughness"),
        ),
        value(
            QuantityRef::Toughness {
                scope: crate::types::ability::ObjectScope::Source,
            },
            tag("this card's toughness"),
        ),
    ))
    .parse(input)
}

/// Parse life-lost references: "the life you've lost this turn", "life you've lost", etc.
/// Includes duration-stripped forms (without "this turn") for post-duration-stripping contexts.
/// Accepts an optional "(the) amount of " prefix so phrases like
/// "the amount of life you lost this turn" (Hope Estheim class) parse uniformly.
fn parse_life_lost_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    // CR 119.3: Optional "the amount of " / "amount of " prefix before the base
    // life-lost phrase. Shared combinator absorbs the prefix once so every
    // downstream variant automatically supports it.
    let (input, _) =
        nom::combinator::opt(alt((tag("the amount of "), tag("amount of ")))).parse(input)?;
    alt((
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Sum,
                },
            },
            tag("the total amount of life your opponents have lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Sum,
                },
            },
            tag("total amount of life your opponents have lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Sum,
                },
            },
            tag("the total life lost by your opponents this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Sum,
                },
            },
            tag("total life lost by your opponents this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            },
            tag("total life you lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            },
            tag("total life you've lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            },
            tag("the life you've lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            },
            tag("the life you lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            },
            tag("life you've lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            },
            tag("life you lost this turn"),
        ),
        // Duration-stripped forms (after strip_trailing_duration removes "this turn")
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            },
            tag("the life you've lost"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            },
            tag("the life you lost"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            },
            tag("life you've lost"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            },
            tag("life you lost"),
        ),
        // CR 119.3 + CR 608.2k: Third-person variants resolve against the
        // per-target player during effect iteration. The runtime's
        // `LifeLostThisTurn` reads `player.life_lost_this_turn` where
        // `player` is the resolution context's bound player — for
        // `LoseLife { target: EachOpponent }` this rebinds per-opponent,
        // so "they lost" / "that player lost" resolve correctly without
        // a new typed variant. Archfiend of Despair, Wound Reflection,
        // Astarion (Feed mode), Blitzwing, Warlock Class.
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            },
            tag("the life that player lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            },
            tag("the life they lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            },
            tag("the amount of life they lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            },
            tag("the life that player lost"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            },
            tag("the life they lost"),
        ),
    ))
    .parse(input)
}

/// Parse life-gained references: "the life you've gained this turn", "life you've gained", etc.
/// Includes duration-stripped forms (without "this turn") for post-duration-stripping contexts.
/// Accepts an optional "(the) amount of " prefix so phrases like
/// "the amount of life you gained this turn" (Hope Estheim class) parse uniformly.
fn parse_life_gained_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    // CR 119.3: Optional "the amount of " / "amount of " prefix; see parse_life_lost_ref.
    let (input, _) =
        nom::combinator::opt(alt((tag("the amount of "), tag("amount of ")))).parse(input)?;
    alt((
        value(
            QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::Controller,
            },
            tag("total life you gained this turn"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::Controller,
            },
            tag("total life you've gained this turn"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::Controller,
            },
            tag("the life you've gained this turn"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::Controller,
            },
            tag("the life you gained this turn"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::Controller,
            },
            tag("life you've gained this turn"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::Controller,
            },
            tag("life you gained this turn"),
        ),
        // Duration-stripped forms
        value(
            QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::Controller,
            },
            tag("the life you've gained"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::Controller,
            },
            tag("the life you gained"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::Controller,
            },
            tag("life you've gained"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::Controller,
            },
            tag("life you gained"),
        ),
    ))
    .parse(input)
}

/// CR 103.4: Parse "your/their starting life total". Format-global constant —
/// "their" is grammatically anaphoric to "a player" but resolves identically.
fn parse_starting_life_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    value(
        QuantityRef::StartingLifeTotal,
        alt((
            tag::<_, _, OracleError<'_>>("your starting life total"),
            tag("their starting life total"),
        )),
    )
    .parse(input)
}

/// CR 202.3: Object mana value references in continuous effects.
///
/// Composes the existing object-scope possessive grammar with the mana-value
/// property, so per-recipient animation effects ("its mana value") and target
/// references ("that creature's mana value") lower through the same
/// `QuantityRef::ObjectManaValue` building block.
fn parse_object_mana_value_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, scope) = parse_object_possessive_scope(input)?;
    let (rest, _) = alt((tag(" mana value"), tag(" converted mana cost"))).parse(rest)?;
    Ok((rest, QuantityRef::ObjectManaValue { scope }))
}

/// CR 608.2k + CR 400.7j + CR 202.3: Previously-referenced object's mana value.
///
/// Composes the prefix grammar
/// `[the] (sacrificed|exiled|discarded|milled) (creature|card|permanent|artifact|enchantment|planeswalker|land)'s (mana value|converted mana cost|power|toughness)`
/// into a single typed combinator. Each axis is a single `alt()` over
/// independent variants — adding a new participle, a new noun, or the British
/// spelling of "mana value" extends one alt branch rather than adding a new
/// top-level arm.
///
/// Used by Food Chain ("1 plus the exiled creature's mana value"),
/// Burnt Offering / Metamorphosis ("the sacrificed creature's mana value"),
/// Heed the Mists ("the milled card's mana value"),
/// and the broader cost-paid-by-property class.
///
/// CR 701.17a + CR 701.17c + CR 400.7j: "milled" card refers to the
/// object that moved from the library to the graveyard; its mana value is read
/// from that public-zone object or LKI as needed.
fn parse_cost_paid_object_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = opt(tag("the ")).parse(input)?;
    let (rest, _) = alt((
        tag("sacrificed "),
        tag("exiled "),
        tag("discarded "),
        // CR 701.17a: "milled" — card moved library → graveyard by the mill action.
        tag("milled "),
    ))
    .parse(rest)?;
    let (rest, _) = alt((
        tag("creature"),
        tag("card"),
        tag("permanent"),
        tag("artifact"),
        tag("enchantment"),
        tag("planeswalker"),
        tag("land"),
    ))
    .parse(rest)?;
    let (rest, property) = parse_object_property_possessive_suffix(rest)?;
    let qty = match property {
        ObjectProperty::Power => QuantityRef::Power {
            scope: ObjectScope::CostPaidObject,
        },
        ObjectProperty::Toughness => QuantityRef::Toughness {
            scope: ObjectScope::CostPaidObject,
        },
        ObjectProperty::ManaValue => QuantityRef::ObjectManaValue {
            scope: ObjectScope::CostPaidObject,
        },
    };
    Ok((rest, qty))
}

fn parse_object_property_possessive_suffix(input: &str) -> OracleResult<'_, ObjectProperty> {
    alt((
        value(ObjectProperty::ManaValue, tag("'s mana value")),
        value(ObjectProperty::ManaValue, tag("'s converted mana cost")),
        value(ObjectProperty::Power, tag("'s power")),
        value(ObjectProperty::Toughness, tag("'s toughness")),
    ))
    .parse(input)
}

fn parse_anaphoric_target_card_property_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("that ").parse(input)?;
    let (rest, has_power_toughness) = alt((
        value(true, tag("creature card")),
        value(false, tag("artifact card")),
        value(false, tag("enchantment card")),
        value(false, tag("planeswalker card")),
        value(false, tag("land card")),
        value(false, tag("card")),
    ))
    .parse(rest)?;
    let (rest, property) = parse_object_property_possessive_suffix(rest)?;
    let qty = match property {
        ObjectProperty::Power if has_power_toughness => QuantityRef::Power {
            scope: ObjectScope::Target,
        },
        ObjectProperty::Toughness if has_power_toughness => QuantityRef::Toughness {
            scope: ObjectScope::Target,
        },
        ObjectProperty::ManaValue => QuantityRef::ObjectManaValue {
            scope: ObjectScope::Target,
        },
        ObjectProperty::Power | ObjectProperty::Toughness => {
            return Err(nom::Err::Error(OracleError::new(
                input,
                nom::error::ErrorKind::Tag,
            )));
        }
    };
    Ok((rest, qty))
}

/// Parse event-context quantity references.
///
/// CR 603.7c: "that {noun}" in a triggered ability refers to the object or
/// value from the triggering event. The source-object variants resolve via
/// `extract_source_from_event` → live object or LKI cache.
fn parse_event_context_refs(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(QuantityRef::EventContextAmount, tag("that much")),
        value(QuantityRef::EventContextAmount, tag("that many")),
        value(QuantityRef::EventContextAmount, tag("that damage")),
        // CR 120.1 + CR 603.7c: "the damage dealt" bare form in a triggered
        // ability body — refers to the total from the triggering combat-damage
        // event. Distinct from "that damage" (different article+verb) and
        // "damage dealt this way" (PreviousEffectAmount).
        value(QuantityRef::EventContextAmount, tag("the damage dealt")),
        value(
            QuantityRef::Power {
                scope: ObjectScope::CostPaidObject,
            },
            tag("that creature's power"),
        ),
        value(
            QuantityRef::Toughness {
                scope: ObjectScope::CostPaidObject,
            },
            tag("that creature's toughness"),
        ),
        // "Whenever you cast an enchantment spell, ... equal to that spell's
        // mana value" (Dusty Parlor) — the SpellCast event's source object is
        // the spell itself, so CMC reads cleanly off it.
        value(
            QuantityRef::ObjectManaValue {
                scope: ObjectScope::CostPaidObject,
            },
            tag("that spell's mana value"),
        ),
        // CR 109.2a + CR 608.2c: "that [type] card's [property]" — anaphoric
        // reference to a card selected by an earlier instruction in the same
        // resolution sequence.
        parse_anaphoric_target_card_property_ref,
    ))
    .parse(input)
}

/// Parse target-creature power refs:
///   - Saxon-genitive: "target creature's power" / "the target creature's power"
///   - Of-form: "the power of target creature [you control|an opponent controls]?"
///
/// All variants resolve to the same `QuantityRef::Power { scope: crate::types::ability::ObjectScope::Target }`. CR 107.1.
/// Longest-first ordering: the controller-qualified of-form variants must come
/// before the bare of-form so `alt`'s short-circuit doesn't strand the
/// "you control" / "an opponent controls" suffix as un-consumed remainder
/// (which would cause `parse_quantity_ref`'s `rest.is_empty()` check to fail).
/// Soul's Majesty, Predator's Rapport, and similar.
fn parse_target_power_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(
            QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Target,
            },
            tag("target creature's power"),
        ),
        value(
            QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Target,
            },
            tag("the target creature's power"),
        ),
        value(
            QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Target,
            },
            tag("the power of target creature you control"),
        ),
        value(
            QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Target,
            },
            tag("the power of target creature an opponent controls"),
        ),
        value(
            QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Target,
            },
            tag("the power of target creature"),
        ),
    ))
    .parse(input)
}

/// Parse "target player's life total" / "that player's life total".
fn parse_target_life_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(
            QuantityRef::LifeTotal {
                player: PlayerScope::Target,
            },
            tag("target player's life total"),
        ),
        value(
            QuantityRef::LifeTotal {
                player: PlayerScope::Target,
            },
            tag("that player's life total"),
        ),
    ))
    .parse(input)
}

/// Parse the bare domain suffix: "basic land type[s] among lands <controller> controls".
///
/// Factored out so both the full "the number of ..." form (Domain quantity) and
/// the "there are N ..." condition form (see `parse_there_are_conditions` in
/// `oracle_nom/condition.rs`) share a single tag authority. The singular form
/// appears after "for each"; the plural form appears after "the number of".
fn parse_basic_land_types_among_lands_controlled_by_ref(
    input: &str,
) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("basic land type").parse(input)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    let (rest, _) = tag(" among lands ").parse(rest)?;
    let (rest, controller) = alt((
        value(ControllerRef::You, tag("you control")),
        value(ControllerRef::TargetPlayer, tag("they control")),
    ))
    .parse(rest)?;
    Ok((rest, QuantityRef::BasicLandTypeCount { controller }))
}

/// Parse "the number of basic land types among lands you control" (Domain).
fn parse_basic_land_type_count(input: &str) -> OracleResult<'_, QuantityRef> {
    preceded(
        tag("the number of "),
        parse_basic_land_types_among_lands_controlled_by_ref,
    )
    .parse(input)
}

/// Parse devotion references.
fn parse_devotion_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("your devotion to ").parse(input)?;
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that color").parse(rest) {
        return Ok((
            rest,
            QuantityRef::Devotion {
                colors: DevotionColors::ChosenColor,
            },
        ));
    }
    let (rest, color) = super::primitives::parse_color(rest)?;
    // Check for " and [color]" for multi-color devotion
    if let Ok((rest2, _)) = tag::<_, _, OracleError<'_>>(" and ").parse(rest) {
        if let Ok((rest3, color2)) = super::primitives::parse_color(rest2) {
            return Ok((
                rest3,
                QuantityRef::Devotion {
                    colors: DevotionColors::Fixed(vec![color, color2]),
                },
            ));
        }
    }
    Ok((
        rest,
        QuantityRef::Devotion {
            colors: DevotionColors::Fixed(vec![color]),
        },
    ))
}

/// Parse "equal to [quantity]" from Oracle text.
///
/// Returns the quantity expression following "equal to ".
pub fn parse_equal_to(input: &str) -> OracleResult<'_, QuantityExpr> {
    let (rest, _) = tag("equal to ").parse(input)?;
    parse_quantity(rest)
}

/// Parse "for each [type] you control" from Oracle text.
///
/// Returns a QuantityRef::ObjectCount with the matched filter.
pub fn parse_for_each(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("for each ").parse(input)?;
    parse_for_each_clause_ref(rest)
}

/// Parse the inner content after "for each ".
pub fn parse_for_each_clause_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    parse_for_each_clause_ref_with_they_controller(input, ControllerRef::ScopedPlayer)
}

pub(crate) fn parse_for_each_clause_ref_with_context<'a>(
    input: &'a str,
    ctx: &ParseContext,
) -> OracleResult<'a, QuantityRef> {
    let they_controller = ctx
        .third_person_player_controller_ref()
        .unwrap_or(ControllerRef::ScopedPlayer);
    parse_for_each_clause_ref_with_they_controller(input, they_controller)
}

fn parse_for_each_clause_ref_with_they_controller(
    input: &str,
    they_controller: ControllerRef,
) -> OracleResult<'_, QuantityRef> {
    alt((
        parse_for_each_opponents_life_change,
        parse_counter_added_this_turn_for_each,
        parse_object_colors_for_each,
        parse_object_name_word_count_for_each,
        parse_mana_symbols_in_object_mana_cost_for_each,
        parse_distinct_card_types_in_zone,
        parse_foretold_cards_owned_in_exile,
        parse_zone_card_count,
        parse_for_each_attached_to_source,
        // CR 700.8: "creature in your party" must precede the generic
        // "<type> you control" arm — same reason as in
        // `parse_number_of_inner`.
        parse_creature_in_party_for_each,
        parse_player_counter_ref_tail,
        // CR 700.4: "creature that died this turn" / "creature that
        // died under your control this turn" — event-based count of dies-events
        // tracked in `state.zone_changes_this_turn`. Must precede
        // `parse_for_each_controlled_type` since the leading "creature" token
        // would otherwise commit the simple `<type> you control` arm.
        parse_for_each_subtype_died_this_turn,
        parse_for_each_creature_died_this_turn,
        parse_entered_this_turn_ref,
    ))
    .or(alt((
        |input| parse_for_each_combat_creature_controlled(input, they_controller.clone()),
        parse_for_each_combat_creature_other_than_source,
        parse_for_each_attacking_controller_type,
        parse_for_each_blocking_source_type,
        parse_for_each_recipient_shared_quality,
        parse_for_each_battlefield_type,
        parse_for_each_commander_cast_count,
        parse_mana_spent_to_cast_ref,
        // CR 122.1: "kind of counter on/among <filter>" (Bribe Taker). Placed
        // before the generic `<type> you control` arm so the leading "kind"
        // token does not commit to it.
        parse_for_each_distinct_counter_kinds_among,
        parse_for_each_controlled_type,
    )))
    .parse(input)
}

/// CR 400.7: Parse "[type] that entered (the battlefield) this turn" into
/// the shared entered-this-turn battlefield count. The "under your control"
/// surface form stamps `ControllerRef::You` onto the typed filter; phrases
/// that already include "you control" keep the controller supplied by
/// `parse_type_phrase`.
fn parse_entered_this_turn_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, (type_text, inject_you)) = parse_entered_this_turn_clause(input)?;
    let (filter, remainder) = parse_type_phrase(type_text.trim());
    if matches!(filter, TargetFilter::Any) || !remainder.trim().is_empty() {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let filter = if inject_you {
        inject_controller(filter, ControllerRef::You)
    } else {
        filter
    };
    Ok((rest, QuantityRef::EnteredThisTurn { filter }))
}

fn parse_entered_this_turn_clause(input: &str) -> OracleResult<'_, (&str, bool)> {
    map(
        pair(
            take_until(" that entered"),
            preceded(
                tag(" that entered"),
                alt((
                    value(true, tag(" the battlefield under your control this turn")),
                    value(false, tag(" the battlefield this turn")),
                    value(false, tag(" this turn")),
                )),
            ),
        ),
        |(type_text, inject_you)| (type_text, inject_you),
    )
    .parse(input)
}

/// CR 111.2: Parse "[type] tokens you created this turn" into the shared
/// token-creation count. The player scope carries "you"; the filter carries
/// token characteristics such as Treasure/Food/creature.
fn parse_tokens_created_this_turn_tail(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, type_text) = take_until(" you created this turn").parse(input)?;
    let (rest, _) = tag(" you created this turn").parse(rest)?;
    let (filter, remainder) = parse_type_phrase(type_text.trim());
    if matches!(filter, TargetFilter::Any) || !remainder.trim().is_empty() {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((
        rest,
        QuantityRef::TokensCreatedThisTurn {
            player: PlayerScope::Controller,
            filter,
        },
    ))
}

fn inject_controller(filter: TargetFilter, controller: ControllerRef) -> TargetFilter {
    match filter {
        TargetFilter::Typed(tf) => TargetFilter::Typed(tf.controller(controller)),
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|filter| inject_controller(filter, controller.clone()))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|filter| inject_controller(filter, controller.clone()))
                .collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(inject_controller(*filter, controller)),
        },
        other => other,
    }
}

/// CR 601.2h + CR 202.2: Parse a self-scoped mana-spent-to-cast reference in
/// any of three metrics:
///
/// - `DistinctColors` — "color[s] of mana spent to cast <self>" (Converge,
///   Sunburst class).
/// - `FromSource { source_filter }` — "mana from <source-filter> [that was]
///   spent to cast <self>" (Treasure/Cave/artifact-source cousins).
/// - `Total` — bare "mana spent to cast <self>" (Wildgrowth Archaic family,
///   Molten Note).
///
/// Recognized self-subjects come from `parse_mana_spent_self_subject`: `it`,
/// `this spell`, `this creature`, `this permanent`, `them`, `~`.
///
/// The same combinator is used both after "for each" (where the input has
/// already had the "for each " prefix stripped) and after "the number of"
/// (where the input has had "the number of " stripped) — the trailing surface
/// form is identical in both contexts, so a single combinator suffices.
fn parse_mana_spent_to_cast_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    if let Ok((rest, _)) = pair(tag::<_, _, OracleError<'_>>("color"), opt(tag("s"))).parse(input) {
        let (rest, _) = tag(" of mana spent to cast ").parse(rest)?;
        // SelfObject literal retained: this ref form never accepts "that" subjects.
        let (rest, _scope) = parse_mana_spent_self_subject(rest)?;
        return Ok((
            rest,
            QuantityRef::ManaSpentToCast {
                scope: CastManaObjectScope::SelfObject,
                metric: CastManaSpentMetric::DistinctColors,
            },
        ));
    }

    if let Ok((rest, source_filter)) = parse_mana_from_source_spent_to_cast(input) {
        return Ok((
            rest,
            QuantityRef::ManaSpentToCast {
                scope: CastManaObjectScope::SelfObject,
                metric: CastManaSpentMetric::FromSource { source_filter },
            },
        ));
    }

    let (rest, _) = tag("mana spent to cast ").parse(input)?;
    // SelfObject literal retained: this ref form never accepts "that" subjects.
    let (rest, _scope) = parse_mana_spent_self_subject(rest)?;
    Ok((
        rest,
        QuantityRef::ManaSpentToCast {
            scope: CastManaObjectScope::SelfObject,
            metric: CastManaSpentMetric::Total,
        },
    ))
}

/// CR 106.3 + CR 601.2h: Parse
/// "mana from [a/an] <source-filter> [source] spent to cast <self>" and the
/// "that was spent" variant.
pub(crate) fn parse_mana_from_source_spent_to_cast(input: &str) -> OracleResult<'_, TargetFilter> {
    let (rest, _) = tag("mana from ").parse(input)?;
    let (rest, source_filter) = parse_mana_source_filter(rest)?;
    let (rest, _) = alt((tag(" that was spent to cast "), tag(" spent to cast "))).parse(rest)?;
    // SelfObject literal retained: this ref form never accepts "that" subjects.
    let (rest, _scope) = parse_mana_spent_self_subject(rest)?;
    Ok((rest, source_filter))
}

pub(crate) fn parse_mana_source_filter(input: &str) -> OracleResult<'_, TargetFilter> {
    let (source_filter, rest) = parse_type_phrase(input);
    if rest.len() == input.len() {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let (rest, _) = opt(alt((tag(" sources"), tag(" source")))).parse(rest)?;
    Ok((rest, source_filter))
}

/// CR 400.7d: Parse the subject anaphora of a "mana spent to cast <subject>"
/// clause and report which `CastManaObjectScope` it selects.
///
/// The grammatical anaphora *is* the scope signal in MTG templating:
/// - "it" / "this spell" / "this creature" / "this permanent" / "them" / "~"
///   → the object the spell/ability *is* → `CastManaObjectScope::SelfObject`
/// - "that spell" / "that creature"
///   → an object referenced by a triggering event → `CastManaObjectScope::TriggeringSpell`
///
/// A resolving sorcery referring to "this spell" must select `SelfObject` (CR
/// 400.7d): the resolving spell references its own payment-time mana. A
/// triggered ability referring to "that spell" selects `TriggeringSpell`.
pub(crate) fn parse_mana_spent_self_subject(input: &str) -> OracleResult<'_, CastManaObjectScope> {
    alt((
        value(CastManaObjectScope::TriggeringSpell, tag("that spell")),
        value(CastManaObjectScope::TriggeringSpell, tag("that creature")),
        value(CastManaObjectScope::SelfObject, tag("this spell")),
        value(CastManaObjectScope::SelfObject, tag("this creature")),
        value(CastManaObjectScope::SelfObject, tag("this permanent")),
        value(CastManaObjectScope::SelfObject, tag("it")),
        value(CastManaObjectScope::SelfObject, tag("them")),
        value(CastManaObjectScope::SelfObject, tag("~")),
    ))
    .parse(input)
}

/// CR 122.1 + CR 122.6: Parse post-"for each" counter-placement history,
/// e.g. "+1/+1 counter you've put on creatures under your control this turn".
pub fn parse_counter_added_this_turn_for_each(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, counters) = parse_typed_counter_match(input)?;
    let (rest, _) = alt((tag(" you've put on "), tag(" you put on "))).parse(rest)?;
    let (rest, target) = parse_counter_added_target(rest)?;
    let (rest, _) = tag(" this turn").parse(rest)?;
    Ok((
        rest,
        QuantityRef::CounterAddedThisTurn {
            actor: CountScope::Controller,
            counters,
            target,
        },
    ))
}

/// CR 122.1 + CR 603.4: Parse "you've put one or more +1/+1 counters on a
/// creature this turn" and the generic-counter sibling "you put a counter on a
/// permanent this turn" into the shared counter-history quantity.
pub fn parse_counter_added_this_turn_condition(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        parse_counter_added_this_turn_condition_active,
        parse_counter_added_this_turn_condition_passive,
    ))
    .parse(input)
}

fn parse_counter_added_this_turn_condition_active(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((tag("you put "), tag("you've put "))).parse(input)?;
    let (rest, _) = alt((tag("one or more "), tag("a "))).parse(rest)?;
    let (rest, counters) =
        alt((parse_typed_counter_match, parse_generic_counter_match)).parse(rest)?;
    let (rest, _) = tag(" on ").parse(rest)?;
    let (rest, target) = parse_counter_added_target(rest)?;
    let (rest, _) = tag(" this turn").parse(rest)?;
    Ok((
        rest,
        QuantityRef::CounterAddedThisTurn {
            actor: CountScope::Controller,
            counters,
            target,
        },
    ))
}

fn parse_counter_added_this_turn_condition_passive(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = parse_article(input)?;
    let (rest, counters) = parse_typed_counter_match(rest)?;
    let (rest, _) = tag(" was put on ").parse(rest)?;
    let (rest, target) = parse_counter_added_target(rest)?;
    let (rest, _) = tag(" this turn").parse(rest)?;
    Ok((
        rest,
        QuantityRef::CounterAddedThisTurn {
            actor: CountScope::All,
            counters,
            target,
        },
    ))
}

fn parse_typed_counter_match(input: &str) -> OracleResult<'_, CounterMatch> {
    let (rest, counter_type) = parse_counter_type_typed(input)?;
    let (rest, _) = parse_counter_word(rest)?;
    Ok((rest, CounterMatch::OfType(counter_type)))
}

fn parse_generic_counter_match(input: &str) -> OracleResult<'_, CounterMatch> {
    value(CounterMatch::Any, alt((tag("counters"), tag("counter")))).parse(input)
}

fn parse_counter_word(input: &str) -> OracleResult<'_, ()> {
    let (rest, _) = tag(" counter").parse(input)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    Ok((rest, ()))
}

fn parse_counter_added_target(input: &str) -> OracleResult<'_, TargetFilter> {
    let (rest, _) = opt(alt((tag("a "), tag("an ")))).parse(input)?;
    alt((
        value(
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            alt((
                tag("creature under your control"),
                tag("creature you control"),
                tag("creatures under your control"),
                tag("creatures you control"),
            )),
        ),
        value(
            TargetFilter::Typed(TypedFilter::creature()),
            alt((tag("creatures"), tag("creature"))),
        ),
        value(
            TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::You)),
            alt((
                tag("permanent under your control"),
                tag("permanent you control"),
                tag("permanents under your control"),
                tag("permanents you control"),
            )),
        ),
        value(
            TargetFilter::Typed(TypedFilter::permanent()),
            alt((tag("permanents"), tag("permanent"))),
        ),
    ))
    .parse(rest)
}

/// CR 201.1 + CR 201.2: Parse
/// "word[s] in <object>'s name" into a scoped object-name word count. The
/// `"its"` form is recipient-relative so Aura/Equipment statics bind to the
/// enchanted/equipped object rather than the source permanent.
fn parse_object_name_word_count_for_each(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((tag("words"), tag("word"))).parse(input)?;
    let (rest, _) = tag(" in ").parse(rest)?;
    let (rest, scope) = parse_object_possessive_scope(rest)?;
    let (rest, _) = tag(" name").parse(rest)?;
    Ok((rest, QuantityRef::ObjectNameWordCount { scope }))
}

/// CR 107.4 + CR 202.1: Parse
/// "<color> mana symbol[s] in <object>'s mana cost" into a scoped per-object
/// mana-cost symbol count. The `"its"` form is recipient-relative so static
/// layer boosts bind to each affected object.
fn parse_mana_symbols_in_object_mana_cost_for_each(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, color) = super::primitives::parse_color(input)?;
    let (rest, _) = tag(" mana symbol").parse(rest)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    let (rest, _) = tag(" in ").parse(rest)?;
    let (rest, scope) = parse_object_possessive_scope(rest)?;
    let (rest, _) = tag(" mana cost").parse(rest)?;
    Ok((rest, QuantityRef::ManaSymbolsInManaCost { scope, color }))
}

/// CR 105.1 + CR 105.2: Parse "for each [of] <object>'s colors" into a
/// scoped object-color count. The `"its"` form is recipient-relative: in
/// continuous effects it binds to the affected object; in targeted effects it
/// falls back to the selected object target.
fn parse_object_colors_for_each(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = opt(tag("of ")).parse(input)?;
    parse_object_colors_ref_tail(rest)
}

fn parse_object_colors_ref_tail(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, scope) = parse_object_possessive_scope(input)?;
    let (rest, _) = tag(" color").parse(rest)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    Ok((rest, QuantityRef::ObjectColorCount { scope }))
}

fn parse_number_of_object_name_words_tail(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((tag("words in "), tag("word in "))).parse(input)?;
    let (rest, scope) = parse_object_possessive_scope(rest)?;
    let (rest, _) = tag(" name").parse(rest)?;
    Ok((rest, QuantityRef::ObjectNameWordCount { scope }))
}

fn parse_number_of_object_colors_tail(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, scope) = alt((
        value(ObjectScope::EventSource, tag("colors that spell is")),
        |i| {
            let (rest, _) = tag("colors of ").parse(i)?;
            let (rest, scope) = parse_object_color_of_scope(rest)?;
            Ok((rest, scope))
        },
    ))
    .parse(input)?;
    Ok((rest, QuantityRef::ObjectColorCount { scope }))
}

/// Parse controller-relative combat-class counts:
/// "for each attacking/blocking creature they/you control".
fn parse_for_each_combat_creature_controlled(
    input: &str,
    they_controller: ControllerRef,
) -> OracleResult<'_, QuantityRef> {
    let (rest, combat_property) = alt((
        value(FilterProp::Attacking, tag("attacking ")),
        value(FilterProp::Blocking, tag("blocking ")),
    ))
    .parse(input)?;
    let (rest, tf) = parse_type_filter_word(rest)?;
    let (rest, controller) = alt((
        value(they_controller, tag(" they control")),
        value(ControllerRef::You, tag(" you control")),
    ))
    .parse(rest)?;

    Ok((
        rest,
        QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![tf],
                controller: Some(controller),
                properties: vec![combat_property],
            }),
        },
    ))
}

/// Parse source-excluding combat-class counts:
/// "for each attacking/blocking creature other than ~".
fn parse_for_each_combat_creature_other_than_source(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, combat_property) = alt((
        value(FilterProp::Attacking, tag("attacking ")),
        value(FilterProp::Blocking, tag("blocking ")),
    ))
    .parse(input)?;
    let (rest, _) = tag("creature").parse(rest)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    let (rest, _) = tag(" other than ").parse(rest)?;
    let (rest, _) = alt((tag("~"), tag("this creature"))).parse(rest)?;

    Ok((
        rest,
        QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: None,
                properties: vec![combat_property, FilterProp::Another],
            }),
        },
    ))
}

fn parse_object_possessive_scope(input: &str) -> OracleResult<'_, ObjectScope> {
    alt((
        value(ObjectScope::Recipient, tag("its")),
        value(ObjectScope::Recipient, tag("their")),
        value(ObjectScope::Recipient, tag("enchanted creature's")),
        value(ObjectScope::Recipient, tag("equipped creature's")),
        value(ObjectScope::Target, tag("target creature's")),
        value(ObjectScope::Target, tag("target permanent's")),
        value(ObjectScope::EventSource, tag("that spell's")),
        value(ObjectScope::Target, tag("that creature's")),
        value(ObjectScope::Target, tag("that permanent's")),
        value(ObjectScope::Target, tag("that planeswalker's")),
        value(ObjectScope::Source, tag("~'s")),
        value(ObjectScope::Source, tag("this creature's")),
        value(ObjectScope::Source, tag("this permanent's")),
        value(ObjectScope::Source, tag("this spell's")),
        value(ObjectScope::Source, tag("this card's")),
    ))
    .parse(input)
}

fn parse_object_color_of_scope(input: &str) -> OracleResult<'_, ObjectScope> {
    alt((
        value(ObjectScope::Recipient, tag("it")),
        value(ObjectScope::Recipient, tag("the enchanted creature")),
        value(ObjectScope::Recipient, tag("the equipped creature")),
        value(ObjectScope::Target, tag("target creature")),
        value(ObjectScope::Target, tag("target permanent")),
        value(ObjectScope::EventSource, tag("the triggering spell")),
        value(ObjectScope::EventSource, tag("that spell")),
        value(ObjectScope::Target, tag("that creature")),
        value(ObjectScope::Target, tag("that permanent")),
        value(ObjectScope::Target, tag("that planeswalker")),
        value(ObjectScope::Source, tag("~")),
        value(ObjectScope::Source, tag("this creature")),
        value(ObjectScope::Source, tag("this permanent")),
        value(ObjectScope::Source, tag("this spell")),
        value(ObjectScope::Source, tag("this card")),
    ))
    .parse(input)
}

/// CR 702.143c-d: "foretold card you own in exile" counts cards carrying the
/// foretold designation in exile. The designation is distinct from the
/// Foretell keyword; a foretold card may be made foretold by an effect.
fn parse_foretold_cards_owned_in_exile(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("foretold ").parse(input)?;
    let (rest, _) = alt((tag("card"), tag("cards"))).parse(rest)?;
    let (rest, _) = tag(" you own in exile").parse(rest)?;
    Ok((
        rest,
        QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter::card().properties(vec![
                FilterProp::Foretold,
                FilterProp::Owned {
                    controller: ControllerRef::You,
                },
                FilterProp::InZone {
                    zone: crate::types::zones::Zone::Exile,
                },
            ])),
        },
    ))
}

fn parse_for_each_commander_cast_count(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("time").parse(input)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    let (rest, _) = tag(" ").parse(rest)?;
    let (rest, _) = alt((tag("you've"), tag("youve"))).parse(rest)?;
    let (rest, _) = tag(" cast your commander from the command zone this game").parse(rest)?;
    Ok((rest, QuantityRef::CommanderCastFromCommandZoneCount))
}

/// CR 700.4: Parse "creature that died" / "creature that died
/// under your control" → filtered zone-change count.
///
/// Engine tracking is per-turn-only (no last-turn / total counts), so the
/// trailing "this turn" qualifier is semantically redundant — it gets stripped
/// upstream by `strip_trailing_duration` before this arm sees the clause.
/// Both the with-qualifier and without-qualifier forms map to the same
/// `ZoneChangeCountThisTurn` quantity ref.
fn parse_for_each_creature_died_this_turn(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((
        // "creature that died" canonical forms
        tag("creature that died under your control this turn"),
        tag("creature that died under your control"),
        tag("creature that died this turn"),
        tag("creature that died"),
        // CR 700.4: "creature put into [a/your] graveyard from the battlefield"
        // is the long form of "died" — both reference the same battlefield→
        // graveyard transition tracked in `zone_changes_this_turn`.
        tag("creature put into your graveyard from the battlefield this turn"),
        tag("creature put into your graveyard from the battlefield"),
        tag("creature put into a graveyard from the battlefield this turn"),
        tag("creature put into a graveyard from the battlefield"),
    ))
    .parse(input)?;
    Ok((rest, creatures_died_this_turn_ref()))
}

fn parse_for_each_subtype_died_this_turn(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, subtype_text) = take_until(" that died").parse(input)?;
    let (rest, _) = alt((tag(" that died this turn"), tag(" that died"))).parse(rest)?;
    let Some((subtype, consumed)) = parse_subtype(subtype_text) else {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    };
    if consumed != subtype_text.len() {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((
        rest,
        QuantityRef::ZoneChangeCountThisTurn {
            from: Some(Zone::Battlefield),
            to: Some(Zone::Graveyard),
            filter: TargetFilter::Typed(TypedFilter::creature().subtype(subtype)),
        },
    ))
}

fn creatures_died_this_turn_ref() -> QuantityRef {
    QuantityRef::ZoneChangeCountThisTurn {
        from: Some(Zone::Battlefield),
        to: Some(Zone::Graveyard),
        filter: TargetFilter::Typed(TypedFilter::creature()),
    }
}

/// CR 301.5 + CR 303.4: Parse "<type> [and <type>]* attached to ~" — counts
/// objects whose `attached_to` field references the source object. Used by
/// "for each Aura and Equipment attached to ~" (Kellan, the Fae-Blooded) and
/// any analogous boost that scales with attachments on the source.
///
/// Composes `parse_type_filter_word` for each type term, joined by " and ",
/// then matches `" attached to ~"`. Returns a `QuantityRef::ObjectCount` over
/// a `TypedFilter` whose type filters are the matched types and whose only
/// property is `FilterProp::AttachedToSource`.
fn parse_for_each_attached_to_source(input: &str) -> OracleResult<'_, QuantityRef> {
    let (mut rest, first) = parse_type_filter_word(input)?;
    let mut types = vec![first];
    while let Ok((after_and, _)) = tag::<_, _, OracleError<'_>>(" and ").parse(rest) {
        let (after_type, next) = parse_type_filter_word(after_and)?;
        types.push(next);
        rest = after_type;
    }
    // CR 301.5 + CR 303.4 + CR 613.4c: Two referents share the "<type>
    // [and <type>]* attached to <referent>" shape. The static parser already
    // normalizes the source's printed name to `~`, so a literal `~` referent
    // means "attached to the static's source object" (Kellan, the
    // Fae-Blooded — `AttachedToSource`). The pronoun/noun phrase
    // `it` / `that creature` is anaphoric on the affected subject of the
    // surrounding effect — for
    // "Enchanted creature gets +N/+M for each Aura and Equipment attached to
    // it", "it" refers to the enchanted creature, the per-recipient host of
    // the layer-evaluated boost (`AttachedToRecipient`). Baki's Curse uses the
    // same recipient-relative grammar for damage: "each creature for each Aura
    // attached to that creature." These literals are single-token leaves of
    // the same combinator, so we dispatch with `alt` and select the matching
    // `FilterProp` from a typed pair.
    let (rest, prop) = alt((
        value(FilterProp::AttachedToSource, tag(" attached to ~")),
        value(
            FilterProp::AttachedToRecipient,
            alt((tag(" attached to it"), tag(" attached to that creature"))),
        ),
    ))
    .parse(rest)?;
    let type_filters = if types.len() == 1 {
        types
    } else {
        vec![TypeFilter::AnyOf(types)]
    };
    Ok((
        rest,
        QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters,
                controller: None,
                properties: vec![prop],
            }),
        },
    ))
}

fn parse_for_each_attacking_controller_type(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, tf) = parse_type_filter_word(input)?;
    let (rest, _) = tag(" attacking you").parse(rest)?;
    Ok((
        rest,
        QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![tf],
                controller: None,
                properties: vec![FilterProp::AttackingController],
            }),
        },
    ))
}

fn parse_for_each_blocking_source_type(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, tf) = parse_type_filter_word(input)?;
    let (rest, _) = alt((
        tag(" blocking it"),
        tag(" blocking ~"),
        tag(" blocking this creature"),
    ))
    .parse(rest)?;
    Ok((
        rest,
        QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![tf],
                controller: None,
                properties: vec![FilterProp::BlockingSource],
            }),
        },
    ))
}

fn parse_for_each_recipient_shared_quality(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, has_other) =
        opt(alt((value((), tag("other ")), value((), tag("another "))))).parse(input)?;
    let (rest, type_filter) = parse_type_filter_word(rest)?;
    let (rest, _) = tag(" on the battlefield ").parse(rest)?;
    let (rest, shared_quality) = parse_shared_quality_clause(rest)?;

    let mut properties = Vec::new();
    if has_other.is_some() {
        properties.push(FilterProp::Another);
    }
    properties.push(shared_quality);

    Ok((
        rest,
        QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![type_filter],
                controller: None,
                properties,
            }),
        },
    ))
}

fn parse_for_each_battlefield_type(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, tf) = parse_type_filter_word(input)?;
    let (rest, _) = tag(" on the battlefield").parse(rest)?;
    Ok((
        rest,
        QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![tf],
                controller: None,
                properties: Vec::new(),
            }),
        },
    ))
}

fn parse_for_each_controlled_type(input: &str) -> OracleResult<'_, QuantityRef> {
    // CR 109.4: Only objects on the stack or on the battlefield have a
    // controller, so a "you control" count is over battlefield permanents
    // under the source's controller. An optional leading "other " / "another "
    // prefix is lowered to `FilterProp::Another`, which excludes the source
    // object at runtime via filter evaluation against its identity.
    let (rest, has_other) = nom::combinator::opt(alt((
        nom::combinator::value((), tag::<_, _, OracleError<'_>>("other ")),
        nom::combinator::value((), tag("another ")),
    )))
    .parse(input)?;
    let (rest, tf) = parse_type_filter_word(rest)?;
    // Tolerate the "already" adverb in "<type> you already control" so the
    // count matches tribal payoffs like Giada ("for each Angel you already
    // control"). The adverb sits between "you" and "control", so the literal
    // " you control" tag is split around an optional " already".
    let (rest, _) = tag(" you").parse(rest)?;
    let (rest, _) = opt(tag(" already")).parse(rest)?;
    let (rest, _) = tag(" control").parse(rest)?;
    let (rest, chosen_type_prop) = opt(alt((
        value(FilterProp::IsChosenCreatureType, tag(" of that type")),
        value(FilterProp::IsChosenCreatureType, tag(" of the chosen type")),
    )))
    .parse(rest)?;
    let mut properties = Vec::new();
    if has_other.is_some() {
        properties.push(FilterProp::Another);
    }
    if let Some(prop) = chosen_type_prop {
        properties.push(prop);
    }
    if !properties.is_empty() {
        return Ok((
            rest,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![tf],
                    controller: Some(ControllerRef::You),
                    properties,
                }),
            },
        ));
    }
    Ok((
        rest,
        QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![tf],
                controller: Some(ControllerRef::You),
                properties: Vec::new(),
            }),
        },
    ))
}

#[cfg(test)]
fn assert_for_each_controlled_chosen_type(
    clause: &str,
    expected_type: TypeFilter,
    expected_properties: Vec<FilterProp>,
) {
    let (rest, q) = parse_for_each_clause_ref(clause).unwrap();
    assert_eq!(rest, "");
    match q {
        QuantityRef::ObjectCount { filter } => match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.type_filters, vec![expected_type]);
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert_eq!(tf.properties, expected_properties);
            }
            other => panic!("expected Typed filter, got {other:?}"),
        },
        other => panic!("expected ObjectCount, got {other:?}"),
    }
}

/// Parse "your speed" → the controller's speed (CR 702.179f).
fn parse_speed_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    value(
        QuantityRef::Speed {
            player: PlayerScope::Controller,
        },
        tag("your speed"),
    )
    .parse(input)
}

/// CR 122.1: Parse "[kind] counters <possessor>" → `QuantityRef::PlayerCounter`.
///
/// Reached after `parse_the_number_of` consumes the leading `"the number of "`.
/// Composes a typed kind alt and a typed possessor alt — no string matching
/// downstream and no permutation-enumerated tag lists.
fn parse_player_counter_ref_tail(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, kind) = parse_player_counter_kind(input)?;
    let (rest, _) = tag(" counter").parse(rest)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    let (rest, _) = tag(" ").parse(rest)?;
    let (rest, scope) = parse_player_counter_possessor(rest)?;
    Ok((rest, QuantityRef::PlayerCounter { kind, scope }))
}

/// CR 122.1: Parse the full "the number of [kind] counters <possessor>" phrase.
///
/// Public entry point used by trailing "where X is …" plumbing in the
/// imperative parser (see `parse_earthbend_counter_count`). Mirrors the arm
/// composed inside `parse_quantity_ref` so static and imperative parsing
/// share a single grammar authority.
pub fn parse_the_number_of_player_counters(input: &str) -> OracleResult<'_, QuantityRef> {
    preceded(tag("the number of "), parse_player_counter_ref_tail).parse(input)
}

/// CR 122.1: Typed alt over named player-counter kinds. Each arm emits the
/// `PlayerCounterKind` variant directly (no intermediate string). `pub(crate)`
/// so the `PlayerCounter` player-attribute predicate parser
/// (`oracle_quantity::parse_player_attribute_predicate`) shares this single
/// kind grammar rather than re-enumerating counter tags.
pub(crate) fn parse_player_counter_kind(input: &str) -> OracleResult<'_, PlayerCounterKind> {
    alt((
        value(PlayerCounterKind::Experience, tag("experience")),
        value(PlayerCounterKind::Poison, tag("poison")),
        value(PlayerCounterKind::Rad, tag("rad")),
        value(PlayerCounterKind::Ticket, tag("ticket")),
    ))
    .parse(input)
}

/// CR 122.1 + CR 109.5: Typed possessor alt mapping to `CountScope`. Each arm
/// emits the scope variant directly. New possessor phrases extend this typed
/// alt rather than adding full phrase permutations.
fn parse_player_counter_possessor(input: &str) -> OracleResult<'_, CountScope> {
    alt((
        value(CountScope::Controller, tag("you have")),
        value(CountScope::ScopedPlayer, tag("that player has")),
        value(CountScope::Opponents, tag("each opponent has")),
        value(CountScope::Opponents, tag("your opponents have")),
        value(CountScope::All, tag("each player has")),
    ))
    .parse(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AggregateFunction, FilterProp, ObjectProperty, SharedQuality, SharedQualityRelation,
        TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::mana::ManaColor;

    /// CR 400.7d: each subject anaphora maps to the correct
    /// `CastManaObjectScope` — "this …"/"it"/"them"/"~" → `SelfObject`;
    /// "that …" → `TriggeringSpell`.
    #[test]
    fn test_parse_mana_spent_self_subject_scope() {
        for subj in [
            "it",
            "this spell",
            "this creature",
            "this permanent",
            "them",
            "~",
        ] {
            let (rest, scope) = parse_mana_spent_self_subject(subj).unwrap();
            assert_eq!(rest, "", "subject {subj:?} should fully consume");
            assert_eq!(scope, CastManaObjectScope::SelfObject, "subject {subj:?}");
        }
        for subj in ["that spell", "that creature"] {
            let (rest, scope) = parse_mana_spent_self_subject(subj).unwrap();
            assert_eq!(rest, "", "subject {subj:?} should fully consume");
            assert_eq!(
                scope,
                CastManaObjectScope::TriggeringSpell,
                "subject {subj:?}"
            );
        }
    }

    #[test]
    fn test_parse_quantity_fixed() {
        let (rest, q) = parse_quantity("3 damage").unwrap();
        assert_eq!(q, QuantityExpr::Fixed { value: 3 });
        assert_eq!(rest, " damage");
    }

    #[test]
    fn parse_for_each_attached_to_source_two_kinds() {
        // CR 301.5 + CR 303.4: Kellan, the Fae-Blooded — "for each Aura and
        // Equipment attached to ~". Composes a typed AnyOf over Aura/Equipment
        // subtypes with the new `AttachedToSource` filter prop.
        let (rest, q) = parse_for_each_clause_ref("aura and equipment attached to ~").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::ObjectCount { filter } => match filter {
                TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller,
                    properties,
                }) => {
                    assert_eq!(controller, None);
                    assert_eq!(properties, vec![FilterProp::AttachedToSource]);
                    assert_eq!(
                        type_filters,
                        vec![TypeFilter::AnyOf(vec![
                            TypeFilter::Subtype("Aura".into()),
                            TypeFilter::Subtype("Equipment".into())
                        ])]
                    );
                }
                other => panic!("expected Typed filter, got {other:?}"),
            },
            other => panic!("expected ObjectCount, got {other:?}"),
        }
    }

    #[test]
    fn parse_for_each_attached_to_source_single_kind() {
        // Single-subtype variant: "for each Aura attached to ~" — proves the
        // combinator handles singular type lists without an outer `AnyOf`.
        let (rest, q) = parse_for_each_clause_ref("aura attached to ~").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::ObjectCount { filter } => match filter {
                TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller,
                    properties,
                }) => {
                    assert_eq!(controller, None);
                    assert_eq!(properties, vec![FilterProp::AttachedToSource]);
                    assert_eq!(type_filters, vec![TypeFilter::Subtype("Aura".into())]);
                }
                other => panic!("expected Typed filter, got {other:?}"),
            },
            other => panic!("expected ObjectCount, got {other:?}"),
        }
    }

    #[test]
    fn parse_for_each_attached_to_recipient_two_kinds_strong_back() {
        // CR 301.5 + CR 303.4 + CR 613.4c: Strong Back's "Enchanted creature
        // gets +2/+2 for each Aura and Equipment attached to it." The pronoun
        // "it" refers to the *enchanted creature* (the per-recipient host of
        // the Aura's continuous boost), not to the static's source. The
        // combinator must emit `AttachedToRecipient`, distinct from Kellan's
        // self-relative `AttachedToSource`.
        let (rest, q) = parse_for_each_clause_ref("aura and equipment attached to it").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::ObjectCount { filter } => match filter {
                TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller,
                    properties,
                }) => {
                    assert_eq!(controller, None);
                    assert_eq!(properties, vec![FilterProp::AttachedToRecipient]);
                    assert_eq!(
                        type_filters,
                        vec![TypeFilter::AnyOf(vec![
                            TypeFilter::Subtype("Aura".into()),
                            TypeFilter::Subtype("Equipment".into())
                        ])]
                    );
                }
                other => panic!("expected Typed filter, got {other:?}"),
            },
            other => panic!("expected ObjectCount, got {other:?}"),
        }
    }

    #[test]
    fn parse_for_each_attached_to_recipient_single_kind() {
        // CR 303.4 + CR 613.4c: Single-subtype variant ("for each Aura
        // attached to it" — Auramancer's Guise / Gatherer of Graces /
        // Graceblade Artisan family). Confirms the singular path also emits
        // `AttachedToRecipient`.
        let (rest, q) = parse_for_each_clause_ref("aura attached to it").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::ObjectCount { filter } => match filter {
                TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller,
                    properties,
                }) => {
                    assert_eq!(controller, None);
                    assert_eq!(properties, vec![FilterProp::AttachedToRecipient]);
                    assert_eq!(type_filters, vec![TypeFilter::Subtype("Aura".into())]);
                }
                other => panic!("expected Typed filter, got {other:?}"),
            },
            other => panic!("expected ObjectCount, got {other:?}"),
        }
    }

    #[test]
    fn parse_for_each_attached_to_that_creature_recipient() {
        let (rest, q) = parse_for_each_clause_ref("aura attached to that creature").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::ObjectCount { filter } => match filter {
                TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller,
                    properties,
                }) => {
                    assert_eq!(controller, None);
                    assert_eq!(properties, vec![FilterProp::AttachedToRecipient]);
                    assert_eq!(type_filters, vec![TypeFilter::Subtype("Aura".into())]);
                }
                other => panic!("expected Typed filter, got {other:?}"),
            },
            other => panic!("expected ObjectCount, got {other:?}"),
        }
    }

    #[test]
    fn parse_for_each_other_battlefield_creature_sharing_type_with_recipient() {
        for clause in [
            "other creature on the battlefield that shares a creature type with it",
            "other creature on the battlefield that shares at least one creature type with it",
        ] {
            let (rest, q) = parse_for_each_clause_ref(clause).unwrap();
            assert_eq!(rest, "");
            match q {
                QuantityRef::ObjectCount { filter } => match filter {
                    TargetFilter::Typed(TypedFilter {
                        type_filters,
                        controller,
                        properties,
                    }) => {
                        assert_eq!(type_filters, vec![TypeFilter::Creature]);
                        assert_eq!(controller, None);
                        assert!(properties.iter().any(|prop| prop == &FilterProp::Another));
                        assert!(properties.iter().any(|prop| matches!(
                            prop,
                            FilterProp::SharesQuality {
                                quality: SharedQuality::CreatureType,
                                reference: Some(reference),
                                relation: SharedQualityRelation::Shares,
                            } if matches!(reference.as_ref(), TargetFilter::ParentTarget)
                        )));
                    }
                    other => panic!("expected Typed filter, got {other:?}"),
                },
                other => panic!("expected ObjectCount, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_for_each_counter_added_this_turn_counts_typed_recipient() {
        let (rest, q) = parse_for_each_clause_ref(
            "+1/+1 counter you've put on creatures under your control this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::CounterAddedThisTurn {
                actor: CountScope::Controller,
                counters: crate::types::counter::CounterMatch::OfType(
                    crate::types::counter::CounterType::Plus1Plus1,
                ),
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            }
        );
    }

    #[test]
    fn parse_for_each_color_of_mana_spent_to_cast_this_spell() {
        let (rest, q) =
            parse_for_each_clause_ref("color of mana spent to cast this spell").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::ManaSpentToCast {
                scope: crate::types::ability::CastManaObjectScope::SelfObject,
                metric: crate::types::ability::CastManaSpentMetric::DistinctColors
            }
        );

        let (rest, q) = parse_for_each_clause_ref("colors of mana spent to cast it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::ManaSpentToCast {
                scope: crate::types::ability::CastManaObjectScope::SelfObject,
                metric: crate::types::ability::CastManaSpentMetric::DistinctColors
            }
        );
    }

    #[test]
    fn parse_for_each_mana_spent_to_cast_it() {
        let (rest, q) = parse_for_each_clause_ref("mana spent to cast it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::ManaSpentToCast {
                scope: crate::types::ability::CastManaObjectScope::SelfObject,
                metric: crate::types::ability::CastManaSpentMetric::Total
            }
        );
    }

    #[test]
    fn parse_for_each_mana_from_source_spent_to_cast_it() {
        let (rest, q) = parse_for_each_clause_ref("mana from a cave spent to cast it").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::ManaSpentToCast {
                scope: crate::types::ability::CastManaObjectScope::SelfObject,
                metric: crate::types::ability::CastManaSpentMetric::FromSource { source_filter },
            } => match source_filter {
                TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                    assert_eq!(type_filters, vec![TypeFilter::Subtype("Cave".into())]);
                }
                other => panic!("expected typed source filter, got {other:?}"),
            },
            other => panic!("expected source-qualified mana spent ref, got {other:?}"),
        }

        let (rest, q) =
            parse_for_each_clause_ref("mana from an artifact source spent to cast it").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::ManaSpentToCast {
                scope: crate::types::ability::CastManaObjectScope::SelfObject,
                metric: crate::types::ability::CastManaSpentMetric::FromSource { source_filter },
            } => match source_filter {
                TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                    assert_eq!(type_filters, vec![TypeFilter::Artifact]);
                }
                other => panic!("expected typed source filter, got {other:?}"),
            },
            other => panic!("expected source-qualified mana spent ref, got {other:?}"),
        }

        let (rest, q) =
            parse_for_each_clause_ref("mana from a treasure that was spent to cast this spell")
                .unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            q,
            QuantityRef::ManaSpentToCast {
                scope: crate::types::ability::CastManaObjectScope::SelfObject,
                metric: crate::types::ability::CastManaSpentMetric::FromSource { .. },
            }
        ));

        let (rest, q) =
            parse_for_each_clause_ref("mana from a treasure spent to cast them").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            q,
            QuantityRef::ManaSpentToCast {
                scope: crate::types::ability::CastManaObjectScope::SelfObject,
                metric: crate::types::ability::CastManaSpentMetric::FromSource { .. },
            }
        ));

        let (rest, q) =
            parse_for_each_clause_ref("mana from an artifact or creature source spent to cast it")
                .unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::ManaSpentToCast {
                metric: crate::types::ability::CastManaSpentMetric::FromSource { source_filter },
                ..
            } => assert!(matches!(source_filter, TargetFilter::Or { .. })),
            other => panic!("expected source-qualified mana spent ref, got {other:?}"),
        }
    }

    /// CR 202.2 + CR 601.2h + CR 207.2c: GitHub #307 — Painful Truths bug.
    /// "the number of colors of mana spent to cast this spell" is the canonical
    /// Converge ability-word phrase. It must produce
    /// `ManaSpentToCast { metric: DistinctColors }` so that the where-X rewriter
    /// rebinds the bare `Variable("X")` count in `Draw`/`LoseLife`/etc. to the
    /// actual distinct-color count of the cast. Before the fix, the dispatcher
    /// only matched the `it` subject and fell back to an empty `ObjectCount`
    /// when the spell text used `this spell`, causing X to resolve to the
    /// battlefield permanent count (~30 in the late game).
    #[test]
    fn parse_quantity_ref_the_number_of_colors_of_mana_spent_to_cast_this_spell() {
        for input in [
            "the number of colors of mana spent to cast this spell",
            "the number of colors of mana spent to cast it",
            "the number of colors of mana spent to cast this creature",
            "the number of colors of mana spent to cast this permanent",
            "the number of colors of mana spent to cast them",
            "the number of color of mana spent to cast this spell",
            "the number of colors of mana spent to cast ~",
        ] {
            let (rest, q) =
                parse_quantity_ref(input).unwrap_or_else(|_| panic!("failed to parse {input:?}"));
            assert_eq!(rest, "", "leftover input for {input:?}");
            assert_eq!(
                q,
                QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::SelfObject,
                    metric: CastManaSpentMetric::DistinctColors,
                },
                "wrong ref for {input:?}"
            );
        }
    }

    /// CR 601.2h: Bare "the number of mana spent to cast …" → `Total` metric.
    /// Less common than the colors form but covered by the same combinator —
    /// the `parse_mana_spent_to_cast_ref` shared between the for-each and
    /// number-of dispatch paths handles all three metrics uniformly.
    #[test]
    fn parse_quantity_ref_the_number_of_mana_spent_to_cast_self_subjects() {
        for input in [
            "the number of mana spent to cast this spell",
            "the number of mana spent to cast it",
        ] {
            let (rest, q) =
                parse_quantity_ref(input).unwrap_or_else(|_| panic!("failed to parse {input:?}"));
            assert_eq!(rest, "", "leftover input for {input:?}");
            assert_eq!(
                q,
                QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::SelfObject,
                    metric: CastManaSpentMetric::Total,
                },
                "wrong ref for {input:?}"
            );
        }
    }

    #[test]
    fn parse_counter_added_condition_accepts_typed_creature_target() {
        let (rest, q) = parse_counter_added_this_turn_condition(
            "you've put one or more +1/+1 counters on a creature this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::CounterAddedThisTurn {
                actor: CountScope::Controller,
                counters: crate::types::counter::CounterMatch::OfType(
                    crate::types::counter::CounterType::Plus1Plus1,
                ),
                target: TargetFilter::Typed(TypedFilter::creature()),
            }
        );
    }

    #[test]
    fn parse_counter_added_condition_accepts_passive_owned_permanent_target() {
        let (rest, q) = parse_counter_added_this_turn_condition(
            "a +1/+1 counter was put on a permanent under your control this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::CounterAddedThisTurn {
                actor: CountScope::All,
                counters: crate::types::counter::CounterMatch::OfType(
                    crate::types::counter::CounterType::Plus1Plus1,
                ),
                target: TargetFilter::Typed(
                    TypedFilter::permanent().controller(ControllerRef::You)
                ),
            }
        );
    }

    #[test]
    fn parse_for_each_foretold_card_owned_in_exile() {
        let (rest, q) = parse_for_each_clause_ref("foretold card you own in exile").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::card().properties(vec![
                    FilterProp::Foretold,
                    FilterProp::Owned {
                        controller: ControllerRef::You,
                    },
                    FilterProp::InZone {
                        zone: crate::types::zones::Zone::Exile,
                    },
                ])),
            }
        );
    }

    #[test]
    fn parse_for_each_permanent_you_control_of_that_type() {
        assert_for_each_controlled_chosen_type(
            "permanent you control of that type",
            TypeFilter::Permanent,
            vec![FilterProp::IsChosenCreatureType],
        );
    }

    #[test]
    fn parse_for_each_permanent_you_control_of_the_chosen_type() {
        assert_for_each_controlled_chosen_type(
            "permanent you control of the chosen type",
            TypeFilter::Permanent,
            vec![FilterProp::IsChosenCreatureType],
        );
    }

    #[test]
    fn parse_for_each_other_creature_you_control_of_that_type() {
        assert_for_each_controlled_chosen_type(
            "other creature you control of that type",
            TypeFilter::Creature,
            vec![FilterProp::Another, FilterProp::IsChosenCreatureType],
        );
    }

    /// Issue #204 — Giada, Font of Hope: "for each Angel you already control".
    /// The `already` adverb between the subtype word and " you control" must be
    /// tolerated so the count resolves to a dynamic `ObjectCount`.
    #[test]
    fn parse_for_each_subtype_you_already_control() {
        let (rest, q) = parse_for_each_clause_ref("Angel you already control").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Subtype("Angel".to_string())],
                    controller: Some(ControllerRef::You),
                    properties: Vec::new(),
                }),
            }
        );
    }

    /// Negative control: the same phrase without the `already` adverb parses
    /// identically — the `opt(tag(" already"))` is non-consuming when absent.
    #[test]
    fn parse_for_each_subtype_you_control_no_adverb() {
        let (rest, q) = parse_for_each_clause_ref("Angel you control").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Subtype("Angel".to_string())],
                    controller: Some(ControllerRef::You),
                    properties: Vec::new(),
                }),
            }
        );
    }

    #[test]
    fn test_parse_quantity_ref_life_total() {
        let (rest, q) = parse_quantity("your life total").unwrap();
        assert_eq!(
            q,
            QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::Controller
                }
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_their_starting_life_total() {
        let (rest, q) = parse_quantity_ref("their starting life total").unwrap();
        assert_eq!(q, QuantityRef::StartingLifeTotal);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_quantity_ref_party_size_phrasings() {
        // CR 700.8: standalone party-size phrasings.
        for phrase in [
            "your party's size",
            "the size of your party",
            "the number of creatures in your party",
            "the number of creature in your party",
        ] {
            let (rest, q) = parse_quantity(phrase).unwrap();
            assert_eq!(
                q,
                QuantityExpr::Ref {
                    qty: QuantityRef::PartySize {
                        player: PlayerScope::Controller
                    }
                },
                "phrase: {phrase}"
            );
            assert_eq!(rest, "", "phrase: {phrase}");
        }
    }

    #[test]
    fn test_parse_for_each_creature_in_your_party() {
        // CR 700.8: post-"for each" form.
        let (rest, q) = parse_for_each("for each creature in your party").unwrap();
        assert_eq!(
            q,
            QuantityRef::PartySize {
                player: PlayerScope::Controller
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_for_each_object_colors_recipient_and_target() {
        for phrase in [
            "for each of its colors",
            "for each of enchanted creature's colors",
        ] {
            let (rest, q) = parse_for_each(phrase).unwrap();
            assert_eq!(
                q,
                QuantityRef::ObjectColorCount {
                    scope: crate::types::ability::ObjectScope::Recipient
                },
                "phrase: {phrase}"
            );
            assert_eq!(rest, "", "phrase: {phrase}");
        }

        let (rest, q) = parse_for_each("for each of that creature's colors").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectColorCount {
                scope: crate::types::ability::ObjectScope::Target
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_for_each_object_name_word_count_recipient_and_target() {
        let (rest, q) = parse_for_each("for each word in its name").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectNameWordCount {
                scope: crate::types::ability::ObjectScope::Recipient
            }
        );
        assert_eq!(rest, "");

        let (rest, q) = parse_for_each_clause_ref("words in that creature's name").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectNameWordCount {
                scope: crate::types::ability::ObjectScope::Target
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_for_each_mana_symbols_in_recipient_mana_cost() {
        let (rest, q) = parse_for_each("for each white mana symbol in its mana cost").unwrap();
        assert_eq!(
            q,
            QuantityRef::ManaSymbolsInManaCost {
                scope: crate::types::ability::ObjectScope::Recipient,
                color: ManaColor::White,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_number_of_object_colors() {
        let (rest, q) = parse_quantity_ref("the number of colors of target creature").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectColorCount {
                scope: crate::types::ability::ObjectScope::Target
            }
        );
        assert_eq!(rest, "");

        let (rest, q) = parse_quantity_ref("the number of colors that spell is").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectColorCount {
                scope: crate::types::ability::ObjectScope::EventSource
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_number_of_distinct_colors_among_permanents() {
        let (rest, q) =
            parse_quantity_ref("the number of colors among permanents you control").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::DistinctColorsAmongPermanents { filter } => match filter {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.type_filters, vec![TypeFilter::Permanent]);
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                }
                other => panic!("expected typed permanent filter, got {other:?}"),
            },
            other => panic!("expected DistinctColorsAmongPermanents, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_for_each_distinct_counter_kinds_among() {
        // CR 122.1: "kind of counter on permanents you control" iteration source.
        let (rest, q) =
            parse_for_each_clause_ref("kind of counter on permanents you control").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::DistinctCounterKindsAmong { filter } => match filter {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.type_filters, vec![TypeFilter::Permanent]);
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                }
                other => panic!("expected typed permanent filter, got {other:?}"),
            },
            other => panic!("expected DistinctCounterKindsAmong, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_for_each_distinct_counter_kinds_among_creatures() {
        // "among" surface form + a non-permanent type phrase.
        let (rest, q) =
            parse_for_each_clause_ref("kind of counter among creatures you control").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(q, QuantityRef::DistinctCounterKindsAmong { .. }));
    }

    #[test]
    fn test_parse_number_of_object_name_words() {
        let (rest, q) =
            parse_quantity_ref("the number of words in target creature's name").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectNameWordCount {
                scope: crate::types::ability::ObjectScope::Target
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_object_mana_value_recipient_and_target() {
        let (rest, q) = parse_quantity_ref("its mana value").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectManaValue {
                scope: crate::types::ability::ObjectScope::Recipient,
            }
        );
        assert_eq!(rest, "");

        let (rest, q) = parse_quantity_ref("that creature's converted mana cost").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectManaValue {
                scope: crate::types::ability::ObjectScope::Target,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_quantity_ref_hand_size() {
        let (rest, q) = parse_quantity_ref("cards in your hand").unwrap();
        assert_eq!(
            q,
            QuantityRef::ZoneCardCount {
                zone: ZoneRef::Hand,
                card_types: Vec::new(),
                scope: CountScope::Controller,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_quantity_ref_total_life_lost_by_opponents() {
        let (rest, q) =
            parse_quantity_ref("the total life lost by your opponents this turn").unwrap();
        assert!(matches!(
            q,
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Opponent { .. }
            }
        ));
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_recipient_controller_hand_count() {
        for phrase in [
            "card in its controller's hand",
            "cards in enchanted creature's controller's hand",
        ] {
            let (rest, q) = parse_for_each_clause_ref(phrase).unwrap();
            assert_eq!(
                q,
                QuantityRef::HandSize {
                    player: PlayerScope::RecipientController,
                },
                "phrase: {phrase}"
            );
            assert_eq!(rest, "", "phrase: {phrase}");
        }

        let (rest, q) = parse_quantity_ref("the number of cards in its controller's hand").unwrap();
        assert_eq!(
            q,
            QuantityRef::HandSize {
                player: PlayerScope::RecipientController,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_quantity_ref_total_cards_in_all_players_hands() {
        let (rest, q) =
            parse_quantity_ref("the total number of cards in all players' hands").unwrap();
        assert_eq!(
            q,
            QuantityRef::HandSize {
                player: PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Sum,
                    exclude: None,
                },
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_quantity_ref_controlled_by_fewest_player() {
        // CR 107.1: Balance's equalization minimum.
        let (rest, q) = parse_quantity_ref(
            "the number of lands controlled by the player who controls the fewest",
        )
        .unwrap();
        assert_eq!(
            q,
            QuantityRef::ControlledByEachPlayer {
                filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
                aggregate: AggregateFunction::Min,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_quantity_ref_controlled_by_most_player() {
        // The `Max` direction — "the player who controls the most".
        let (rest, q) = parse_quantity_ref(
            "the number of creatures controlled by the player who controls the most",
        )
        .unwrap();
        assert_eq!(
            q,
            QuantityRef::ControlledByEachPlayer {
                filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                aggregate: AggregateFunction::Max,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_quantity_ref_controlled_by_fewest_permanents() {
        // Balancing Act's "permanents" filter routes through the same arm.
        let (rest, q) = parse_quantity_ref(
            "the number of permanents controlled by the player who controls the fewest",
        )
        .unwrap();
        assert_eq!(
            q,
            QuantityRef::ControlledByEachPlayer {
                filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Permanent)),
                aggregate: AggregateFunction::Min,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_quantity_ref_cards_in_their_hand_is_target_zone_count() {
        // CR 109.4 + CR 115.7: "the number of cards in their hand" must resolve
        // against the effect's player target, not count every hand in the game.
        // Sword of War and Peace exemplar.
        let (rest, q) = parse_quantity_ref("the number of cards in their hand").unwrap();
        assert_eq!(
            q,
            QuantityRef::TargetZoneCardCount {
                zone: ZoneRef::Hand,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_quantity_ref_cards_in_that_players_hand_is_target_zone_count() {
        let (rest, q) = parse_quantity_ref("the number of cards in that player's hand").unwrap();
        assert_eq!(
            q,
            QuantityRef::TargetZoneCardCount {
                zone: ZoneRef::Hand,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_quantity_ref_self_power() {
        let (rest, q) = parse_quantity_ref("its power").unwrap();
        assert_eq!(
            q,
            QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Source
            }
        );
        assert_eq!(rest, "");
    }

    /// CR 400.7: Scavenge activates from the graveyard, so the source is a
    /// card. All four self-power phrasings must collapse to `SelfPower`.
    #[test]
    fn test_parse_quantity_ref_self_power_phrasings() {
        for phrase in [
            "its power",
            "~'s power",
            "this creature's power",
            "this card's power",
        ] {
            let (rest, q) = parse_quantity_ref(phrase).unwrap();
            assert_eq!(
                q,
                QuantityRef::Power {
                    scope: crate::types::ability::ObjectScope::Source
                },
                "phrase: {phrase}"
            );
            assert_eq!(rest, "", "phrase: {phrase}");
        }
    }

    #[test]
    fn test_parse_quantity_ref_graveyard() {
        let (rest, q) = parse_quantity_ref("cards in your graveyard and").unwrap();
        assert_eq!(
            q,
            QuantityRef::ZoneCardCount {
                zone: ZoneRef::Graveyard,
                card_types: Vec::new(),
                scope: CountScope::Controller,
            }
        );
        assert_eq!(rest, " and");
    }

    /// CR 604.3: `" and/or "` joins multiple type filters as a disjunction,
    /// matching cards with any of the listed types. Used by the Ghitu
    /// Lavarunner / Magmatic Channeler / Curious Homunculus class ("instant
    /// and/or sorcery cards in your graveyard").
    #[test]
    fn test_parse_quantity_ref_and_or_type_list_in_graveyard() {
        let (rest, q) =
            parse_quantity_ref("instant and/or sorcery cards in your graveyard").unwrap();
        assert_eq!(
            q,
            QuantityRef::ZoneCardCount {
                zone: ZoneRef::Graveyard,
                card_types: vec![TypeFilter::Instant, TypeFilter::Sorcery],
                scope: CountScope::Controller,
            }
        );
        assert_eq!(rest, "");
    }

    /// CR 604.3: Plain `" or "` joining is also valid in Oracle text — both
    /// forms appear historically depending on era ("instant or sorcery
    /// cards"). Resolves identically to the `and/or` form.
    #[test]
    fn test_parse_quantity_ref_or_type_list_in_graveyard() {
        let (rest, q) = parse_quantity_ref("instant or sorcery cards in your graveyard").unwrap();
        assert_eq!(
            q,
            QuantityRef::ZoneCardCount {
                zone: ZoneRef::Graveyard,
                card_types: vec![TypeFilter::Instant, TypeFilter::Sorcery],
                scope: CountScope::Controller,
            }
        );
        assert_eq!(rest, "");
    }

    /// CR 604.3: `" and "` joining for compound type lists in zone-count
    /// phrases ("artifact and creature cards in your graveyard"). Disjunction
    /// at the count level (`matches_zone_card_filter` uses `.iter().any(...)`).
    #[test]
    fn test_parse_quantity_ref_and_type_list_in_graveyard() {
        let (rest, q) =
            parse_quantity_ref("artifact and creature cards in your graveyard").unwrap();
        assert_eq!(
            q,
            QuantityRef::ZoneCardCount {
                zone: ZoneRef::Graveyard,
                card_types: vec![TypeFilter::Artifact, TypeFilter::Creature],
                scope: CountScope::Controller,
            }
        );
        assert_eq!(rest, "");
    }

    /// CR 604.3: End-to-end through `parse_inner_condition`, the path used by
    /// `parse_static_condition` for "as long as ..." gates. Pins the Ghitu
    /// Lavarunner regression at the static-condition layer.
    #[test]
    fn test_parse_inner_condition_there_are_and_or() {
        use crate::parser::oracle_nom::condition::parse_inner_condition;
        use crate::types::ability::{Comparator, StaticCondition};

        let (rest, cond) = parse_inner_condition(
            "there are two or more instant and/or sorcery cards in your graveyard",
        )
        .unwrap();
        assert_eq!(rest, "");
        match cond {
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 2 });
                match lhs {
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ZoneCardCount {
                                zone,
                                card_types,
                                scope,
                            },
                    } => {
                        assert_eq!(zone, ZoneRef::Graveyard);
                        assert_eq!(card_types, vec![TypeFilter::Instant, TypeFilter::Sorcery]);
                        assert_eq!(scope, CountScope::Controller);
                    }
                    other => panic!("expected ZoneCardCount lhs, got {other:?}"),
                }
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_quantity_ref_subtype_cards_in_graveyard() {
        let (rest, q) = parse_quantity_ref("Lesson cards in your graveyard").unwrap();
        assert_eq!(
            q,
            QuantityRef::ZoneCardCount {
                zone: ZoneRef::Graveyard,
                card_types: vec![TypeFilter::Subtype("Lesson".to_string())],
                scope: CountScope::Controller,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_opponents_total_life_lost_this_turn() {
        let (rest, q) =
            parse_quantity_ref("the total amount of life your opponents have lost this turn")
                .unwrap();
        assert_eq!(
            q,
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Sum,
                },
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_distinct_card_types_in_exile() {
        let (rest, q) =
            parse_quantity_ref("the number of card types among cards in exile").unwrap();
        assert_eq!(
            q,
            QuantityRef::DistinctCardTypes {
                source: CardTypeSetSource::Zone {
                    zone: ZoneRef::Exile,
                    scope: CountScope::All,
                },
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_distinct_card_types_exiled_with_source() {
        let (rest, q) =
            parse_quantity_ref("the number of card types among cards exiled with ~").unwrap();
        assert_eq!(
            q,
            QuantityRef::DistinctCardTypes {
                source: CardTypeSetSource::ExiledBySource,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_distinct_card_types_exiled_with_this_creature() {
        let (rest, q) =
            parse_quantity_ref("the number of card types among cards exiled with this creature")
                .unwrap();
        assert_eq!(
            q,
            QuantityRef::DistinctCardTypes {
                source: CardTypeSetSource::ExiledBySource,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_distinct_card_types_among_other_nonland_permanents() {
        let (rest, q) = parse_quantity_ref(
            "the number of card types among other nonland permanents you control",
        )
        .unwrap();
        let QuantityRef::DistinctCardTypes {
            source:
                CardTypeSetSource::Objects {
                    filter: TargetFilter::Typed(filter),
                },
        } = q
        else {
            panic!("expected object-scoped DistinctCardTypes, got {q:?}");
        };
        assert_eq!(rest, "");
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

    #[test]
    fn test_parse_number_of_cards_exiled_with_it() {
        let (rest, q) = parse_quantity_ref("the number of cards exiled with it").unwrap();
        assert_eq!(q, QuantityRef::CardsExiledBySource);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_quantity_ref_life_lost() {
        let (rest, q) = parse_quantity_ref("the life you've lost this turn").unwrap();
        assert_eq!(
            q,
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_quantity_ref_amount_of_life_gained() {
        // CR 119.3: Hope Estheim class — "the amount of life you gained this turn".
        let (rest, q) = parse_quantity_ref("the amount of life you gained this turn").unwrap();
        assert_eq!(
            q,
            QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::Controller
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_quantity_ref_amount_of_life_lost() {
        let (rest, q) = parse_quantity_ref("the amount of life you lost this turn").unwrap();
        assert_eq!(
            q,
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_quantity_failure() {
        assert!(parse_quantity("xyz").is_err());
    }

    /// CR 202.2 + CR 601.2h: "the number of colors of mana spent to cast it"
    /// resolves to `QuantityRef::ManaSpentToCast { scope: crate::types::ability::CastManaObjectScope::SelfObject, metric: crate::types::ability::CastManaSpentMetric::DistinctColors }`. Used by Wildgrowth Archaic
    /// and the cousin-card family for ETB-counter quantity expressions.
    #[test]
    fn parses_colors_spent_to_cast_it() {
        let (rest, q) =
            parse_quantity_ref("the number of colors of mana spent to cast it").unwrap();
        assert_eq!(
            q,
            QuantityRef::ManaSpentToCast {
                scope: crate::types::ability::CastManaObjectScope::SelfObject,
                metric: crate::types::ability::CastManaSpentMetric::DistinctColors
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_the_number_of_creatures() {
        let (rest, q) = parse_quantity_ref("the number of creatures you control").unwrap();
        match q {
            QuantityRef::ObjectCount { filter } => match filter {
                TargetFilter::Typed(tf) => {
                    assert!(matches!(tf.type_filters[0], TypeFilter::Creature));
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                }
                _ => panic!("expected Typed filter"),
            },
            _ => panic!("expected ObjectCount"),
        }
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_event_context_refs() {
        let (rest, q) = parse_quantity_ref("that much life").unwrap();
        assert_eq!(q, QuantityRef::EventContextAmount);
        assert_eq!(rest, " life");

        let (rest, q) = parse_quantity_ref("that damage").unwrap();
        assert_eq!(q, QuantityRef::EventContextAmount);
        assert_eq!(rest, "");

        // CR 603.7c: bare "the damage dealt" form maps to EventContextAmount.
        let (rest, q) = parse_quantity_ref("the damage dealt").unwrap();
        assert_eq!(q, QuantityRef::EventContextAmount);
        assert_eq!(rest, "");

        let (rest2, q2) = parse_quantity_ref("that creature's power").unwrap();
        assert_eq!(
            q2,
            QuantityRef::Power {
                scope: ObjectScope::CostPaidObject,
            }
        );
        assert_eq!(rest2, "");
    }

    #[test]
    fn test_parse_anaphoric_target_card_property_refs() {
        let cases = [
            (
                "that creature card's power",
                QuantityRef::Power {
                    scope: ObjectScope::Target,
                },
            ),
            (
                "that creature card's toughness",
                QuantityRef::Toughness {
                    scope: ObjectScope::Target,
                },
            ),
            (
                "that artifact card's mana value",
                QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Target,
                },
            ),
        ];

        for (input, expected) in cases {
            let (rest, qty) = parse_quantity_ref(input).unwrap();
            assert_eq!(qty, expected);
            assert_eq!(rest, "");
        }
    }

    /// CR 603.7c: Dusty Parlor — the SpellCast event's source object is the
    /// spell, so "that spell's mana value" reads its CMC via the parameterized
    /// `ObjectManaValue { scope: EventSource }` path.
    #[test]
    fn test_parse_that_spells_mana_value() {
        let (rest, q) = parse_quantity_ref("that spell's mana value").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectManaValue {
                scope: crate::types::ability::ObjectScope::EventSource
            }
        );
        assert_eq!(rest, "");
    }

    /// CR 117.1 + CR 202.3: Food Chain — "the exiled creature's mana value"
    /// resolves to the cost-paid object snapshot (NOT the trigger-event
    /// source), so the parser must emit a cost-paid-object-scoped mana value.
    #[test]
    fn test_parse_exiled_creatures_mana_value() {
        let (rest, q) = parse_quantity_ref("the exiled creature's mana value").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectManaValue {
                scope: ObjectScope::CostPaidObject
            }
        );
        assert_eq!(rest, "");
    }

    /// CR 117.1 + CR 202.3: Burnt Offering / Metamorphosis — additional
    /// sacrifice cost referenced as "the sacrificed creature's mana value".
    #[test]
    fn test_parse_sacrificed_creatures_mana_value() {
        let (rest, q) = parse_quantity_ref("the sacrificed creature's mana value").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectManaValue {
                scope: ObjectScope::CostPaidObject
            }
        );
        assert_eq!(rest, "");
    }

    /// Parser must accept the legacy "converted mana cost" phrasing.
    #[test]
    fn test_parse_sacrificed_creatures_converted_mana_cost() {
        let (rest, q) =
            parse_quantity_ref("the sacrificed creature's converted mana cost").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectManaValue {
                scope: ObjectScope::CostPaidObject
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_equal_to() {
        let (rest, q) = parse_equal_to("equal to its power").unwrap();
        assert_eq!(
            q,
            QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: crate::types::ability::ObjectScope::Source
                }
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_for_each() {
        let (rest, q) = parse_for_each("for each creature you control").unwrap();
        match q {
            QuantityRef::ObjectCount { filter } => match filter {
                TargetFilter::Typed(tf) => {
                    assert!(matches!(tf.type_filters[0], TypeFilter::Creature));
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                }
                _ => panic!("expected Typed filter"),
            },
            _ => panic!("expected ObjectCount"),
        }
        assert_eq!(rest, "");
    }

    fn assert_entered_this_turn_typed(
        q: QuantityRef,
    ) -> (Vec<TypeFilter>, Option<ControllerRef>, Vec<FilterProp>) {
        match q {
            QuantityRef::EnteredThisTurn {
                filter:
                    TargetFilter::Typed(TypedFilter {
                        type_filters,
                        controller,
                        properties,
                    }),
            } => (type_filters, controller, properties),
            other => panic!("expected typed EnteredThisTurn ref, got {other:?}"),
        }
    }

    #[test]
    fn parse_for_each_entered_this_turn_under_your_control() {
        let (rest, q) = parse_for_each_clause_ref(
            "land that entered the battlefield under your control this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        let (type_filters, controller, properties) = assert_entered_this_turn_typed(q);
        assert_eq!(type_filters, vec![TypeFilter::Land]);
        assert_eq!(controller, Some(ControllerRef::You));
        assert!(properties.is_empty());
    }

    #[test]
    fn parse_for_each_other_subtype_entered_this_turn() {
        let (rest, q) = parse_for_each_clause_ref(
            "other zombie that entered the battlefield under your control this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        let (type_filters, controller, properties) = assert_entered_this_turn_typed(q);
        assert_eq!(
            type_filters,
            vec![TypeFilter::Subtype("Zombie".to_string())]
        );
        assert_eq!(controller, Some(ControllerRef::You));
        assert!(properties.iter().any(|prop| prop == &FilterProp::Another));
    }

    #[test]
    fn parse_number_of_controlled_entered_this_turn() {
        let (rest, q) = parse_quantity_ref(
            "the number of nontoken creatures you control that entered this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        let (type_filters, controller, properties) = assert_entered_this_turn_typed(q);
        assert!(type_filters.contains(&TypeFilter::Creature));
        assert!(properties.contains(&FilterProp::NonToken));
        assert_eq!(controller, Some(ControllerRef::You));
    }

    #[test]
    fn parse_quantity_ref_tokens_created_this_turn() {
        let (rest, q) = parse_quantity_ref("the number of tokens you created this turn").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::TokensCreatedThisTurn {
                player: PlayerScope::Controller,
                filter: TargetFilter::Typed(TypedFilter { properties, .. }),
            } => assert!(properties.contains(&FilterProp::Token)),
            other => panic!("expected controller TokensCreatedThisTurn, got {other:?}"),
        }
    }

    #[test]
    fn parse_quantity_ref_treasure_tokens_created_this_turn() {
        let (rest, q) =
            parse_quantity_ref("the number of Treasure tokens you created this turn").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::TokensCreatedThisTurn {
                player: PlayerScope::Controller,
                filter:
                    TargetFilter::Typed(TypedFilter {
                        type_filters,
                        properties,
                        ..
                    }),
            } => {
                assert!(type_filters.contains(&TypeFilter::Subtype("Treasure".to_string())));
                assert!(properties.contains(&FilterProp::Token));
            }
            other => panic!("expected Treasure TokensCreatedThisTurn, got {other:?}"),
        }
    }

    fn assert_shared_quality_count_typed(
        q: QuantityRef,
    ) -> (
        Vec<TypeFilter>,
        Option<ControllerRef>,
        Vec<FilterProp>,
        SharedQuality,
        AggregateFunction,
    ) {
        match q {
            QuantityRef::ObjectCountBySharedQuality {
                filter:
                    TargetFilter::Typed(TypedFilter {
                        type_filters,
                        controller,
                        properties,
                    }),
                quality,
                aggregate,
            } => (type_filters, controller, properties, quality, aggregate),
            other => panic!("expected ObjectCountBySharedQuality over Typed, got {other:?}"),
        }
    }

    #[test]
    fn parse_greatest_creature_type_count_in_common() {
        let (rest, q) = parse_quantity_ref(
            "the greatest number of creatures you control that have a creature type in common",
        )
        .unwrap();
        assert_eq!(rest, "");
        let (type_filters, controller, properties, quality, aggregate) =
            assert_shared_quality_count_typed(q);
        assert_eq!(type_filters, vec![TypeFilter::Creature]);
        assert_eq!(controller, Some(ControllerRef::You));
        assert!(!properties
            .iter()
            .any(|p| matches!(p, FilterProp::SharesQuality { .. })));
        assert_eq!(quality, SharedQuality::CreatureType);
        assert_eq!(aggregate, AggregateFunction::Max);
    }

    #[test]
    fn parse_fewest_noncreature_shared_quality_count_in_common() {
        let (rest, q) = parse_quantity_ref(
            "the fewest number of artifacts you control that share a color in common",
        )
        .unwrap();
        assert_eq!(rest, "");
        let (type_filters, controller, _properties, quality, aggregate) =
            assert_shared_quality_count_typed(q);
        assert_eq!(type_filters, vec![TypeFilter::Artifact]);
        assert_eq!(controller, Some(ControllerRef::You));
        assert_eq!(quality, SharedQuality::Color);
        assert_eq!(aggregate, AggregateFunction::Min);
    }

    #[test]
    fn parse_singular_at_least_one_shared_quality_count_in_common() {
        let (rest, q) = parse_quantity_ref(
            "the greatest number of permanent you control that has at least one color in common",
        )
        .unwrap();
        assert_eq!(rest, "");
        let (type_filters, controller, _properties, quality, aggregate) =
            assert_shared_quality_count_typed(q);
        assert_eq!(type_filters, vec![TypeFilter::Permanent]);
        assert_eq!(controller, Some(ControllerRef::You));
        assert_eq!(quality, SharedQuality::Color);
        assert_eq!(aggregate, AggregateFunction::Max);
    }

    #[test]
    fn parse_total_shared_quality_count_in_common() {
        let (rest, q) = parse_quantity_ref(
            "the total number of permanents you control that have a card type in common",
        )
        .unwrap();
        assert_eq!(rest, "");
        let (type_filters, controller, _properties, quality, aggregate) =
            assert_shared_quality_count_typed(q);
        assert_eq!(type_filters, vec![TypeFilter::Permanent]);
        assert_eq!(controller, Some(ControllerRef::You));
        assert_eq!(quality, SharedQuality::CardType);
        assert_eq!(aggregate, AggregateFunction::Sum);
    }

    #[test]
    fn parse_shared_quality_count_rejects_partial_population() {
        assert!(parse_quantity_ref(
            "the greatest number of creatures you control banana that have a creature type in common",
        )
        .is_err());
    }

    #[test]
    fn parse_shared_quality_count_rejects_empty_population() {
        assert!(parse_quantity_ref(
            "the greatest number of you control that have a creature type in common",
        )
        .is_err());
    }

    /// Helper: pull the `(type_filters, controller, properties, qualities)` tuple
    /// out of a `QuantityRef::ObjectCountDistinct` over a `TargetFilter::Typed`.
    /// Panics on any other shape so tests fail loudly on misroutes.
    fn assert_distinct_named_typed(
        q: QuantityRef,
    ) -> (
        Vec<TypeFilter>,
        Option<ControllerRef>,
        Vec<FilterProp>,
        Vec<SharedQuality>,
    ) {
        match q {
            QuantityRef::ObjectCountDistinct {
                filter:
                    TargetFilter::Typed(TypedFilter {
                        type_filters,
                        controller,
                        properties,
                    }),
                qualities,
            } => (type_filters, controller, properties, qualities),
            other => panic!("expected ObjectCountDistinct over Typed, got {other:?}"),
        }
    }

    #[test]
    fn parse_quantity_ref_differently_named_artifact_tokens_you_control() {
        // Gimbal, Gremlin Prodigy / Sandsteppe War Riders shape.
        let (rest, q) =
            parse_quantity_ref("the number of differently named artifact tokens you control")
                .unwrap();
        assert_eq!(rest, "");
        let (type_filters, controller, properties, qualities) = assert_distinct_named_typed(q);
        assert!(type_filters.contains(&TypeFilter::Artifact));
        assert_eq!(controller, Some(ControllerRef::You));
        assert!(properties.contains(&FilterProp::Token));
        assert_eq!(qualities, vec![SharedQuality::Name]);
    }

    #[test]
    fn parse_quantity_ref_differently_named_lands_you_control() {
        // Awakened Amalgam / All-Fates Scroll / Fungal Colossus shape.
        let (rest, q) =
            parse_quantity_ref("the number of differently named lands you control").unwrap();
        assert_eq!(rest, "");
        let (type_filters, controller, properties, qualities) = assert_distinct_named_typed(q);
        assert!(type_filters.contains(&TypeFilter::Land));
        assert_eq!(controller, Some(ControllerRef::You));
        assert!(!properties.contains(&FilterProp::Token));
        assert_eq!(qualities, vec![SharedQuality::Name]);
    }

    #[test]
    fn parse_quantity_ref_differently_named_creature_tokens_you_control() {
        // Audience with Trostani shape.
        let (rest, q) =
            parse_quantity_ref("the number of differently named creature tokens you control")
                .unwrap();
        assert_eq!(rest, "");
        let (type_filters, controller, properties, qualities) = assert_distinct_named_typed(q);
        assert!(type_filters.contains(&TypeFilter::Creature));
        assert_eq!(controller, Some(ControllerRef::You));
        assert!(properties.contains(&FilterProp::Token));
        assert_eq!(qualities, vec![SharedQuality::Name]);
    }

    #[test]
    fn parse_quantity_ref_differently_named_tokens_you_control() {
        // Neriv, Crackling Vanguard shape — bare "tokens" (any card type).
        let (rest, q) =
            parse_quantity_ref("the number of differently named tokens you control").unwrap();
        assert_eq!(rest, "");
        let (_type_filters, controller, properties, qualities) = assert_distinct_named_typed(q);
        assert_eq!(controller, Some(ControllerRef::You));
        assert!(properties.contains(&FilterProp::Token));
        assert_eq!(qualities, vec![SharedQuality::Name]);
    }

    #[test]
    fn parse_for_each_subtype_that_died_this_turn() {
        let (rest, q) = parse_for_each_clause_ref("zubera that died this turn").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            q,
            QuantityRef::ZoneChangeCountThisTurn {
                from: Some(Zone::Battlefield),
                to: Some(Zone::Graveyard),
                filter: TargetFilter::Typed(TypedFilter {
                    ref type_filters,
                    ..
                }),
            } if type_filters.contains(&TypeFilter::Creature)
                && type_filters.contains(&TypeFilter::Subtype("Zubera".to_string()))
        ));
    }

    #[test]
    fn test_parse_for_each_creature_blocking_it() {
        let (rest, q) = parse_for_each("for each creature blocking it").unwrap();
        match q {
            QuantityRef::ObjectCount { filter } => match filter {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                    assert_eq!(tf.controller, None);
                    assert_eq!(tf.properties, vec![FilterProp::BlockingSource]);
                }
                _ => panic!("expected Typed filter"),
            },
            _ => panic!("expected ObjectCount"),
        }
        assert_eq!(rest, "");

        let (rest, q) = parse_for_each("for each creature blocking ~").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            q,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    properties,
                    ..
                })
            } if properties == vec![FilterProp::BlockingSource]
        ));
    }

    #[test]
    fn test_parse_for_each_attacking_creature_other_than_source() {
        let (rest, q) = parse_for_each("for each attacking creature other than ~").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            q,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller: None,
                    properties,
                    ..
                })
            } if type_filters == vec![TypeFilter::Creature]
                && properties == vec![FilterProp::Attacking, FilterProp::Another]
        ));
    }

    #[test]
    fn test_parse_for_each_attacking_creature_they_control() {
        let (rest, q) = parse_for_each("for each attacking creature they control").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            q,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters,
                    properties,
                    ..
                })
            } if type_filters == vec![TypeFilter::Creature]
                && properties == vec![FilterProp::Attacking]
        ));
    }

    #[test]
    fn test_parse_for_each_attacking_creature_they_control_uses_context() {
        let ctx = ParseContext {
            relative_player_scope: Some(ControllerRef::TargetPlayer),
            ..Default::default()
        };
        let (rest, q) =
            parse_for_each_clause_ref_with_context("attacking creature they control", &ctx)
                .unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            q,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::TargetPlayer),
                    ..
                })
            }
        ));
    }

    #[test]
    fn test_parse_for_each_attacking_creature_you_control_ignores_they_context() {
        let ctx = ParseContext {
            relative_player_scope: Some(ControllerRef::TargetPlayer),
            ..Default::default()
        };
        let (rest, q) =
            parse_for_each_clause_ref_with_context("attacking creature you control", &ctx).unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            q,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::You),
                    ..
                })
            }
        ));
    }

    #[test]
    fn test_parse_half_permanents_they_control_uses_scoped_player() {
        let (rest, q) = parse_half_rounded("half the permanents they control").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            q,
            QuantityExpr::DivideRounded {
                inner,
                ..
            } if matches!(
                *inner,
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(TypedFilter {
                            controller: Some(ControllerRef::ScopedPlayer),
                            ..
                        })
                    }
                }
            )
        ));
    }

    #[test]
    fn test_parse_half_non_demon_permanents_you_control_preserves_full_filter() {
        let (rest, q) =
            parse_half_rounded("half the non-Demon permanents you control, rounded up").unwrap();
        assert_eq!(rest, "");
        let QuantityExpr::DivideRounded {
            inner,
            divisor: 2,
            rounding: RoundingMode::Up,
        } = q
        else {
            panic!("expected DivideRounded(Up), got {q:?}");
        };
        let QuantityExpr::Ref {
            qty:
                QuantityRef::ObjectCount {
                    filter:
                        TargetFilter::Typed(TypedFilter {
                            type_filters,
                            controller: Some(ControllerRef::You),
                            ..
                        }),
                },
        } = *inner
        else {
            panic!("expected ObjectCount with You controller");
        };
        assert_eq!(
            type_filters,
            vec![
                TypeFilter::Permanent,
                TypeFilter::Non(Box::new(TypeFilter::Subtype("Demon".to_string()))),
            ]
        );
    }

    #[test]
    fn test_parse_half_non_god_creatures_they_control_preserves_scoped_filter() {
        let (rest, q) =
            parse_half_rounded("half the non-God creatures they control, rounded down").unwrap();
        assert_eq!(rest, "");
        let QuantityExpr::DivideRounded {
            inner,
            divisor: 2,
            rounding: RoundingMode::Down,
        } = q
        else {
            panic!("expected DivideRounded(Down), got {q:?}");
        };
        let QuantityExpr::Ref {
            qty:
                QuantityRef::ObjectCount {
                    filter:
                        TargetFilter::Typed(TypedFilter {
                            type_filters,
                            controller: Some(ControllerRef::ScopedPlayer),
                            ..
                        }),
                },
        } = *inner
        else {
            panic!("expected ObjectCount with ScopedPlayer controller");
        };
        assert_eq!(
            type_filters,
            vec![
                TypeFilter::Creature,
                TypeFilter::Non(Box::new(TypeFilter::Subtype("God".to_string()))),
            ]
        );
    }

    #[test]
    fn test_parse_third_and_tenth_object_fractions() {
        let (rest, third) =
            parse_fraction_rounded("a third of the lands they control, rounded down").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            third,
            QuantityExpr::DivideRounded {
                divisor: 3,
                rounding: RoundingMode::Down,
                ..
            }
        ));

        let (rest, tenth) =
            parse_fraction_rounded("a tenth of the creatures they control, rounded up").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            tenth,
            QuantityExpr::DivideRounded {
                divisor: 10,
                rounding: RoundingMode::Up,
                ..
            }
        ));
    }

    #[test]
    fn test_parse_for_each_blocking_creatures_other_than_this_creature() {
        let (rest, q) =
            parse_for_each("for each blocking creatures other than this creature").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            q,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller: None,
                    properties,
                    ..
                })
            } if type_filters == vec![TypeFilter::Creature]
                && properties == vec![FilterProp::Blocking, FilterProp::Another]
        ));
    }

    #[test]
    fn test_parse_devotion() {
        let (rest, q) = parse_quantity_ref("your devotion to red").unwrap();
        assert_eq!(
            q,
            QuantityRef::Devotion {
                colors: DevotionColors::Fixed(vec![ManaColor::Red])
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_devotion_chosen_color() {
        let (rest, q) = parse_quantity_ref("your devotion to that color").unwrap();
        assert_eq!(
            q,
            QuantityRef::Devotion {
                colors: DevotionColors::ChosenColor
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_devotion_multicolor() {
        let (rest, q) = parse_quantity_ref("your devotion to white and black").unwrap();
        assert_eq!(
            q,
            QuantityRef::Devotion {
                colors: DevotionColors::Fixed(vec![ManaColor::White, ManaColor::Black])
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_target_power() {
        let (rest, q) = parse_quantity_ref("target creature's power").unwrap();
        assert_eq!(
            q,
            QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Target
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_basic_land_type_count() {
        let (rest, q) =
            parse_quantity_ref("the number of basic land types among lands you control").unwrap();
        assert_eq!(
            q,
            QuantityRef::BasicLandTypeCount {
                controller: ControllerRef::You,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_basic_land_type_count_singular_for_each_suffix() {
        let (rest, q) = parse_quantity_ref("basic land type among lands you control").unwrap();
        assert_eq!(
            q,
            QuantityRef::BasicLandTypeCount {
                controller: ControllerRef::You,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_basic_land_type_count_target_player_suffix() {
        let (rest, q) = parse_quantity_ref("basic land type among lands they control").unwrap();
        assert_eq!(
            q,
            QuantityRef::BasicLandTypeCount {
                controller: ControllerRef::TargetPlayer,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_for_each_commander_cast_count() {
        let (rest, q) = parse_for_each_clause_ref(
            "times you've cast your commander from the command zone this game",
        )
        .unwrap();
        assert_eq!(q, QuantityRef::CommanderCastFromCommandZoneCount);
        assert_eq!(rest, "");

        let (rest, q) = parse_for_each_clause_ref(
            "time youve cast your commander from the command zone this game",
        )
        .unwrap();
        assert_eq!(q, QuantityRef::CommanderCastFromCommandZoneCount);
        assert_eq!(rest, "");
    }

    // --- Half-rounded fractional expressions (CR 107.1a) ---

    #[test]
    fn test_parse_half_their_library_rounded_down() {
        let (rest, q) = parse_quantity("half their library, rounded down").unwrap();
        assert_eq!(
            q,
            QuantityExpr::DivideRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::TargetZoneCardCount {
                        zone: ZoneRef::Library,
                    },
                }),
                divisor: 2,
                rounding: RoundingMode::Down,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_half_their_life_rounded_up() {
        let (rest, q) = parse_quantity("half their life, rounded up").unwrap();
        assert_eq!(
            q,
            QuantityExpr::DivideRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Target
                    },
                }),
                divisor: 2,
                rounding: RoundingMode::Up,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_half_their_life_total_rounded_up() {
        let (rest, q) = parse_quantity("half their life total, rounded up").unwrap();
        assert_eq!(
            q,
            QuantityExpr::DivideRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Target
                    },
                }),
                divisor: 2,
                rounding: RoundingMode::Up,
            }
        );
        assert_eq!(rest, "");
    }

    /// CR 400.7: "its power" resolves to the source object's power via
    /// `SelfPower`. "half its power" composes over the existing ref.
    #[test]
    fn test_parse_half_its_power_rounded_up() {
        let (rest, q) = parse_quantity("half its power, rounded up").unwrap();
        assert_eq!(
            q,
            QuantityExpr::DivideRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: crate::types::ability::ObjectScope::Source
                    },
                }),
                divisor: 2,
                rounding: RoundingMode::Up,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_half_your_life_rounded_up() {
        let (rest, q) = parse_quantity("half your life, rounded up").unwrap();
        assert_eq!(
            q,
            QuantityExpr::DivideRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Controller
                    },
                }),
                divisor: 2,
                rounding: RoundingMode::Up,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_half_your_library_rounded_up() {
        let (rest, q) = parse_quantity("half your library, rounded up").unwrap();
        assert_eq!(
            q,
            QuantityExpr::DivideRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Library,
                        card_types: Vec::new(),
                        scope: CountScope::Controller,
                    },
                }),
                divisor: 2,
                rounding: RoundingMode::Up,
            }
        );
        assert_eq!(rest, "");
    }

    /// Legacy Oracle text for life-loss cards used "his or her life" before
    /// the 2014 "their" reword. Resolves to the same `TargetLifeTotal` ref.
    #[test]
    fn test_parse_half_his_or_her_life_rounded_up() {
        let (rest, q) = parse_quantity("half his or her life, rounded up").unwrap();
        assert_eq!(
            q,
            QuantityExpr::DivideRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Target
                    },
                }),
                divisor: 2,
                rounding: RoundingMode::Up,
            }
        );
        assert_eq!(rest, "");
    }

    /// CR 107.1a: Oracle text must specify rounding. When absent (duration
    /// stripped upstream, or malformed text), we fall back to `Down`.
    #[test]
    fn test_parse_half_default_rounding_is_down() {
        let (rest, q) = parse_quantity("half their library").unwrap();
        assert_eq!(
            q,
            QuantityExpr::DivideRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::TargetZoneCardCount {
                        zone: ZoneRef::Library,
                    },
                }),
                divisor: 2,
                rounding: RoundingMode::Down,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_half_round_up_variant() {
        // "round up" variant (no "-ed") — less common but present in some text.
        let (rest, q) = parse_quantity("half their life, round up").unwrap();
        assert_eq!(
            q,
            QuantityExpr::DivideRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Target
                    },
                }),
                divisor: 2,
                rounding: RoundingMode::Up,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_half_preserves_trailing_text() {
        // After the rounding suffix, remaining text should be passed through
        // unchanged so callers can consume it (e.g., the period at end-of-line).
        let (rest, q) = parse_quantity("half their library, rounded down.").unwrap();
        assert!(matches!(q, QuantityExpr::DivideRounded { .. }));
        assert_eq!(rest, ".");
    }

    #[test]
    fn test_parse_possessive_ref_their_hand() {
        let (rest, q) = parse_possessive_quantity_ref("their hand").unwrap();
        assert_eq!(
            q,
            QuantityRef::TargetZoneCardCount {
                zone: ZoneRef::Hand,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_possessive_ref_your_hand() {
        let (rest, q) = parse_possessive_quantity_ref("your hand").unwrap();
        assert_eq!(
            q,
            QuantityRef::ZoneCardCount {
                zone: ZoneRef::Hand,
                card_types: Vec::new(),
                scope: CountScope::Controller,
            }
        );
        assert_eq!(rest, "");
    }

    /// CR 122.1: typed player-counter quantity refs cover every kind × scope
    /// permutation through composed nom alts (no string permutation matrix).
    #[test]
    fn parses_player_counter_ref_for_each_kind_and_scope() {
        let cases: &[(&str, PlayerCounterKind, CountScope)] = &[
            (
                "the number of experience counters you have",
                PlayerCounterKind::Experience,
                CountScope::Controller,
            ),
            (
                "the number of poison counters you have",
                PlayerCounterKind::Poison,
                CountScope::Controller,
            ),
            (
                "the number of rad counters you have",
                PlayerCounterKind::Rad,
                CountScope::Controller,
            ),
            (
                "the number of ticket counters you have",
                PlayerCounterKind::Ticket,
                CountScope::Controller,
            ),
            (
                "the number of experience counters each opponent has",
                PlayerCounterKind::Experience,
                CountScope::Opponents,
            ),
            (
                "the number of poison counters each player has",
                PlayerCounterKind::Poison,
                CountScope::All,
            ),
            (
                "the number of poison counters that player has",
                PlayerCounterKind::Poison,
                CountScope::ScopedPlayer,
            ),
            (
                "the number of rad counters that player has",
                PlayerCounterKind::Rad,
                CountScope::ScopedPlayer,
            ),
        ];
        for (phrase, kind, scope) in cases {
            let (rest, q) = parse_quantity_ref(phrase).unwrap_or_else(|e| {
                panic!("phrase `{phrase}` failed to parse: {e:?}");
            });
            assert_eq!(
                q,
                QuantityRef::PlayerCounter {
                    kind: *kind,
                    scope: scope.clone(),
                },
                "{phrase}"
            );
            assert_eq!(rest, "", "{phrase}");
        }
    }

    /// CR 122.1: the public entry point accepts the full "the number of …"
    /// phrase so the imperative-side `parse_earthbend_count_expr` can hook in.
    #[test]
    fn parses_player_counter_via_public_entry_point() {
        let (rest, q) =
            parse_the_number_of_player_counters("the number of experience counters you have")
                .unwrap();
        assert_eq!(
            q,
            QuantityRef::PlayerCounter {
                kind: PlayerCounterKind::Experience,
                scope: CountScope::Controller,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parses_player_counter_for_each_singular_and_plural() {
        let cases: &[(&str, PlayerCounterKind, CountScope)] = &[
            (
                "experience counter you have",
                PlayerCounterKind::Experience,
                CountScope::Controller,
            ),
            (
                "rad counters each opponent has",
                PlayerCounterKind::Rad,
                CountScope::Opponents,
            ),
            (
                "poison counter your opponents have",
                PlayerCounterKind::Poison,
                CountScope::Opponents,
            ),
        ];
        for (phrase, kind, scope) in cases {
            let (rest, q) = parse_for_each_clause_ref(phrase).unwrap_or_else(|e| {
                panic!("for-each phrase `{phrase}` failed to parse: {e:?}");
            });
            assert_eq!(
                q,
                QuantityRef::PlayerCounter {
                    kind: *kind,
                    scope: scope.clone(),
                },
                "{phrase}"
            );
            assert_eq!(rest, "", "{phrase}");
        }
    }

    #[test]
    fn test_parse_linked_exile_mana_value_ref() {
        for phrase in [
            "the mana value of the exiled card",
            "the converted mana cost of the exiled card",
            "the exiled card's mana value",
            "the exiled card's converted mana cost",
        ] {
            let (rest, q) = parse_quantity_ref(phrase).unwrap();
            assert_eq!(rest, "");
            assert_eq!(
                q,
                QuantityRef::Aggregate {
                    function: AggregateFunction::Sum,
                    property: ObjectProperty::ManaValue,
                    filter: TargetFilter::And {
                        filters: vec![
                            TargetFilter::ExiledBySource,
                            TargetFilter::Typed(TypedFilter::default().properties(vec![
                                FilterProp::Owned {
                                    controller: ControllerRef::You,
                                },
                            ])),
                        ],
                    },
                }
            );
        }
    }

    /// CR 701.17a + CR 701.17c: "the milled card's mana value" routes through
    /// `parse_cost_paid_object_ref` (participle = "milled") and yields
    /// `ObjectManaValue { CostPaidObject }`. Covers Heed the Mists and the
    /// broader class of "milled card's <property>" CDA patterns.
    #[test]
    fn test_parse_milled_card_mana_value_ref() {
        for phrase in [
            "the milled card's mana value",
            "the milled card's converted mana cost",
            "milled card's mana value",
        ] {
            let (rest, q) = parse_quantity_ref(phrase)
                .unwrap_or_else(|_| panic!("parse_quantity_ref({phrase:?}) should succeed"));
            assert_eq!(rest, "", "phrase {phrase:?} should be fully consumed");
            assert_eq!(
                q,
                QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject,
                },
                "phrase {phrase:?} must yield ObjectManaValue{{CostPaidObject}}"
            );
        }
    }

    /// CR 700.12: "the number of outlaws you control" counts every permanent
    /// with an outlaw creature type (Assassin/Mercenary/Pirate/Rogue/Warlock).
    /// Laughing Jasper Flint. Routes through `parse_number_of_controlled_type`
    /// once `parse_type_filter_word` recognizes the "outlaws" head noun.
    #[test]
    fn parse_quantity_ref_the_number_of_outlaws_you_control() {
        let outlaws = TypeFilter::AnyOf(
            ["Assassin", "Mercenary", "Pirate", "Rogue", "Warlock"]
                .iter()
                .map(|s| TypeFilter::Subtype((*s).to_string()))
                .collect(),
        );
        let (rest, q) = parse_quantity_ref("the number of outlaws you control").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![outlaws],
                    controller: Some(ControllerRef::You),
                    properties: Vec::new(),
                }),
            }
        );
    }

    /// CR 205.2b: "permanents you control that are creatures and/or Vehicles"
    /// restricts the controlled population to the listed types, merged into an
    /// `AnyOf` disjunction so a creature-Vehicle is counted once. Collision
    /// Course.
    #[test]
    fn parse_quantity_ref_controlled_type_disjunction_clause() {
        let (rest, q) = parse_quantity_ref(
            "the number of permanents you control that are creatures and/or vehicles",
        )
        .unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::AnyOf(vec![
                        TypeFilter::Creature,
                        TypeFilter::Subtype("Vehicle".to_string()),
                    ])],
                    controller: Some(ControllerRef::You),
                    properties: Vec::new(),
                }),
            }
        );
    }

    /// Regression: a plain controlled-type count without a "that are" clause
    /// keeps the single head type.
    #[test]
    fn parse_quantity_ref_controlled_type_no_clause_keeps_head() {
        let (rest, q) = parse_quantity_ref("the number of creatures you control").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: Some(ControllerRef::You),
                    properties: Vec::new(),
                }),
            }
        );
    }

    /// A single-type "that are" clause replaces the head with that one type
    /// (no `AnyOf` wrapper).
    #[test]
    fn parse_quantity_ref_controlled_type_single_clause() {
        let (rest, q) =
            parse_quantity_ref("the number of permanents you control that are artifacts").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Artifact],
                    controller: Some(ControllerRef::You),
                    properties: Vec::new(),
                }),
            }
        );
    }

    /// A non-type "that are" clause (e.g. "that are tapped") must NOT be
    /// consumed by the optional type-list clause — the `opt` returns `None` and
    /// the count keeps the head type, leaving the clause for a later parser.
    #[test]
    fn parse_quantity_ref_controlled_type_non_type_clause_falls_through() {
        let (rest, q) =
            parse_number_of_controlled_type("creatures you control that are tapped").unwrap();
        assert_eq!(rest, " that are tapped");
        assert_eq!(
            q,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: Some(ControllerRef::You),
                    properties: Vec::new(),
                }),
            }
        );
    }
}
