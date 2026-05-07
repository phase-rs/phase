//! Condition combinators for Oracle text parsing.
//!
//! Parses condition phrases: "if [condition]", "as long as [condition]",
//! "unless [condition]" into typed `StaticCondition` values.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::bytes::complete::take_until;
use nom::combinator::{map, opt, value};
use nom::sequence::preceded;
use nom::Parser;

use super::error::{OracleError, OracleResult};
use super::primitives::{parse_article, parse_color, parse_mana_cost, parse_number};
use super::quantity as nom_quantity;
use crate::parser::oracle_target::parse_type_phrase;
use crate::types::ability::{
    AggregateFunction, CastManaObjectScope, CastManaSpentMetric, Comparator, ControllerRef,
    CountScope, FilterProp, ObjectProperty, PlayerScope, QuantityExpr, QuantityRef,
    StaticCondition, TargetFilter, TypeFilter, TypedFilter, ZoneRef,
};
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::game_state::DayNight;
use crate::types::zones::Zone;

/// Parse a condition phrase from Oracle text.
///
/// Matches patterns like "if you control a creature", "as long as you have no
/// cards in hand", "unless an opponent controls a creature".
pub fn parse_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        preceded(tuple_ws_tag("if "), parse_inner_condition),
        preceded(tuple_ws_tag("as long as "), parse_inner_condition),
        preceded(tuple_ws_tag("unless "), parse_unless_condition),
    ))
    .parse(input)
}

/// Parse an "if" or "as long as" condition without the prefix keyword.
///
/// Useful when the prefix has already been consumed by the caller.
pub fn parse_inner_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        parse_turn_conditions,
        parse_source_state_conditions,
        parse_player_state_conditions,
        parse_you_have_conditions,
        parse_compound_control_presence,
        parse_filter_have_total_property,
        parse_control_conditions,
        parse_opponent_poison_conditions,
        parse_defending_player_comparison_conditions,
        parse_opponent_comparison_conditions,
        parse_life_conditions,
        parse_zone_conditions,
        parse_there_are_counters_on_source,
        parse_there_are_conditions,
        parse_there_exists_condition,
        parse_subject_first_zone_count,
        parse_entered_this_turn,
        parse_opponent_cast_spell_this_turn,
        parse_youve_this_turn,
        parse_event_state_conditions,
    ))
    .or(alt((
        parse_source_qualified_mana_spent_condition,
        parse_source_qualified_mana_spent_threshold,
        parse_mana_spent_vs_source_pt,
        parse_mana_spent_threshold,
        parse_combat_context_conditions,
        parse_unless_pay_condition,
    )))
    .parse(input)
}

/// CR 603.4: Parse "you control a/an [type] and a/an [type]" as a compound
/// presence check. This keeps two independent control predicates composable
/// instead of hard-coding card text such as "artifact and enchantment".
fn parse_compound_control_presence(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you control ").parse(input)?;
    let (rest, first) = parse_control_presence_tail(rest)?;
    let (rest, _) = tag(" and ").parse(rest)?;
    let (rest, second) = parse_control_presence_tail(rest)?;
    Ok((
        rest,
        StaticCondition::And {
            conditions: vec![first, second],
        },
    ))
}

fn parse_control_presence_tail(input: &str) -> OracleResult<'_, StaticCondition> {
    let _ = alt((parse_article, value((), tag("another ")))).parse(input)?;

    let (filter, remainder) = parse_type_phrase(input);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let filter = inject_controller_you(filter);
    let consumed = input.len() - remainder.len();
    Ok((
        &input[consumed..],
        StaticCondition::IsPresent {
            filter: Some(filter),
        },
    ))
}

/// Helper: tag with potential leading whitespace trimmed.
fn tuple_ws_tag(t: &str) -> impl FnMut(&str) -> OracleResult<'_, &str> + '_ {
    move |input: &str| tag(t).parse(input)
}

/// Parse turn-based conditions.
fn parse_turn_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        value(StaticCondition::DuringYourTurn, tag("it's your turn")),
        value(StaticCondition::DuringYourTurn, tag("it is your turn")),
        // "it's not your turn" → Not(DuringYourTurn)
        map(tag("it's not your turn"), |_| StaticCondition::Not {
            condition: Box::new(StaticCondition::DuringYourTurn),
        }),
        parse_day_night_condition,
    ))
    .parse(input)
}

/// CR 725.1 / CR 702.131a: Parse player-state conditions.
///
/// Handles "you're the monarch" and "you have the city's blessing".
fn parse_player_state_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        // CR 725.1: Monarch status
        value(
            StaticCondition::IsMonarch,
            alt((tag("you're the monarch"), tag("you are the monarch"))),
        ),
        // CR 702.131a: Ascend / City's Blessing
        value(
            StaticCondition::HasCityBlessing,
            tag("you have the city's blessing"),
        ),
        // CR 702.178a / CR 702.179f: Speed conditions.
        value(
            StaticCondition::HasMaxSpeed,
            alt((tag("you have max speed"), tag("have max speed"))),
        ),
        map(
            alt((tag("you don't have max speed"), tag("don't have max speed"))),
            |_| StaticCondition::Not {
                condition: Box::new(StaticCondition::HasMaxSpeed),
            },
        ),
        parse_speed_threshold_condition,
        // CR 309.7: Dungeon completion
        value(
            StaticCondition::CompletedADungeon,
            tag("you've completed a dungeon"),
        ),
        // CR 903.3: Commander control (Lieutenant mechanic)
        value(
            StaticCondition::ControlsCommander,
            alt((
                tag("you control your commander"),
                tag("you control a commander"),
            )),
        ),
    ))
    .parse(input)
}

fn parse_speed_threshold_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("your speed is ").parse(input)?;
    let (rest, threshold) = parse_number(rest)?;
    let (rest, _) = tag(" or higher").parse(rest)?;
    Ok((
        rest,
        StaticCondition::SpeedGE {
            threshold: u8::try_from(threshold).map_err(|_| {
                nom::Err::Error(nom::error::Error::new(rest, nom::error::ErrorKind::Fail))
            })?,
        },
    ))
}

fn parse_opponent_poison_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("an opponent has ").parse(input)?;
    let (rest, count) = parse_number(rest)?;
    let (rest, _) = tag(" or more poison counters").parse(rest)?;
    Ok((rest, StaticCondition::OpponentPoisonAtLeast { count }))
}

/// Shared subject dispatcher for source-referential predicates.
///
/// Consumes `"<subject> "` — the trailing `"is"` / `"isn't"` is dispatched by the
/// caller so negation (`"~ isn't attacking"`) composes cleanly.
///
/// Subjects: "~", "this creature", "this permanent", "this land", "this artifact",
/// "this enchantment", "equipped creature", "enchanted creature".
fn parse_source_subject(input: &str) -> OracleResult<'_, &str> {
    alt((
        tag("~ "),
        tag("this creature "),
        tag("this permanent "),
        tag("this land "),
        tag("this artifact "),
        tag("this enchantment "),
        tag("equipped creature "),
        tag("enchanted creature "),
    ))
    .parse(input)
}

/// CR 611.2b: Compose subject × predicate for tapped/untapped.
///
/// Predicate: "tapped" → SourceIsTapped, "untapped" → Not(SourceIsTapped).
/// Only the affirmative `"is"` form is produced in Oracle text for tapped/untapped
/// (both are themselves past participles — there is no `"isn't tapped"` idiom),
/// so we only dispatch `tag("is ")` here. Negation patterns live in
/// `parse_combat_state_predicate`.
fn parse_tapped_untapped(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_source_subject(input)?;
    let (rest, _) = tag("is ").parse(rest)?;
    alt((
        value(StaticCondition::SourceIsTapped, tag("tapped")),
        value(
            StaticCondition::Not {
                condition: Box::new(StaticCondition::SourceIsTapped),
            },
            tag("untapped"),
        ),
    ))
    .parse(rest)
}

/// CR 508.1k / CR 509.1g / CR 509.1h: Parse subject × combat-state predicate.
///
/// Composes `parse_source_subject` with:
/// - `"is"` / `"isn't"` for affirmative vs negated predicate,
/// - one of `"attacking or blocking"` (longest-match first) / `"attacking"` /
///   `"blocking"` / `"blocked"`.
///
/// `"attacking or blocking"` emits `Or([SourceIsAttacking, SourceIsBlocking])`
/// via the existing `StaticCondition::Or` combinator — no dedicated variant.
fn parse_combat_state_predicate(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_source_subject(input)?;
    let (rest, negated) =
        alt((value(false, tag("is ")), value(true, tag("isn't ")))).parse(rest)?;
    let (rest, predicate) = alt((
        // Longest-match first — nom's `alt` is first-match.
        map(tag("attacking or blocking"), |_| StaticCondition::Or {
            conditions: vec![
                StaticCondition::SourceIsAttacking,
                StaticCondition::SourceIsBlocking,
            ],
        }),
        value(StaticCondition::SourceIsAttacking, tag("attacking")),
        value(StaticCondition::SourceIsBlocking, tag("blocking")),
        value(StaticCondition::SourceIsBlocked, tag("blocked")),
    ))
    .parse(rest)?;
    let result = if negated {
        StaticCondition::Not {
            condition: Box::new(predicate),
        }
    } else {
        predicate
    };
    Ok((rest, result))
}

/// CR 301.5a: Parse "<subject> is equipped" → SourceIsEquipped.
fn parse_source_is_equipped(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_source_subject(input)?;
    value(StaticCondition::SourceIsEquipped, tag("is equipped")).parse(rest)
}

/// CR 701.37: Parse "<subject> is monstrous" → SourceIsMonstrous.
fn parse_source_is_monstrous(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_source_subject(input)?;
    value(StaticCondition::SourceIsMonstrous, tag("is monstrous")).parse(rest)
}

/// CR 301.5 + CR 303.4: Parse "<subject> is attached to a creature" → SourceAttachedToCreature.
fn parse_source_attached_to_creature(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_source_subject(input)?;
    value(
        StaticCondition::SourceAttachedToCreature,
        tag("is attached to a creature"),
    )
    .parse(rest)
}

/// CR 611.2b: Parse source-state conditions (tapped, untapped, entered this turn).
fn parse_source_state_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        // CR 611.2b: Tapped/untapped — composed as subject × predicate.
        // Parse subject ("~ is", "this creature is", etc.) then branch on "tapped"/"untapped".
        parse_tapped_untapped,
        // CR 508.1k / CR 509.1g / CR 509.1h: Combat-state predicates —
        // "is attacking" / "is blocking" / "is blocked" / "is attacking or blocking"
        // and their negations ("isn't attacking", etc.).
        parse_combat_state_predicate,
        // CR 301.5a: "~ is equipped" / "this creature is equipped" / etc.
        parse_source_is_equipped,
        // CR 701.37: "~ is monstrous" / "this creature is monstrous" / etc.
        parse_source_is_monstrous,
        // CR 301.5 + CR 303.4: "~ is attached to a creature" / "this equipment is attached to a creature".
        // Must precede `parse_source_is_type` so the specific "is attached to a creature"
        // predicate wins over generic "is <type>" dispatch.
        parse_source_attached_to_creature,
        // CR 122.1: "<subject> has <quantity> <counter_type> counter(s) on it"
        // — covers Unleash/Outlast/Renown bodies, Primordial Hydra's trample gate,
        // and every "as long as it has …" counter-comparator static.
        // Must precede `parse_source_is_type` so "has … counters on it" wins over
        // any other interpretation.
        parse_source_has_counters,
        // CR 400.7: Entered this turn.
        // Accept both the long "entered the battlefield this turn" and the abbreviated
        // "entered this turn" forms — Oracle templates vary between them for the same
        // semantic. Longer tag first so the shorter one doesn't shadow it.
        value(
            StaticCondition::SourceEnteredThisTurn,
            alt((
                tag("~ entered the battlefield this turn"),
                tag("~ entered this turn"),
            )),
        ),
        parse_this_type_entered_this_turn,
        // CR 708.2: "enchanted creature is face down" — the attached-to creature is face-down.
        value(
            StaticCondition::EnchantedIsFaceDown,
            alt((
                tag("enchanted creature is face down"),
                tag("enchanted permanent is face down"),
            )),
        ),
        value(StaticCondition::IsRingBearer, tag("~ is your ring-bearer")),
        parse_source_is_type,
        parse_source_power_toughness_condition,
    ))
    .parse(input)
}

/// CR 122.1: Parse "<subject> has <quantity> [type] counter[s] on it" into a
/// `StaticCondition::HasCounters`.
///
/// Accepts:
/// - `"~ has a counter on it"` / `"this creature has a counter on it"` →
///   `CounterMatch::Any` with `minimum: 1` (Demon Wall).
/// - `"~ has a [type] counter on it"` / `"~ has N or more [type] counters on it"` →
///   `CounterMatch::OfType(ct)`.
/// - `"~ has no counters on it"` / `"~ has no [type] counters on it"` →
///   `minimum: 0, maximum: Some(0)` (no counters of the specified flavor).
///
/// Composes subject (`parse_source_subject`) × quantity axis × optional
/// counter-type word × `"counter"/"counters"` × `"on it"` — each axis is a
/// single `alt()` so new variants add one arm rather than enumerating
/// permutations.
pub(crate) fn parse_source_has_counters(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_counter_condition_subject(input)?;
    let (rest, _) = tag("has ").parse(rest)?;

    // Quantity axis: produces (minimum, maximum).
    let (rest, (minimum, maximum)) = parse_has_counters_quantity(rest)?;

    // Counter type axis: typed first for robustness — a typed token like
    // "loyalty counter" shares no prefix with bare "counter", so branch
    // order is semantic-only (no longest-match dependency), but trying the
    // more specific alternative first is the conventional pattern.
    let (rest, counters) = alt((
        // Typed noun: `<type> counter[s]` (e.g. "a loyalty counter on it").
        parse_typed_counter_noun,
        // Bare noun → any counter type (CR 122.1 "a counter on it").
        value(CounterMatch::Any, alt((tag("counters"), tag("counter")))),
    ))
    .parse(rest)?;

    let (rest, _) = tag(" on it").parse(rest)?;

    Ok((
        rest,
        StaticCondition::HasCounters {
            counters,
            minimum,
            maximum,
        },
    ))
}

/// Subject axis for counter-has conditions. Accepts the canonical
/// source-referential subjects and the bound pronoun `"it "` used in
/// `"for as long as it has a counter on it"` style clauses. Kept separate
/// from `parse_source_subject` because `"it "` would be ambiguous in the
/// tapped/combat predicate family (which already uses `"it"` as part of
/// longer phrases) — scoping the pronoun branch to this combinator avoids
/// that coupling.
fn parse_counter_condition_subject(input: &str) -> OracleResult<'_, &str> {
    alt((parse_source_subject, tag("it "))).parse(input)
}

/// Quantity axis for `parse_source_has_counters`.
///
/// Returns `(minimum, maximum)`:
/// - `"a"` / `"one or more"` → `(1, None)`
/// - `"no"` → `(0, Some(0))`
/// - `"N or more"` → `(N, None)`
/// - `"exactly N"` → `(N, Some(N))`
/// - `"N or fewer"` → `(0, Some(N))`
fn parse_has_counters_quantity(input: &str) -> OracleResult<'_, (u32, Option<u32>)> {
    alt((
        value((1u32, None), tag("a ")),
        value((1u32, None), tag("one or more ")),
        value((0u32, Some(0u32)), tag("no ")),
        parse_exactly_n_counters,
        parse_n_or_more_counters,
        parse_n_or_fewer_counters,
    ))
    .parse(input)
}

fn parse_n_or_more_counters(input: &str) -> OracleResult<'_, (u32, Option<u32>)> {
    let (rest, n) = parse_number(input)?;
    let (rest, _) = tag(" or more ").parse(rest)?;
    Ok((rest, (n, None)))
}

fn parse_n_or_fewer_counters(input: &str) -> OracleResult<'_, (u32, Option<u32>)> {
    let (rest, n) = parse_number(input)?;
    let (rest, _) = tag(" or fewer ").parse(rest)?;
    Ok((rest, (0, Some(n))))
}

fn parse_exactly_n_counters(input: &str) -> OracleResult<'_, (u32, Option<u32>)> {
    let (rest, _) = tag("exactly ").parse(input)?;
    let (rest, n) = parse_number(rest)?;
    let (rest, _) = tag(" ").parse(rest)?;
    Ok((rest, (n, Some(n))))
}

/// Consume `"<type> counter"` / `"<type> counters"` and return
/// `CounterMatch::OfType(canonical)`.
///
/// Terminator-anchored: reads arbitrary Oracle text up to the literal
/// `" counter"` / `" counters"` suffix, then canonicalizes the consumed
/// token through `types::counter::parse_counter_type`. This accepts the
/// full set of Oracle-declared counter types (flood, charge, oil, quest,
/// …) without needing to enumerate every name in a nom `alt()` — any
/// unrecognized token falls through to `CounterType::Generic(raw)` via
/// the canonical mapping.
///
/// Fails if the input does not contain `" counter"` before end of string,
/// or if the token slice is empty (that case is the caller's `Any` branch).
fn parse_typed_counter_noun(input: &str) -> OracleResult<'_, CounterMatch> {
    let (rest_after_noun, type_slice) = take_until(" counter").parse(input)?;
    if type_slice.is_empty() {
        // Fail so the caller's `Any` branch (bare "counter[s]") can try.
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let (rest, _) =
        preceded(tag(" "), alt((tag("counters"), tag("counter")))).parse(rest_after_noun)?;
    let ct = crate::types::counter::parse_counter_type(type_slice);
    Ok((rest, CounterMatch::OfType(ct)))
}

/// CR 608.2c: Parse "this creature/permanent is a [type]" → SourceMatchesFilter.
/// Used by leveler-style cards (Figure of Fable, Figure of Destiny) where each
/// activation level gates on the source's current subtype.
fn parse_source_is_type(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_source_subject(input)?;
    let (rest, negated) = alt((
        value(false, tag("is ")),
        value(true, alt((tag("isn't "), tag("is not ")))),
    ))
    .parse(rest)?;
    let (rest, _) = parse_article(rest)?;
    let (filter, remainder) = parse_type_phrase(rest);
    let condition = StaticCondition::SourceMatchesFilter { filter };
    let condition = if negated {
        StaticCondition::Not {
            condition: Box::new(condition),
        }
    } else {
        condition
    };
    Ok((remainder, condition))
}

/// CR 400.7: Parse "this [type] entered (the battlefield) this turn" → SourceEnteredThisTurn.
fn parse_this_type_entered_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("this ").parse(input)?;
    // Consume the type word (aura, enchantment, permanent, creature, artifact, land, etc.)
    let (rest, _) = alt((
        tag("aura"),
        tag("enchantment"),
        tag("permanent"),
        tag("creature"),
        tag("artifact"),
        tag("land"),
    ))
    .parse(rest)?;
    // " entered this turn" or " entered the battlefield this turn"
    let (rest, _) = alt((
        tag(" entered the battlefield this turn"),
        tag(" entered this turn"),
    ))
    .parse(rest)?;
    Ok((rest, StaticCondition::SourceEnteredThisTurn))
}

/// CR 208.1: Parse source power/toughness comparison conditions.
///
/// Handles "its power is N or less/greater", "~ has power N or greater",
/// and equivalent enchanted/equipped creature patterns.
/// Used for "as long as enchanted creature's power is 3 or less" etc.
fn parse_source_power_toughness_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    // Subject: "its ", "~ has ", "enchanted creature's ", "equipped creature's "
    let (rest, _) = alt((
        tag("its "),
        tag("enchanted creature's "),
        tag("equipped creature's "),
    ))
    .parse(input)?;
    // Property: "power " or "toughness "
    let (rest, qty) = alt((
        value(
            QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Source,
            },
            tag("power is "),
        ),
        value(
            QuantityRef::Toughness {
                scope: crate::types::ability::ObjectScope::Source,
            },
            tag("toughness is "),
        ),
    ))
    .parse(rest)?;
    let (rest, n) = parse_number(rest)?;
    // Comparator: "or less" / "or greater"
    let (rest, comparator) = alt((
        value(Comparator::LE, tag(" or less")),
        value(Comparator::GE, tag(" or greater")),
    ))
    .parse(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref { qty },
            comparator,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
}

/// Parse "you have" quantity conditions: hand size, graveyard size, life.
///
/// Composable: "you have " + threshold/absence + quantity suffix.
/// Handles "you have no cards in hand", "you have N or more/fewer cards in hand",
/// "you have N or more cards in your graveyard", "you have N or more/less life".
fn parse_you_have_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you have ").parse(input)?;

    // "you have no cards in hand" → HandSize EQ 0
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("no cards in hand").parse(rest) {
        return Ok((
            rest,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            },
        ));
    }

    // "you have N or more [quantity-suffix]"
    let (rest, n) = parse_number(rest)?;

    // Try each quantity suffix
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(" or more cards in hand").parse(rest) {
        return Ok((
            rest,
            make_quantity_ge(
                QuantityRef::HandSize {
                    player: PlayerScope::Controller,
                },
                n,
            ),
        ));
    }
    if let Ok((rest, _)) =
        tag::<_, _, OracleError<'_>>(" or more cards in your graveyard").parse(rest)
    {
        return Ok((rest, make_quantity_ge(QuantityRef::GraveyardSize, n)));
    }
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(" or more life").parse(rest) {
        return Ok((
            rest,
            make_quantity_ge(
                QuantityRef::LifeTotal {
                    player: PlayerScope::Controller,
                },
                n,
            ),
        ));
    }
    // "you have N or less life" → LifeTotal LE N
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(" or less life").parse(rest) {
        return Ok((
            rest,
            make_quantity_comparison(
                QuantityRef::LifeTotal {
                    player: PlayerScope::Controller,
                },
                Comparator::LE,
                n,
            ),
        ));
    }
    // "you have N or fewer cards in hand" → HandSize LE N
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(" or fewer cards in hand").parse(rest) {
        return Ok((
            rest,
            make_quantity_comparison(
                QuantityRef::HandSize {
                    player: PlayerScope::Controller,
                },
                Comparator::LE,
                n,
            ),
        ));
    }

    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
}

/// Build a QuantityComparison: qty [comparator] n.
fn make_quantity_comparison(qty: QuantityRef, comparator: Comparator, n: u32) -> StaticCondition {
    StaticCondition::QuantityComparison {
        lhs: QuantityExpr::Ref { qty },
        comparator,
        rhs: QuantityExpr::Fixed { value: n as i32 },
    }
}

/// Build a QuantityComparison: qty >= n.
fn make_quantity_ge(qty: QuantityRef, n: u32) -> StaticCondition {
    make_quantity_comparison(qty, Comparator::GE, n)
}

fn creatures_died_this_turn_ref() -> QuantityRef {
    QuantityRef::ZoneChangeCountThisTurn {
        from: Some(Zone::Battlefield),
        to: Some(Zone::Graveyard),
        filter: TargetFilter::Typed(TypedFilter::creature()),
    }
}

fn nonland_permanents_left_battlefield_this_turn_ref() -> QuantityRef {
    QuantityRef::ZoneChangeCountThisTurn {
        from: Some(Zone::Battlefield),
        to: None,
        filter: TargetFilter::Typed(
            TypedFilter::permanent().with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
        ),
    }
}

fn permanents_you_controlled_left_battlefield_this_turn_ref() -> QuantityRef {
    QuantityRef::ZoneChangeCountThisTurn {
        from: Some(Zone::Battlefield),
        to: None,
        filter: TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::You)),
    }
}

fn creatures_you_controlled_left_battlefield_this_turn_ref() -> QuantityRef {
    QuantityRef::ZoneChangeCountThisTurn {
        from: Some(Zone::Battlefield),
        to: None,
        filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
    }
}

/// Parse "you control" condition patterns.
fn parse_control_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        // CR 201.2 + CR 603.4: "you control N or more [type] with different names"
        // → QuantityComparison(ObjectCountDistinctNames >= N). Tried before the
        // plain ObjectCount arm so the `with different names` suffix is not
        // mis-classified as a raw count threshold. Field of the Dead canonical.
        parse_control_count_ge_distinct_names,
        // "you control N or more [type]" → QuantityComparison(ObjectCount >= N)
        parse_control_count_ge,
        // "you control N or fewer [type]" → QuantityComparison(ObjectCount <= N)
        parse_control_count_le,
        // "you control a/an/another [type]" → IsPresent with filter
        parse_you_control_a,
        // "you don't control a/an [type]" → Not(IsPresent)
        parse_you_dont_control_a,
        // "you control no [type]" → Not(IsPresent)
        parse_you_control_no,
    ))
    .parse(input)
}

/// Parse a "≥ N" threshold prefix: either `"N or more "` or `"at least N "`.
///
/// Single authority used by all `you control` / `an opponent controls` count
/// arms so "at least five other Mountains" (Valakut) and "three or more
/// creatures" (Defense of the Heart) share the same parse path. Returns the
/// threshold N and the remaining input positioned at the type phrase.
///
/// CR 603.4: Intervening-if conditions are evaluated as written — both
/// idioms are grammatically equivalent `>= N` thresholds.
fn parse_ge_threshold(input: &str) -> OracleResult<'_, u32> {
    alt((
        // "N or more "
        |i| {
            let (rest, n) = parse_number(i)?;
            let rest = rest.trim_start();
            let (rest, _) = tag("or more ").parse(rest)?;
            Ok((rest, n))
        },
        // "at least N "
        |i| {
            let (rest, _) = tag("at least ").parse(i)?;
            let (rest, n) = parse_number(rest)?;
            let rest = rest.trim_start();
            Ok((rest, n))
        },
    ))
    .parse(input)
}

/// CR 201.2 + CR 603.4: Parse "you control N or more [type] with different names"
/// → `QuantityComparison { ObjectCountDistinctNames(filter) >= N }`.
///
/// Field of the Dead: "if you control seven or more lands with different
/// names". Two objects with the same printed name count once. General enough
/// to cover any `<article> <type> with different names` suffix, so the class
/// extends to other distinct-name threshold cards without per-card code.
fn parse_control_count_ge_distinct_names(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you control ").parse(input)?;
    let (rest, n) = parse_ge_threshold(rest)?;
    let type_text = rest.trim_end_matches('.');
    let (filter, remainder) = parse_type_phrase(type_text);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    // Require the exact "with different names" suffix on the remainder.
    let trimmed = remainder.trim_start();
    let (after_suffix, _) = tag("with different names").parse(trimmed)?;
    let filter = inject_controller_you(filter);
    let consumed = after_suffix.as_ptr() as usize - input.as_ptr() as usize;
    Ok((
        &input[consumed..],
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCountDistinctNames { filter },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
}

/// Canonical combinator: "you control N or more [type]" → QuantityComparison.
///
/// Single authority for this pattern — called from `oracle_static.rs` and
/// `oracle_trigger.rs` to avoid three-way duplication.
/// Returns the remainder after the type phrase (may be non-empty for trailing text).
pub fn parse_control_count_ge(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you control ").parse(input)?;
    let (rest, n) = parse_ge_threshold(rest)?;
    let type_text = rest.trim_end_matches('.');
    let (filter, remainder) = parse_type_phrase(type_text);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let filter = inject_controller_you(filter);
    // Map remainder back to original input slice — parse_type_phrase consumed
    // from a potentially trimmed copy, so use pointer arithmetic to get the
    // correct byte offset (remainder.len() would be wrong if trailing chars
    // were stripped by trim_end_matches).
    let consumed = remainder.as_ptr() as usize - input.as_ptr() as usize;
    Ok((
        &input[consumed..],
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
}

/// Parse "you control a/an/another [type]" → IsPresent with filter.
///
/// Generalized: uses `parse_type_phrase` so any type phrase is supported,
/// not just hardcoded creature/artifact/enchantment/planeswalker.
/// "another" is handled by passing "another [type]" to `parse_type_phrase`,
/// which recognizes "another" and adds `FilterProp::Another`.
fn parse_you_control_a(input: &str) -> OracleResult<'_, StaticCondition> {
    // Strip "you control " prefix, then pass the rest (including a/an/another) to parse_type_phrase.
    // parse_type_phrase handles "a ", "an ", and "another " as article/modifier prefixes.
    let (rest, _) = tag("you control ").parse(input)?;
    // Must start with an article or "another" — reject bare "you control creatures" (that's count)
    if !rest.starts_with("a ") && !rest.starts_with("an ") && !rest.starts_with("another ") {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let (filter, remainder) = parse_type_phrase(rest);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let filter = inject_controller_you(filter);
    let consumed = input.len() - remainder.len();
    Ok((
        &input[consumed..],
        StaticCondition::IsPresent {
            filter: Some(filter),
        },
    ))
}

/// Parse "you control N or fewer [type]" → QuantityComparison(ObjectCount <= N).
fn parse_control_count_le(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you control ").parse(input)?;
    let (rest, n) = parse_number(rest)?;
    let rest = rest.trim_start();
    let (rest, _) = tag("or fewer ").parse(rest)?;
    let type_text = rest.trim_end_matches('.');
    let (filter, remainder) = parse_type_phrase(type_text);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let filter = inject_controller_you(filter);
    let consumed = remainder.as_ptr() as usize - input.as_ptr() as usize;
    Ok((
        &input[consumed..],
        make_quantity_comparison(QuantityRef::ObjectCount { filter }, Comparator::LE, n),
    ))
}

/// Parse "you control no [type]" → Not(IsPresent { filter }).
fn parse_you_control_no(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you control no ").parse(input)?;
    let (filter, remainder) = parse_type_phrase(rest);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let filter = inject_controller_you(filter);
    let consumed = input.len() - remainder.len();
    Ok((
        &input[consumed..],
        StaticCondition::Not {
            condition: Box::new(StaticCondition::IsPresent {
                filter: Some(filter),
            }),
        },
    ))
}

/// Parse "you don't control a/an [type]" → Not(IsPresent).
fn parse_you_dont_control_a(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you don't control ").parse(input)?;
    let (rest, _) = parse_article(rest)?;
    let (filter, remainder) = parse_type_phrase(rest);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let filter = inject_controller_you(filter);
    let consumed = input.len() - remainder.len();
    Ok((
        &input[consumed..],
        StaticCondition::Not {
            condition: Box::new(StaticCondition::IsPresent {
                filter: Some(filter),
            }),
        },
    ))
}

/// CR 107.3e + CR 208.1 + CR 202.3: Parse
/// "<filter> have total <property> N or {greater|more|less|fewer}" →
/// `StaticCondition::QuantityComparison { lhs: Aggregate{Sum, property, filter}, comparator, rhs: N }`.
///
/// Single combinator parameterized over `ObjectProperty` so it covers total
/// power and toughness (CR 208.1), and total mana value (CR 202.3)
/// uniformly — one parse path instead of three sibling combinators
/// ("Parameterize, don't proliferate"). The motivating card is Betor, Kin to
/// All ("if creatures you control have total toughness 10 or greater"), but
/// the building block extends to any "<filter> have total <property> N or X"
/// phrase.
///
/// The `filter` subject reuses `parse_type_phrase`, so any subject-controller
/// combination it understands ("creatures you control", "creatures an opponent
/// controls", etc.) flows through automatically.
///
/// The result composes with both gating sites:
/// - Trigger-level intervening-if (`oracle_trigger::extract_if_condition` →
///   `static_condition_to_trigger_condition`).
/// - Per-clause "Then if X" sub-ability conditions
///   (`oracle_effect::conditions::strip_leading_general_conditional` →
///   `static_condition_to_ability_condition`).
fn parse_filter_have_total_property(input: &str) -> OracleResult<'_, StaticCondition> {
    // 1. Filter subject. parse_type_phrase consumes the noun phrase plus its
    //    trailing controller suffix ("creatures you control") and returns the
    //    typed filter with controller already injected. Reject `Any` so a bare
    //    "have total ..." prefix cannot accidentally match without a subject.
    let (filter, remainder) = parse_type_phrase(input);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }

    // 2. " have total " connective.
    let (rest, _) = tag(" have total ").parse(remainder)?;

    // 3. Property keyword. Tags include the trailing space so the number that
    //    follows can be parsed without an extra trim_start.
    let (rest, property) = alt((
        value(ObjectProperty::Toughness, tag("toughness ")),
        value(ObjectProperty::Power, tag("power ")),
        value(ObjectProperty::ManaValue, tag("mana value ")),
    ))
    .parse(rest)?;

    // 4. Threshold number.
    let (rest, n) = parse_number(rest)?;

    // 5. Comparator suffix. "or greater" / "or more" both denote `>=`;
    //    "or less" / "or fewer" both denote `<=`. The leading space is part
    //    of the suffix because `parse_number` consumes the digits but not the
    //    trailing whitespace.
    let (rest, comparator) = alt((
        value(Comparator::GE, alt((tag(" or greater"), tag(" or more")))),
        value(Comparator::LE, alt((tag(" or less"), tag(" or fewer")))),
    ))
    .parse(rest)?;

    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Sum,
                    property,
                    filter,
                },
            },
            comparator,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
}

/// Inject `ControllerRef::You` into a TargetFilter produced by `parse_type_phrase`.
fn inject_controller_you(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(tf) => TargetFilter::Typed(tf.controller(ControllerRef::You)),
        other => other,
    }
}

/// CR 102.2 + CR 102.3: Recognize opponent possessive prefixes. Shared
/// combinator used by zone-count parsing and life-total condition parsing.
fn parse_opponent_possessive(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag::<_, _, OracleError<'_>>("an opponent's "),
            tag("opponent's "),
            tag("opponents' "),
            tag("opponents "),
            tag("each opponent's "),
        )),
    )
    .parse(input)
}

/// Scope kind parsed from the possessive prefix, before the comparator
/// determines the aggregate function for existential semantics.
#[derive(Debug, Clone, Copy)]
enum LifeTotalScope {
    Controller,
    AllPlayers,
    Opponent,
}

/// CR 119: Parse "your/a player's/an opponent's life total is [comparator]
/// [quantity]" conditions. Fractional RHS quantities such as "half your
/// starting life total" compose through `parse_quantity` (CR 107.1a).
/// Note: "you have N or more life" is handled by `parse_you_have_conditions`.
fn parse_life_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    // Stage A: parse possessive prefix → scope kind (aggregate TBD).
    let (rest, scope) = alt((
        value(
            LifeTotalScope::Controller,
            tag::<_, _, OracleError<'_>>("your life total is "),
        ),
        // CR 119 + CR 102.1: Life total comparison across all players (existential).
        value(LifeTotalScope::AllPlayers, tag("a player's life total is ")),
        // CR 119 + CR 102.2: Life total comparison across opponents (existential).
        |i| {
            let (rest, _) = parse_opponent_possessive(i)?;
            let (rest, _) = tag("life total is ").parse(rest)?;
            Ok((rest, LifeTotalScope::Opponent))
        },
    ))
    .parse(input)?;

    // Stage B: parse comparator, then couple aggregate to comparator direction.
    // LE/LT → Min (min ≤ X ⟹ ∃ player with life ≤ X).
    // GE/GT → Max (max ≥ X ⟹ ∃ player with life ≥ X).
    let build_player = |scope: LifeTotalScope, comparator: Comparator| -> PlayerScope {
        match scope {
            LifeTotalScope::Controller => PlayerScope::Controller,
            LifeTotalScope::AllPlayers => PlayerScope::AllPlayers {
                aggregate: existential_aggregate(comparator),
            },
            LifeTotalScope::Opponent => PlayerScope::Opponent {
                aggregate: existential_aggregate(comparator),
            },
        }
    };

    if let Ok((rest, comparator)) = parse_life_total_comparator(rest) {
        let (rest, rhs) = nom_quantity::parse_quantity(rest)?;
        return Ok((
            rest,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: build_player(scope, comparator),
                    },
                },
                comparator,
                rhs,
            },
        ));
    }

    let (rest, n) = parse_number(rest)?;
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(" or less").parse(rest) {
        return Ok((
            rest,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: build_player(scope, Comparator::LE),
                    },
                },
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: n as i32 },
            },
        ));
    }
    let (rest, _) = tag(" or greater").parse(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: build_player(scope, Comparator::GE),
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
}

/// Existential aggregate: for "any X satisfies comparator threshold",
/// LE/LT need Min (min ≤ threshold ⟹ ∃), GE/GT need Max (max ≥ threshold ⟹ ∃).
/// EQ/NE have no single-aggregate existential encoding — unreachable from
/// `parse_life_total_comparator` which only produces LT/LE/GT/GE.
fn existential_aggregate(comparator: Comparator) -> AggregateFunction {
    match comparator {
        Comparator::LE | Comparator::LT => AggregateFunction::Min,
        Comparator::GE | Comparator::GT => AggregateFunction::Max,
        Comparator::EQ | Comparator::NE => unreachable!(
            "EQ/NE have no single-aggregate existential encoding; \
             parse_life_total_comparator never produces them"
        ),
    }
}

/// CR 119: Comparator phrase for current life total checks. Longest
/// alternatives must precede their prefixes ("less than or equal to" before
/// "less than").
fn parse_life_total_comparator(input: &str) -> OracleResult<'_, Comparator> {
    alt((
        value(
            Comparator::LE,
            tag::<_, _, OracleError<'_>>("less than or equal to "),
        ),
        value(Comparator::GE, tag("greater than or equal to ")),
        value(Comparator::LT, tag("less than ")),
        value(Comparator::GT, tag("greater than ")),
    ))
    .parse(input)
}

/// CR 113.6b: Parse zone-based source conditions.
/// Handles all player-specific zones (graveyard, hand, library) with "your",
/// and the shared exile zone (no "your").
fn parse_zone_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    use crate::types::zones::Zone;

    alt((
        // CR 702.62b: A card is suspended while it is in exile with a time
        // counter on it. The "has suspend" component is guaranteed by cards
        // that print this source-referential condition.
        value(
            StaticCondition::And {
                conditions: vec![
                    StaticCondition::SourceInZone { zone: Zone::Exile },
                    StaticCondition::HasCounters {
                        counters: CounterMatch::OfType(CounterType::Time),
                        minimum: 1,
                        maximum: None,
                    },
                ],
            },
            alt((tag("~ is suspended"), tag("this card is suspended"))),
        ),
        // Graveyard (player-specific)
        value(
            StaticCondition::SourceInZone {
                zone: Zone::Graveyard,
            },
            alt((
                tag("~ is in your graveyard"),
                tag("this card is in your graveyard"),
            )),
        ),
        // Hand (player-specific)
        value(
            StaticCondition::SourceInZone { zone: Zone::Hand },
            alt((tag("~ is in your hand"), tag("this card is in your hand"))),
        ),
        // Library (player-specific)
        value(
            StaticCondition::SourceInZone {
                zone: Zone::Library,
            },
            alt((
                tag("~ is in your library"),
                tag("this card is in your library"),
            )),
        ),
        // Exile (shared zone — no "your")
        value(
            StaticCondition::SourceInZone { zone: Zone::Exile },
            alt((tag("~ is in exile"), tag("this card is in exile"))),
        ),
    ))
    .parse(input)
}

fn parse_day_night_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("it's "), tag("it is "))).parse(input)?;
    let (rest, state) = alt((
        value(DayNight::Night, tag("night")),
        value(DayNight::Day, tag("day")),
    ))
    .parse(rest)?;
    Ok((rest, StaticCondition::DayNightIs { state }))
}

/// Parse "you've [done X] this turn" conditions.
///
/// CR 119: Life gain/loss event conditions.
/// CR 700.13: Crime tracking.
fn parse_youve_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you've ").parse(input)?;
    alt((
        |input| parse_another_spell_cast_this_turn(input, 2),
        value(
            make_quantity_ge(QuantityRef::CrimesCommittedThisTurn, 1),
            tag("committed a crime this turn"),
        ),
        value(
            make_quantity_ge(
                QuantityRef::LifeGainedThisTurn {
                    player: PlayerScope::Controller,
                },
                1,
            ),
            tag("gained life this turn"),
        ),
        value(
            make_quantity_ge(
                QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Controller,
                },
                1,
            ),
            tag("lost life this turn"),
        ),
        // "you've cast another spell this turn" → SpellsCastThisTurn >= 2
        value(
            make_quantity_ge(
                QuantityRef::SpellsCastThisTurn {
                    scope: CountScope::Controller,
                    filter: None,
                },
                2,
            ),
            tag("cast two or more spells this turn"),
        ),
        // "you've attacked this turn" / "you've attacked with a creature this turn"
        value(
            make_quantity_ge(QuantityRef::AttackedThisTurn, 1),
            alt((
                tag("attacked with a creature this turn"),
                tag("attacked this turn"),
            )),
        ),
        // "you've descended this turn"
        value(
            make_quantity_ge(QuantityRef::DescendedThisTurn, 1),
            tag("descended this turn"),
        ),
    ))
    .parse(rest)
}

/// Parse event-state conditions: "a creature died this turn", "you attacked this turn",
/// "an opponent lost life this turn", "no spells were cast last turn", etc.
///
/// These are game-state boolean checks expressible as QuantityComparison.
fn parse_event_state_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        // Compound: "you gained and lost life this turn" → And([Gained >= 1, Lost >= 1])
        // Must precede individual verb handlers to avoid partial match on "you gained".
        parse_compound_verb_condition,
        // Negated event patterns — must precede positive variants to catch "didn't" prefix.
        parse_you_didnt_this_turn,
        // "a creature died this turn" (Morbid) → zone-change count >= 1
        value(
            make_quantity_ge(creatures_died_this_turn_ref(), 1),
            alt((
                tag("a creature died this turn"),
                tag("a creature died under your control this turn"),
            )),
        ),
        // "a nonland permanent left the battlefield this turn" (Revolt variant)
        value(
            make_quantity_ge(nonland_permanents_left_battlefield_this_turn_ref(), 1),
            tag("a nonland permanent left the battlefield this turn"),
        ),
        // "a permanent you controlled left the battlefield this turn" (Revolt)
        value(
            make_quantity_ge(
                permanents_you_controlled_left_battlefield_this_turn_ref(),
                1,
            ),
            alt((
                tag("a permanent you controlled left the battlefield this turn"),
                tag("a permanent left the battlefield under your control this turn"),
            )),
        ),
        // "a creature left the battlefield under your control this turn"
        value(
            make_quantity_ge(creatures_you_controlled_left_battlefield_this_turn_ref(), 1),
            alt((
                tag("a creature you controlled left the battlefield this turn"),
                tag("a creature left the battlefield under your control this turn"),
            )),
        ),
        // "an opponent lost life this turn"
        value(
            make_quantity_ge(
                QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Sum,
                    },
                },
                1,
            ),
            alt((
                tag("an opponent lost life this turn"),
                tag("that player lost life this turn"),
            )),
        ),
        parse_opponent_lost_life_this_turn,
        // CR 119.4 + CR 603.4: "an opponent gained life this turn" — sum across
        // opponents, mirroring the lost-life sibling. Unlocks Needlebite Trap
        // alt-cost gate and any future opponent-gain-gated trap/condition.
        value(
            make_quantity_ge(
                QuantityRef::LifeGainedThisTurn {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Sum,
                    },
                },
                1,
            ),
            alt((
                tag("an opponent gained life this turn"),
                tag("that player gained life this turn"),
            )),
        ),
        // CR 119.3 + CR 603.4: "a player lost N or more life this turn"
        // (Y'shtola, Night's Blessed; Knight of the Ebon Legion). The "a
        // player" quantifier covers controller + opponents; the threshold
        // semantic is "any single player crossed N", not "sum across
        // players" — resolves via `LifeLostThisTurn { player: AllPlayers {
        // aggregate: Max } }`.
        parse_player_lost_life_this_turn,
        // CR 701.9 + CR 603.4: "an opponent discarded a card this turn"
        value(
            make_quantity_ge(QuantityRef::OpponentDiscardedCardThisTurn, 1),
            alt((
                tag("an opponent discarded a card this turn"),
                tag("any opponent discarded a card this turn"),
            )),
        ),
        // "you attacked this turn" (without "you've" prefix)
        value(
            make_quantity_ge(QuantityRef::AttackedThisTurn, 1),
            alt((
                tag("you attacked with a creature this turn"),
                tag("you attacked this turn"),
            )),
        ),
        // "you descended this turn" (without "you've" prefix)
        value(
            make_quantity_ge(QuantityRef::DescendedThisTurn, 1),
            tag("you descended this turn"),
        ),
        // "you gained life this turn" / "you gained N or more life this turn"
        parse_you_gained_life_this_turn,
        // "you cast another spell this turn" / "you cast a [type] spell this turn"
        parse_you_cast_spell_this_turn,
        // "no spells were cast last turn" (werewolf)
        value(
            make_quantity_comparison(QuantityRef::SpellsCastLastTurn, Comparator::EQ, 0),
            tag("no spells were cast last turn"),
        ),
        // "two or more spells were cast last turn" / "a player cast two or more spells last turn"
        parse_spells_cast_last_turn,
        // "you put a counter on a permanent this turn"
        parse_counter_added_this_turn,
        // "no creatures are on the battlefield"
        parse_no_on_battlefield,
    ))
    .parse(input)
}

/// CR 106.3 + CR 601.2h + CR 603.4: Parse
/// "mana from [a/an] <source-filter> [source] was spent to cast <self>" as a
/// positive quantity check over the source-qualified spent-mana snapshots.
fn parse_source_qualified_mana_spent_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("mana from ").parse(input)?;
    let (rest, source_filter) = nom_quantity::parse_mana_source_filter(rest)?;
    let (rest, _) = tag(" was spent to cast ").parse(rest)?;
    let (rest, _) = nom_quantity::parse_mana_spent_self_subject(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::TriggeringSpell,
                    metric: CastManaSpentMetric::FromSource { source_filter },
                },
            },
            comparator: Comparator::GT,
            rhs: QuantityExpr::Fixed { value: 0 },
        },
    ))
}

/// CR 106.3 + CR 601.2h + CR 603.4: Parse
/// "[N] or more mana from <source-filter> was spent to cast <self>".
fn parse_source_qualified_mana_spent_threshold(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, n) = parse_number(input)?;
    let (rest, comparator) = alt((
        value(Comparator::GE, tag(" or more")),
        value(Comparator::LE, tag(" or fewer")),
        value(Comparator::LE, tag(" or less")),
    ))
    .parse(rest)?;
    let (rest, _) = tag(" mana from ").parse(rest)?;
    let (rest, source_filter) = nom_quantity::parse_mana_source_filter(rest)?;
    let (rest, _) = tag(" was spent to cast ").parse(rest)?;
    let (rest, _) = nom_quantity::parse_mana_spent_self_subject(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::TriggeringSpell,
                    metric: CastManaSpentMetric::FromSource { source_filter },
                },
            },
            comparator,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
}

/// CR 601.2h + CR 603.4: Intervening-if comparing mana spent on the triggering
/// spell against this creature's power and/or toughness.
///
/// Recognizes "the amount of mana you spent is [comparator] this creature's
/// power or toughness" (SOS Increment reminder text). The natural-language
/// "or" means *either* threshold — `A > (P or T)` is satisfied when `A > P`
/// **or** `A > T`. Produces `StaticCondition::Or` over two
/// `QuantityComparison`s so the existing `Or`/`QuantityComparison` bridge in
/// `static_condition_to_trigger_condition` carries it directly to
/// `TriggerCondition::Or`. Also accepts the single-property forms
/// ("greater than this creature's power", "greater than this creature's
/// toughness") so future cards using only one side compose cleanly.
fn parse_mana_spent_vs_source_pt(input: &str) -> OracleResult<'_, StaticCondition> {
    // Subject: "the amount of mana you spent is "
    let (rest, _) = tag("the amount of mana you spent is ").parse(input)?;
    // Comparator: "greater than " / "less than " / "equal to "
    let (rest, comparator) = alt((
        value(Comparator::GT, tag("greater than ")),
        value(Comparator::LT, tag("less than ")),
        value(Comparator::EQ, tag("equal to ")),
    ))
    .parse(rest)?;
    // Object: subject × property, with optional "or [other property]" disjunction.
    let (rest, _) = alt((
        tag("this creature's "),
        tag("this permanent's "),
        tag("~'s "),
    ))
    .parse(rest)?;
    let (rest, first) = alt((
        value(
            QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Source,
            },
            tag("power"),
        ),
        value(
            QuantityRef::Toughness {
                scope: crate::types::ability::ObjectScope::Source,
            },
            tag("toughness"),
        ),
    ))
    .parse(rest)?;
    // Optional " or <other property>" disjunction — natural-language OR.
    let (rest, second) = opt(preceded(
        tag(" or "),
        alt((
            value(
                QuantityRef::Power {
                    scope: crate::types::ability::ObjectScope::Source,
                },
                tag("power"),
            ),
            value(
                QuantityRef::Toughness {
                    scope: crate::types::ability::ObjectScope::Source,
                },
                tag("toughness"),
            ),
        )),
    ))
    .parse(rest)?;

    let lhs = QuantityExpr::Ref {
        qty: QuantityRef::ManaSpentToCast {
            scope: crate::types::ability::CastManaObjectScope::TriggeringSpell,
            metric: crate::types::ability::CastManaSpentMetric::Total,
        },
    };
    let build = |qty: QuantityRef| StaticCondition::QuantityComparison {
        lhs: lhs.clone(),
        comparator,
        rhs: QuantityExpr::Ref { qty },
    };
    let result = match second {
        Some(second) if second != first => StaticCondition::Or {
            conditions: vec![build(first), build(second)],
        },
        _ => build(first),
    };
    Ok((rest, result))
}

/// CR 601.2h + CR 603.4: Intervening-if comparing the total amount of mana
/// spent to cast the triggering spell against a fixed threshold.
///
/// Recognizes "[N] or more mana was spent to cast [that/this] spell/it/~" and
/// the inverse "[N] or less mana was spent to cast …". Produces a
/// `StaticCondition::QuantityComparison` with LHS
/// triggering-spell spent-mana ref that bridges to `TriggerCondition::QuantityComparison`
/// via the existing `static_condition_to_trigger_condition` path.
///
/// Used by Expressive Firedancer's conditional rider ("If five or more mana
/// was spent to cast that spell, ..."), Opus/Increment family cards with
/// mana-threshold riders, and any future card that gates on triggering-spell
/// cost magnitude. Complementary to `parse_mana_spent_vs_source_pt` (which
/// handles Increment-style `greater than this creature's P/T`).
fn parse_mana_spent_threshold(input: &str) -> OracleResult<'_, StaticCondition> {
    // Number first — combinator verifies word boundary via existing helper.
    let (rest, n) = parse_number(input)?;
    // "or more" / "or fewer" / "or less" threshold word — map to comparator.
    let (rest, comparator) = alt((
        value(Comparator::GE, tag(" or more")),
        value(Comparator::LE, tag(" or fewer")),
        value(Comparator::LE, tag(" or less")),
    ))
    .parse(rest)?;
    // Fixed tail: " mana was spent to cast " + subject anaphora.
    let (rest, _) = tag(" mana was spent to cast ").parse(rest)?;
    let (rest, _) = alt((
        tag("that spell"),
        tag("that creature"),
        tag("this spell"),
        tag("this creature"),
        tag("it"),
        tag("them"),
        tag("~"),
    ))
    .parse(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: crate::types::ability::CastManaObjectScope::TriggeringSpell,
                    metric: crate::types::ability::CastManaSpentMetric::Total,
                },
            },
            comparator,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
}

/// CR 509.1b + CR 506.5: Parse combat-context conditions.
///
/// Handles "defending player controls a/an [type]" and "it's attacking alone".
fn parse_combat_context_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        parse_defending_player_controls,
        value(
            StaticCondition::SourceAttackingAlone,
            tag("it's attacking alone"),
        ),
    ))
    .parse(input)
}

/// CR 509.1b: "defending player controls a/an [type]" → DefendingPlayerControls.
fn parse_defending_player_controls(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("defending player controls ").parse(input)?;
    let (rest, _) = parse_article(rest)?;
    // parse_type_phrase returns (filter, remaining_str) — bridge to nom remainder
    let (filter, type_rest) = parse_type_phrase(rest);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let consumed = rest.len() - type_rest.len();
    Ok((
        &rest[consumed..],
        StaticCondition::DefendingPlayerControls { filter },
    ))
}

/// Parse compound-verb event conditions: "you [verb1] and [verb2] [object] this turn".
///
/// Handles shared-object constructions where two event verbs share a subject ("you")
/// and an object ("life this turn"). Each verb maps to a QuantityRef, and the result
/// is `StaticCondition::And { conditions: [lhs >= 1, rhs >= 1] }`.
///
/// Example: "you gained and lost life this turn" → And(LifeGainedThisTurn >= 1, LifeLostThisTurn >= 1)
fn parse_compound_verb_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("you "), tag("you've "))).parse(input)?;

    // Map event verbs to their QuantityRef for the shared "life this turn" object.
    fn life_verb(v: &str) -> Option<QuantityRef> {
        let result: nom::IResult<&str, QuantityRef, OracleError<'_>> = alt((
            value(
                QuantityRef::LifeGainedThisTurn {
                    player: PlayerScope::Controller,
                },
                tag("gained"),
            ),
            value(
                QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Controller,
                },
                tag("lost"),
            ),
        ))
        .parse(v);
        let (rest, qty) = result.ok()?;
        rest.is_empty().then_some(qty)
    }

    // Try "[verb1] and [verb2] life this turn"
    if let Some(and_pos) = rest.find(" and ") {
        let verb1 = &rest[..and_pos];
        let after_and = &rest[and_pos + " and ".len()..];
        // Find the shared object: " life this turn"
        if let Some(obj_pos) = after_and.find(" life this turn") {
            let verb2 = &after_and[..obj_pos];
            if let (Some(lhs), Some(rhs)) = (life_verb(verb1), life_verb(verb2)) {
                let remainder = &after_and[obj_pos + " life this turn".len()..];
                return Ok((
                    remainder,
                    StaticCondition::And {
                        conditions: vec![make_quantity_ge(lhs, 1), make_quantity_ge(rhs, 1)],
                    },
                ));
            }
        }
    }

    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
}

/// Parse "you gained [N or more] life this turn".
fn parse_you_gained_life_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("you gained "), tag("you've gained "))).parse(input)?;
    // Try "N or more life this turn"
    if let Ok((after_n, n)) = parse_number(rest) {
        let after_n = after_n.trim_start();
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("or more life this turn").parse(after_n)
        {
            return Ok((
                rest,
                make_quantity_ge(
                    QuantityRef::LifeGainedThisTurn {
                        player: PlayerScope::Controller,
                    },
                    n,
                ),
            ));
        }
    }
    // "life this turn" (minimum 1)
    let (rest, _) = tag("life this turn").parse(rest)?;
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::Controller,
            },
            1,
        ),
    ))
}

/// CR 119.3 + CR 603.4: Parse "a player lost N or more life this turn".
///
/// Y'shtola, Night's Blessed and Knight of the Ebon Legion use this idiom for
/// the intervening-`if` clause of a phase trigger. The "a player" quantifier
/// covers controller + opponents (not just opponents), and the per-player max
/// semantic is enforced by `LifeLostThisTurn { player: AllPlayers { aggregate:
/// Max } }` (one player must individually have lost ≥ N — not the sum across
/// players).
///
/// Grammar: `"a player lost " + parse_ge_threshold + "life this turn"`.
/// Composes through the existing `StaticCondition::QuantityComparison` →
/// `static_condition_to_trigger_condition` bridge with no new variants.
fn parse_player_lost_life_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("a player lost ").parse(input)?;
    let (rest, n) = parse_ge_threshold(rest)?;
    let (rest, _) = tag("life this turn").parse(rest)?;
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Max,
                },
            },
            n,
        ),
    ))
}

fn parse_opponent_lost_life_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("an opponent lost "), tag("that player lost "))).parse(input)?;
    let (rest, n) = parse_ge_threshold(rest)?;
    let (rest, _) = tag("life this turn").parse(rest)?;
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Max,
                },
            },
            n,
        ),
    ))
}

/// Parse "you cast another spell this turn" / "you cast a [type] spell this turn".
fn parse_you_cast_spell_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("you cast "), tag("you've cast "))).parse(input)?;
    // "another spell this turn" → >= 2
    if let Ok((rest, condition)) = parse_another_spell_this_turn(rest, 2) {
        return Ok((rest, condition));
    }
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("another spell this turn").parse(rest) {
        return Ok((
            rest,
            make_quantity_ge(
                QuantityRef::SpellsCastThisTurn {
                    scope: CountScope::Controller,
                    filter: None,
                },
                2,
            ),
        ));
    }
    // "a [type] spell this turn" / "an [type] spell this turn"
    let (rest, _) = parse_article(rest)?;
    if let Some(spell_pos) = rest.find(" spell this turn") {
        let type_text = &rest[..spell_pos];
        let (filter, leftover) = parse_type_phrase(type_text);
        if leftover.trim().is_empty() && filter != TargetFilter::Any {
            let remaining = &rest[spell_pos + " spell this turn".len()..];
            return Ok((
                remaining,
                make_quantity_ge(
                    QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Controller,
                        filter: Some(filter),
                    },
                    1,
                ),
            ));
        }
    }
    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
}

fn parse_opponent_cast_spell_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("an opponent has cast "), tag("an opponent cast "))).parse(input)?;
    let (rest, _) = parse_article(rest)?;
    let (rest, type_text) = take_until(" this turn").parse(rest)?;
    let (rest, _) = tag(" this turn").parse(rest)?;
    let Some(filter) = parse_spell_history_filter(type_text) else {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    };
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Opponents,
                filter: Some(filter),
            },
            1,
        ),
    ))
}

fn parse_another_spell_cast_this_turn(
    input: &str,
    minimum: u32,
) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("cast another ").parse(input)?;
    parse_another_spell_this_turn(rest, minimum)
}

fn parse_another_spell_this_turn(input: &str, minimum: u32) -> OracleResult<'_, StaticCondition> {
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("spell this turn").parse(input) {
        return Ok((
            rest,
            make_quantity_ge(
                QuantityRef::SpellsCastThisTurn {
                    scope: CountScope::Controller,
                    filter: None,
                },
                minimum,
            ),
        ));
    }
    let (rest, type_text) = take_until(" spell this turn").parse(input)?;
    let (rest, _) = tag(" spell this turn").parse(rest)?;
    let Some(filter) = parse_spell_history_filter(type_text) else {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    };
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Controller,
                filter: Some(filter),
            },
            minimum,
        ),
    ))
}

pub(crate) fn parse_spell_history_filter(type_text: &str) -> Option<TargetFilter> {
    let type_text = strip_spell_history_noun(type_text);
    let (filter, leftover) = parse_type_phrase(type_text);
    if leftover.trim().is_empty() && filter != TargetFilter::Any {
        return Some(filter);
    }
    if let Ok((rest, (first, _, second))) = (parse_color, tag(" or "), parse_color).parse(type_text)
    {
        if rest.trim().is_empty() {
            return Some(TargetFilter::Or {
                filters: vec![
                    TargetFilter::Typed(
                        TypedFilter::card().properties(vec![FilterProp::HasColor { color: first }]),
                    ),
                    TargetFilter::Typed(
                        TypedFilter::card()
                            .properties(vec![FilterProp::HasColor { color: second }]),
                    ),
                ],
            });
        }
    }

    let (rest, color) = parse_color(type_text).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some(TargetFilter::Typed(
        TypedFilter::card().properties(vec![FilterProp::HasColor { color }]),
    ))
}

fn strip_spell_history_noun(type_text: &str) -> &str {
    let type_text = type_text.trim();
    if let Ok((rest, before)) =
        nom::sequence::terminated(take_until::<_, _, OracleError<'_>>(" spell"), tag(" spell"))
            .parse(type_text)
    {
        if rest.trim().is_empty() {
            return before.trim();
        }
    }
    type_text
}

/// Parse "two or more spells were cast last turn" / "a player cast two or more spells last turn".
fn parse_spells_cast_last_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    // "two or more spells were cast last turn"
    if let Ok((rest, _)) =
        tag::<_, _, OracleError<'_>>("two or more spells were cast last turn").parse(input)
    {
        return Ok((rest, make_quantity_ge(QuantityRef::SpellsCastLastTurn, 2)));
    }
    // "a player cast two or more spells last turn"
    let (rest, _) = tag("a player cast ").parse(input)?;
    let (rest, n) = parse_number(rest)?;
    let rest = rest.trim_start();
    let (rest, _) = tag("or more spells last turn").parse(rest)?;
    Ok((rest, make_quantity_ge(QuantityRef::SpellsCastLastTurn, n)))
}

/// Parse "you [put/ve put] [a counter/one or more counters] on a
/// [permanent/creature] this turn". The quantity module owns the shared
/// counter-kind/recipient grammar so conditions and dynamic counts stay aligned.
fn parse_counter_added_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, qty) = nom_quantity::parse_counter_added_this_turn_condition(input)?;
    Ok((rest, make_quantity_ge(qty, 1)))
}

/// Parse negated event-state conditions: "you didn't cast a spell this turn",
/// "you didn't lose life this turn", "you didn't attack this turn".
///
/// CR 603.4: These gate triggers on the absence of an event this turn.
/// Composed as `QuantityComparison(ref EQ 0)` rather than `Not(ref >= 1)`.
fn parse_you_didnt_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you didn't ").parse(input)?;
    alt((
        value(
            make_quantity_comparison(
                QuantityRef::SpellsCastThisTurn {
                    scope: CountScope::Controller,
                    filter: None,
                },
                Comparator::EQ,
                0,
            ),
            tag("cast a spell this turn"),
        ),
        value(
            make_quantity_comparison(
                QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Controller,
                },
                Comparator::EQ,
                0,
            ),
            tag("lose life this turn"),
        ),
        value(
            make_quantity_comparison(QuantityRef::AttackedThisTurn, Comparator::EQ, 0),
            tag("attack this turn"),
        ),
    ))
    .parse(rest)
}

/// Parse "no [type] are on the battlefield" → ObjectCount EQ 0.
///
/// CR 603.8: State-trigger conditions for global absence checks.
/// Handles "no creatures are on the battlefield", "no nonland permanents are on the battlefield".
fn parse_no_on_battlefield(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("no ").parse(input)?;
    if let Some(are_pos) = rest.find(" are on the battlefield") {
        let type_text = &rest[..are_pos];
        let (filter, _) = parse_type_phrase(type_text);
        if !matches!(filter, TargetFilter::Any) {
            let consumed = "no ".len() + are_pos + " are on the battlefield".len();
            return Ok((
                &input[consumed..],
                StaticCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    },
                    comparator: Comparator::EQ,
                    rhs: QuantityExpr::Fixed { value: 0 },
                },
            ));
        }
    }
    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
}

/// Parse "[N or more / a / an] [type] entered the battlefield under your control this turn".
///
/// Unifies the count variant ("two or more creatures entered...") and the singular
/// variant ("a creature entered...") into one combinator.
fn parse_entered_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let entered_suffix = "entered the battlefield under your control this turn";

    // Branch 1: "N or more [type] entered..."
    if let Ok((after_n, n)) = parse_number(input) {
        let after_n = after_n.trim_start();
        if let Ok((type_and_rest, _)) = tag::<_, _, OracleError<'_>>("or more ").parse(after_n) {
            if let Ok((rest, type_text)) =
                take_until::<_, _, OracleError<'_>>(entered_suffix).parse(type_and_rest)
            {
                let (rest, _) = tag(entered_suffix).parse(rest)?;
                let (filter, _) = parse_type_phrase(type_text.trim());
                let filter = inject_controller_you(filter);
                return Ok((
                    rest,
                    make_quantity_ge(QuantityRef::EnteredThisTurn { filter }, n),
                ));
            }
        }
    }

    // Branch 2: "a/an [type] entered..."
    let (type_and_rest, _) = parse_article(input)?;
    let (rest, type_text) = take_until(entered_suffix).parse(type_and_rest)?;
    let (rest, _) = tag(entered_suffix).parse(rest)?;
    let (filter, _) = parse_type_phrase(type_text.trim());
    let filter = inject_controller_you(filter);
    Ok((
        rest,
        make_quantity_ge(QuantityRef::EnteredThisTurn { filter }, 1),
    ))
}

/// Parse "there are N [or more] [things] ..." conditions.
///
/// Covers threshold ("seven or more cards"), delirium ("four or more card types"),
/// mana values ("five or more mana values"), and typed cards ("creature cards",
/// "instant and/or sorcery cards", "land cards", "historic cards", etc.).
///
/// The "or more" modifier is optional. When present, the comparator is GE.
/// When absent — e.g. "there are five basic land types among lands you control"
/// (A-Nael, Avizoa Aeronaut) — English grammar reads as "exactly N", so the
/// comparator is EQ. CR 107.1a: Magic uses integer comparisons; exact-value
/// checks are distinct from threshold checks.
fn parse_there_are_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("there are ").parse(input)?;
    let (rest, n) = parse_number(rest)?;
    let (rest, _) = tag(" ").parse(rest)?;
    let (rest, or_more) = opt(tag("or more ")).parse(rest)?;
    let (rest, qty) = nom_quantity::parse_quantity_ref.parse(rest)?;
    let comparator = if or_more.is_some() {
        Comparator::GE
    } else {
        Comparator::EQ
    };
    Ok((
        rest,
        make_quantity_comparison(
            crate::parser::oracle_quantity::canonicalize_quantity_ref(qty),
            comparator,
            n,
        ),
    ))
}

/// CR 700.2: Parse "N or more [type] cards are in your [zone]" — subject-first
/// grammatical form of the threshold condition. Grammatically inverted form of
/// `parse_there_are_conditions` ("there are N or more cards in your graveyard").
///
/// Covers the Threshold keyword family ("seven or more cards are in your
/// graveyard") and typed variants ("seven or more land cards", "two or more Elf
/// cards"). All observed instances target "your graveyard" but the zone axis is
/// composed from a tag alt for extensibility.
fn parse_subject_first_zone_count(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, n) = parse_ge_threshold(input)?;
    let (rest, type_filters) = parse_subject_first_card_subject(rest)?;
    let (rest, (zone, scope)) = parse_scoped_zone_count_ref(rest)?;
    let qty = if type_filters.is_empty()
        && matches!(zone, crate::types::ability::ZoneRef::Graveyard)
        && matches!(scope, CountScope::Controller)
    {
        QuantityRef::GraveyardSize
    } else {
        QuantityRef::ZoneCardCount {
            zone,
            card_types: type_filters,
            scope,
        }
    };
    Ok((rest, make_quantity_ge(qty, n)))
}

fn parse_subject_first_card_subject(input: &str) -> OracleResult<'_, Vec<TypeFilter>> {
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("cards are in ").parse(input) {
        return Ok((rest, vec![]));
    }

    let (rest, type_text) = alt((
        |i| {
            let (after_type, type_text) = take_until(" cards are in ").parse(i)?;
            let (after, _) = tag(" cards are in ").parse(after_type)?;
            Ok((after, type_text))
        },
        |i| {
            let (after_type, type_text) = take_until(" are in ").parse(i)?;
            let (after, _) = tag(" are in ").parse(after_type)?;
            Ok((after, type_text))
        },
    ))
    .parse(input)?;
    let (filter, _) = parse_type_phrase(type_text.trim());
    let mut card_types = match filter {
        TargetFilter::Typed(TypedFilter { type_filters, .. }) => type_filters,
        _ => vec![],
    };
    card_types.retain(|type_filter| *type_filter != TypeFilter::Card);
    Ok((rest, card_types))
}

fn parse_scoped_zone_count_ref(input: &str) -> OracleResult<'_, (ZoneRef, CountScope)> {
    alt((
        |i| {
            let (rest, _) = tag("your ").parse(i)?;
            let (rest, zone) = parse_zone_count_ref(rest)?;
            Ok((rest, (zone, CountScope::Controller)))
        },
        |i| {
            let (rest, _) = parse_opponent_possessive(i)?;
            let (rest, zone) = parse_zone_count_ref(rest)?;
            Ok((rest, (zone, CountScope::Opponents)))
        },
        |i| {
            let (rest, _) = alt((tag("all "), tag("each player's "), tag("players' "))).parse(i)?;
            let (rest, zone) = parse_zone_count_ref(rest)?;
            Ok((rest, (zone, CountScope::All)))
        },
        |i| {
            let (rest, zone) = parse_zone_count_ref(i)?;
            Ok((rest, (zone, CountScope::All)))
        },
    ))
    .parse(input)
}

fn parse_zone_count_ref(input: &str) -> OracleResult<'_, ZoneRef> {
    alt((
        value(
            ZoneRef::Graveyard,
            alt((tag("graveyard"), tag("graveyards"))),
        ),
        value(ZoneRef::Exile, tag("exile")),
        value(ZoneRef::Hand, alt((tag("hand"), tag("hands")))),
        value(ZoneRef::Library, alt((tag("library"), tag("libraries")))),
    ))
    .parse(input)
}

/// CR 122.1 + CR 608.2c: Parse "there are <quantity> [<type>] counter[s] on <source>".
///
/// Sister combinator to `parse_source_has_counters` ("<source> has [no] [type]
/// counter[s] on it"). Same semantic shape, different syntactic form: the
/// existential there-construction places the counter clause first and the
/// source last. Used by depletion lands ("If there are no depletion counters
/// on this land, sacrifice it"), counter-threshold flip cards (Budoka Pupil,
/// Callow Jushi: "if there are two or more ki counters on this creature"),
/// and many "Then if there are N or more counters on it" continuations
/// (Brass's Tunnel-Grinder, Charitable Levy, etc.).
///
/// Composes the same axes as `parse_source_has_counters`:
/// - Quantity axis (`parse_has_counters_quantity`): "no" / "a" / "N or more"
///   / "N or fewer" / "exactly N" / "one or more".
/// - Counter type axis (`parse_typed_counter_noun` then `Any` fallback).
/// - Source subject: any pronoun / `~` form accepted by
///   `parse_counter_on_source_subject`.
fn parse_there_are_counters_on_source(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("there are ").parse(input)?;
    let (rest, (minimum, maximum)) = parse_has_counters_quantity(rest)?;
    let (rest, counters) = alt((
        parse_typed_counter_noun,
        value(CounterMatch::Any, alt((tag("counters"), tag("counter")))),
    ))
    .parse(rest)?;
    let (rest, _) = tag(" on ").parse(rest)?;
    let (rest, _) = parse_counter_on_source_subject(rest)?;
    Ok((
        rest,
        StaticCondition::HasCounters {
            counters,
            minimum,
            maximum,
        },
    ))
}

/// Trailing source subject for `parse_there_are_counters_on_source`. Mirrors the
/// SELF_REF normalization set: `~` plus the long-form noun phrases that survive
/// normalization in some Oracle prints. Bare `"it"` is included for the
/// continuation form ("Then if there are N or more counters on it") used by
/// Brass's Tunnel-Grinder and similar.
fn parse_counter_on_source_subject(input: &str) -> OracleResult<'_, &str> {
    alt((
        tag("~"),
        tag("this creature"),
        tag("this permanent"),
        tag("this land"),
        tag("this artifact"),
        tag("this enchantment"),
        tag("this aura"),
        tag("this card"),
        tag("it"),
    ))
    .parse(input)
}

/// Parse "there's a X in your Y" / "there is a X in your Y" — singular existence.
///
/// Semantic mapping: `"there's a X"` ≡ `count(X) >= 1`. Composes from existing
/// primitives — the article parser consumes "a"/"an", then `parse_quantity_ref`
/// matches the same `<filter> in <zone>` shape that `parse_there_are_conditions`
/// uses for the plural threshold form. Output is a `QuantityComparison` GE 1,
/// identical in AST shape to the plural form so downstream evaluation is shared.
///
/// Unlocks the full class of "has <keyword> as long as there's a <filter> card
/// in your <zone>" static abilities (e.g. Aang, A Lot to Learn).
fn parse_there_exists_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("there's "), tag("there is "))).parse(input)?;
    let (rest, _) = parse_article(rest)?;
    let (rest, qty) = nom_quantity::parse_quantity_ref.parse(rest)?;
    Ok((
        rest,
        make_quantity_ge(
            crate::parser::oracle_quantity::canonicalize_quantity_ref(qty),
            1,
        ),
    ))
}

/// Parse "defending player controls more [type] than you" → QuantityComparison.
///
/// CR 508.1b + CR 603.4: Attack triggers can carry intervening-if clauses
/// comparing the defending player's permanents to the trigger controller's.
/// The object-count machinery already handles `ControllerRef::You`; this arm
/// adds the combat-context controller axis for the LHS.
fn parse_defending_player_comparison_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("defending player controls more ").parse(input)?;
    let (rest, type_text) = take_until::<_, _, OracleError<'_>>(" than you").parse(rest)?;
    let (rest, _) = tag(" than you").parse(rest)?;

    let (filter, _) = parse_type_phrase(type_text.trim());
    let defending_filter = match filter {
        TargetFilter::Typed(tf) => {
            TargetFilter::Typed(tf.controller(ControllerRef::DefendingPlayer))
        }
        other => other,
    };
    let you_filter = match parse_type_phrase(type_text.trim()) {
        (TargetFilter::Typed(tf), _) => TargetFilter::Typed(tf.controller(ControllerRef::You)),
        (other, _) => other,
    };

    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: defending_filter,
                },
            },
            comparator: Comparator::GT,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter: you_filter },
            },
        },
    ))
}

/// Parse "an opponent controls more [type] than you" → QuantityComparison.
/// Also handles "an opponent has more life/cards in hand than you".
///
/// These are cross-player quantity comparisons where the opponent's quantity
/// exceeds the controller's. Composed as QuantityComparison with opponent-scoped
/// refs on the LHS and controller-scoped refs on the RHS.
fn parse_opponent_comparison_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("an opponent ").parse(input)?;

    // CR 109.3 + CR 603.4: "an opponent controls N or more [type]" /
    // "an opponent controls at least N [type]" → ObjectCount(filter w/
    // ControllerRef::Opponent) >= N. Shares `parse_ge_threshold` with the
    // `you control` arms so both idioms work uniformly. Defense of the Heart
    // ("if an opponent controls three or more creatures") is the canonical
    // card for this pattern.
    if let Ok((rest2, _)) = tag::<_, _, OracleError<'_>>("controls ").parse(rest) {
        if let Ok((rest3, n)) = parse_ge_threshold(rest2) {
            let type_text = rest3.trim_end_matches('.');
            let (filter, remainder) = parse_type_phrase(type_text);
            if !matches!(filter, TargetFilter::Any) {
                let filter = match filter {
                    TargetFilter::Typed(tf) => {
                        TargetFilter::Typed(tf.controller(ControllerRef::Opponent))
                    }
                    other => other,
                };
                let consumed = remainder.as_ptr() as usize - input.as_ptr() as usize;
                return Ok((
                    &input[consumed..],
                    StaticCondition::QuantityComparison {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount { filter },
                        },
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: n as i32 },
                    },
                ));
            }
        }
    }

    // "an opponent controls more [type] than you"
    if let Ok((rest2, _)) = tag::<_, _, OracleError<'_>>("controls more ").parse(rest) {
        if let Ok((rest3, type_text)) =
            take_until::<_, _, OracleError<'_>>(" than you").parse(rest2)
        {
            let (rest3, _) = tag(" than you").parse(rest3)?;
            let (filter, _) = parse_type_phrase(type_text.trim());
            let opp_filter = match filter {
                TargetFilter::Typed(tf) => {
                    TargetFilter::Typed(tf.controller(ControllerRef::Opponent))
                }
                other => other,
            };
            let you_filter = match parse_type_phrase(type_text.trim()) {
                (TargetFilter::Typed(tf), _) => {
                    TargetFilter::Typed(tf.controller(ControllerRef::You))
                }
                (other, _) => other,
            };
            return Ok((
                rest3,
                StaticCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter: opp_filter },
                    },
                    comparator: Comparator::GT,
                    rhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter: you_filter },
                    },
                },
            ));
        }
    }

    // "an opponent has more life than you"
    if let Ok((rest2, _)) = tag::<_, _, OracleError<'_>>("has more life than you").parse(rest) {
        return Ok((
            rest2,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Opponent {
                            aggregate: AggregateFunction::Max,
                        },
                    },
                },
                comparator: Comparator::GT,
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Controller,
                    },
                },
            },
        ));
    }

    // "an opponent has more cards in hand than you"
    if let Ok((rest2, _)) =
        tag::<_, _, OracleError<'_>>("has more cards in hand than you").parse(rest)
    {
        return Ok((
            rest2,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Opponent {
                            aggregate: AggregateFunction::Max,
                        },
                    },
                },
                comparator: Comparator::GT,
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
            },
        ));
    }

    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
}

/// CR 118.12a: Parse "[player] pays {cost}" → UnlessPay { cost }.
///
/// Handles "you pay {N}", "their controller pays {N}", "its controller pays {N}".
/// Used inside "unless" conditions for tax effects (Ghostly Prison, Propaganda, etc.).
fn parse_unless_pay_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    // Consume the payer prefix (all variants lead to the same semantic: paying a cost).
    let (rest, _) = alt((
        tag("you pay "),
        tag("its controller pays "),
        tag("their controller pays "),
        tag("that player pays "),
    ))
    .parse(input)?;
    let (rest, cost) = parse_mana_cost(rest)?;
    Ok((
        rest,
        StaticCondition::UnlessPay {
            cost,
            scaling: crate::types::ability::UnlessPayScaling::Flat,
        },
    ))
}

/// Parse an "unless" condition, wrapping the inner condition in `Not`.
fn parse_unless_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, inner) = parse_inner_condition(input)?;
    Ok((
        rest,
        StaticCondition::Not {
            condition: Box::new(inner),
        },
    ))
}

/// CR 400.7 + CR 608.2c: Parse "a[n] [type] (is|was) [verb-phrase] this way"
/// — the noun-anaphoric clause that gates a sub-ability on the LKI of an
/// object the parent effect just operated on (destroyed, exiled, sacrificed,
/// returned, discarded, milled, countered, or "put onto the battlefield").
///
/// CR 303.4f / CR 301.5b are the host-rules that motivate the present-tense
/// "is put onto the battlefield this way" variant — Aura/Equipment ETB
/// continuations that read "If an Equipment is put onto the battlefield
/// this way, you may attach it to a creature you control"
/// (Armored Skyhunter, Vault 101: Birthday Party chapters II/III, Quest for
/// the Holy Relic, Stonehewer Giant). The clause must be recognized so the
/// chain assembler can wire `forward_result: true` on the parent zone-change
/// and the runtime can check `state.last_zone_changed_ids` against the
/// matched type filter.
///
/// Composes as four orthogonal axes — article × type-phrase × tense × verb —
/// so adding a new tense or verb is a single `tag` arm, not an O(N!)
/// permutation expansion.
///
/// Returns `(remainder, type_filter)` where `remainder` is the input after
/// the consumed " this way" suffix (caller is responsible for stripping any
/// trailing punctuation like ", " or "."). On `wasn't`/`was not` the negation
/// is exposed via `negated`.
pub fn parse_zone_changed_this_way_clause(input: &str) -> OracleResult<'_, (TargetFilter, bool)> {
    // article: "a " | "an "
    let (rest, _) = parse_article(input)?;

    // type phrase — handled by the shared helper which already covers
    // top-level types (creature, artifact, enchantment, …) and subtypes
    // (Aura, Equipment, …) via the lowercase oracle subtype dictionary.
    let (filter, after_filter) = parse_type_phrase(rest);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }

    // `parse_type_phrase` returns a slice of `rest`; trim any leading whitespace
    // it left between the type phrase and the tense verb so the next `tag`
    // matches cleanly.
    let after_filter = after_filter.trim_start();

    // tense: "is" | "was" | "wasn't" | "is not" | "was not" | "isn't"
    let (rest, negated) = alt((
        value(true, tag::<_, _, OracleError<'_>>("wasn't ")),
        value(true, tag("isn't ")),
        value(true, tag("was not ")),
        value(true, tag("is not ")),
        value(false, tag("was ")),
        value(false, tag("is ")),
    ))
    .parse(after_filter)?;

    // verb-phrase: single-word imperatives + the multi-word
    // "put onto the battlefield". The verb itself is value-discarded; the
    // " this way" suffix is the discriminator.
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("put onto the battlefield"),
        tag("destroyed"),
        tag("exiled"),
        tag("sacrificed"),
        tag("returned"),
        tag("discarded"),
        tag("milled"),
        tag("countered"),
    ))
    .parse(rest)?;

    let (rest, _) = tag(" this way").parse(rest)?;
    Ok((rest, (filter, negated)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{CardTypeSetSource, RoundingMode, TypeFilter, TypedFilter};
    use crate::types::mana::ManaCost;

    #[test]
    fn test_parse_condition_your_turn() {
        let (rest, c) = parse_condition("if it's your turn, do").unwrap();
        assert_eq!(rest, ", do");
        assert_eq!(c, StaticCondition::DuringYourTurn);
    }

    #[test]
    fn test_parse_condition_as_long_as_tapped() {
        let (rest, c) = parse_condition("as long as ~ is tapped").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::SourceIsTapped));
    }

    #[test]
    fn parse_condition_as_long_as_counter_added_this_turn_uses_typed_quantity() {
        let (rest, c) = parse_condition(
            "as long as you've put one or more +1/+1 counters on a creature this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 1 });
                assert_eq!(
                    lhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::CounterAddedThisTurn {
                            actor: crate::types::ability::CountScope::Controller,
                            counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                            target: TargetFilter::Typed(TypedFilter::creature()),
                        },
                    }
                );
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_condition_no_cards() {
        let (rest, c) = parse_condition("if you have no cards in hand").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                comparator, rhs, ..
            } => {
                assert_eq!(comparator, Comparator::EQ);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 0 });
            }
            _ => panic!("expected QuantityComparison"),
        }
    }

    #[test]
    fn test_parse_condition_not_your_turn() {
        let (rest, c) = parse_condition("if it's not your turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::Not { condition } => {
                assert_eq!(*condition, StaticCondition::DuringYourTurn);
            }
            _ => panic!("expected Not(DuringYourTurn)"),
        }
    }

    #[test]
    fn test_parse_condition_seven_cards() {
        let (rest, c) = parse_condition("if you have seven or more cards in hand").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                comparator, rhs, ..
            } => {
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 7 });
            }
            _ => panic!("expected QuantityComparison"),
        }
    }

    #[test]
    fn test_parse_condition_life_le() {
        let (rest, c) = parse_condition("if your life total is 5 or less").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                comparator, rhs, ..
            } => {
                assert_eq!(comparator, Comparator::LE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 5 });
            }
            _ => panic!("expected QuantityComparison"),
        }
    }

    #[test]
    fn test_parse_condition_unless() {
        let (rest, c) = parse_condition("unless it's your turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::Not { condition } => {
                assert_eq!(*condition, StaticCondition::DuringYourTurn);
            }
            _ => panic!("expected Not(DuringYourTurn)"),
        }
    }

    #[test]
    fn test_parse_condition_source_in_graveyard() {
        let (rest, c) = parse_condition("as long as ~ is in your graveyard").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::SourceInZone {
                zone: crate::types::zones::Zone::Graveyard
            }
        ));
    }

    #[test]
    fn test_parse_condition_ring_bearer() {
        let (rest, c) = parse_condition("as long as ~ is your ring-bearer").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::IsRingBearer);
    }

    #[test]
    fn test_parse_condition_failure() {
        assert!(parse_condition("when something happens").is_err());
    }

    // -- Generalized control conditions --

    #[test]
    fn test_you_control_a_creature() {
        let (rest, c) = parse_inner_condition("you control a creature").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::IsPresent { filter: Some(_) }));
    }

    #[test]
    fn test_you_control_an_artifact() {
        let (rest, c) = parse_inner_condition("you control an artifact").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::IsPresent { filter: Some(_) }));
    }

    #[test]
    fn test_you_control_compound_presence() {
        let (rest, c) =
            parse_inner_condition("you control an artifact and an enchantment").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::And { conditions } => {
                assert_eq!(conditions.len(), 2);
                assert!(conditions
                    .iter()
                    .all(|c| matches!(c, StaticCondition::IsPresent { filter: Some(_) })));
            }
            other => panic!("expected And(IsPresent, IsPresent), got {other:?}"),
        }
    }

    #[test]
    fn test_max_speed_conditions() {
        let (rest, c) = parse_inner_condition("you have max speed").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::HasMaxSpeed);

        let (rest, c) = parse_inner_condition("your speed is 2 or higher").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SpeedGE { threshold: 2 });
    }

    #[test]
    fn test_you_control_a_land() {
        // Generalized: works for any type phrase, not just hardcoded types
        let (rest, c) = parse_inner_condition("you control a land").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::IsPresent { filter: Some(_) }));
    }

    #[test]
    fn test_you_control_n_or_more_with_different_names() {
        // CR 201.2 + CR 603.4: distinct-name threshold (Field of the Dead).
        let (rest, c) =
            parse_inner_condition("you control seven or more lands with different names").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 7 });
                match lhs {
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCountDistinctNames { filter },
                    } => match filter {
                        TargetFilter::Typed(t) => {
                            assert_eq!(t.controller, Some(ControllerRef::You));
                        }
                        _ => panic!("expected Typed filter, got {:?}", filter),
                    },
                    _ => panic!("expected ObjectCountDistinctNames, got {:?}", lhs),
                }
            }
            _ => panic!("expected QuantityComparison, got {:?}", c),
        }
    }

    #[test]
    fn test_you_control_n_or_more_plain_count_still_works() {
        // Regression: the plain "N or more" path must not be shadowed by the
        // distinct-names combinator when no suffix is present.
        let (rest, c) = parse_inner_condition("you control seven or more lands").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { .. }
                },
                ..
            }
        ));
    }

    #[test]
    fn test_you_dont_control_a_creature() {
        let (rest, c) = parse_inner_condition("you don't control a creature").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::Not { .. }));
    }

    #[test]
    fn test_you_dont_control_an_artifact() {
        let (rest, c) = parse_inner_condition("you don't control an artifact").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::Not { .. }));
    }

    #[test]
    fn test_control_count_ge() {
        let (rest, c) = parse_inner_condition("you control three or more creatures").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                comparator,
                rhs: QuantityExpr::Fixed { value: 3 },
                ..
            } => assert_eq!(comparator, Comparator::GE),
            other => panic!("expected QuantityComparison GE 3, got {other:?}"),
        }
    }

    #[test]
    fn test_control_count_ge_artifacts() {
        let (rest, c) = parse_inner_condition("you control two or more artifacts").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::QuantityComparison {
                comparator: Comparator::GE,
                ..
            }
        ));
    }

    #[test]
    fn test_graveyard_count_ge() {
        let (rest, c) =
            parse_inner_condition("you have five or more cards in your graveyard").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::GraveyardSize,
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 5 },
            } => {}
            other => panic!("expected GraveyardSize GE 5, got {other:?}"),
        }
    }

    // -- Zone condition tests (Phase 1) --

    #[test]
    fn test_source_in_hand() {
        let (rest, c) = parse_inner_condition("~ is in your hand").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::SourceInZone {
                zone: crate::types::zones::Zone::Hand
            }
        ));
    }

    #[test]
    fn test_this_card_in_hand() {
        let (rest, c) = parse_inner_condition("this card is in your hand").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::SourceInZone {
                zone: crate::types::zones::Zone::Hand
            }
        ));
    }

    #[test]
    fn test_source_in_library() {
        let (rest, c) = parse_inner_condition("~ is in your library").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::SourceInZone {
                zone: crate::types::zones::Zone::Library
            }
        ));
    }

    #[test]
    fn test_this_card_in_library() {
        let (rest, c) = parse_inner_condition("this card is in your library").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::SourceInZone {
                zone: crate::types::zones::Zone::Library
            }
        ));
    }

    // -- "There are" graveyard threshold tests (Phase 2) --

    // -- "You control" expanded tests (Phase 6) --

    #[test]
    fn test_you_control_another_creature() {
        let (rest, c) = parse_inner_condition("you control another creature").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::IsPresent { filter: Some(_) }));
    }

    #[test]
    fn test_you_control_no_creatures() {
        let (rest, c) = parse_inner_condition("you control no creatures").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::Not { .. }));
    }

    #[test]
    fn test_you_control_two_or_fewer_artifacts() {
        let (rest, c) = parse_inner_condition("you control two or fewer artifacts").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 2 },
                ..
            } => {}
            other => panic!("expected ObjectCount LE 2, got {other:?}"),
        }
    }

    // -- Tapped/untapped/entered alias tests (Phase 5) --

    #[test]
    fn test_this_creature_is_tapped() {
        let (rest, c) = parse_inner_condition("this creature is tapped").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceIsTapped);
    }

    #[test]
    fn test_this_permanent_is_untapped() {
        let (rest, c) = parse_inner_condition("this permanent is untapped").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::Not { .. }));
    }

    #[test]
    fn test_this_enchantment_entered_this_turn() {
        let (rest, c) = parse_inner_condition("this enchantment entered this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceEnteredThisTurn);
    }

    #[test]
    fn test_this_aura_entered_battlefield_this_turn() {
        let (rest, c) =
            parse_inner_condition("this aura entered the battlefield this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceEnteredThisTurn);
    }

    // CR 400.7: Shardmage's Rescue — `~ entered this turn` (no "the battlefield").
    // After `this aura` → `~` normalization, the condition parser sees the canonical
    // `~` form of the abbreviated phrase.
    #[test]
    fn test_tilde_entered_this_turn_short_form() {
        let (rest, c) = parse_inner_condition("~ entered this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceEnteredThisTurn);
    }

    // CR 400.7: Long form still wins via first-match-longest `alt` ordering.
    #[test]
    fn test_tilde_entered_battlefield_this_turn() {
        let (rest, c) = parse_inner_condition("~ entered the battlefield this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceEnteredThisTurn);
    }

    // CR 708.2: Unable to Scream — attached-to creature face-down gate.
    #[test]
    fn test_enchanted_creature_is_face_down() {
        let (rest, c) = parse_inner_condition("enchanted creature is face down").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::EnchantedIsFaceDown);
    }

    #[test]
    fn test_enchanted_permanent_is_face_down() {
        let (rest, c) = parse_inner_condition("enchanted permanent is face down").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::EnchantedIsFaceDown);
    }

    // CR 406.6 + CR 607.1: Veteran Survivor — threshold over linked-exile pile.
    #[test]
    fn test_there_are_three_or_more_cards_exiled_with_source() {
        let (rest, c) =
            parse_inner_condition("there are three or more cards exiled with ~").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::CardsExiledBySource,
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            } => {}
            other => panic!("expected CardsExiledBySource GE 3, got {other:?}"),
        }
    }

    // Variant phrasing: "this creature" form (used before `~` normalization kicks in,
    // and remains accepted by the quantity parser for robustness).
    #[test]
    fn test_there_are_cards_exiled_with_this_creature() {
        let (rest, c) =
            parse_inner_condition("there are two or more cards exiled with this creature").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::CardsExiledBySource,
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            } => {}
            other => panic!("expected CardsExiledBySource GE 2, got {other:?}"),
        }
    }

    // -- Combat-state predicate tests (CR 508.1k / CR 509.1g / CR 509.1h) --

    #[test]
    fn test_source_is_attacking() {
        let (rest, c) = parse_inner_condition("~ is attacking").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceIsAttacking);
    }

    #[test]
    fn test_this_creature_is_attacking() {
        let (rest, c) = parse_inner_condition("this creature is attacking").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceIsAttacking);
    }

    #[test]
    fn test_equipped_creature_is_attacking() {
        let (rest, c) = parse_inner_condition("equipped creature is attacking").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceIsAttacking);
    }

    #[test]
    fn test_enchanted_creature_is_attacking() {
        let (rest, c) = parse_inner_condition("enchanted creature is attacking").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceIsAttacking);
    }

    #[test]
    fn test_source_isnt_attacking() {
        // Gaea's Liege: "as long as ~ isn't attacking, ..."
        let (rest, c) = parse_inner_condition("~ isn't attacking").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::Not {
                condition: Box::new(StaticCondition::SourceIsAttacking),
            }
        );
    }

    #[test]
    fn test_source_is_blocking() {
        let (rest, c) = parse_inner_condition("~ is blocking").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceIsBlocking);
    }

    #[test]
    fn test_source_is_blocked() {
        let (rest, c) = parse_inner_condition("~ is blocked").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceIsBlocked);
    }

    #[test]
    fn test_source_is_attacking_or_blocking() {
        // Composes via the existing `Or` combinator — no bespoke variant.
        let (rest, c) = parse_inner_condition("~ is attacking or blocking").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::Or {
                conditions: vec![
                    StaticCondition::SourceIsAttacking,
                    StaticCondition::SourceIsBlocking,
                ],
            }
        );
    }

    #[test]
    fn test_tapped_untapped_regression_after_subject_refactor() {
        // Regression guard: after extracting `parse_source_subject` (which now consumes
        // only "<subject> " without trailing "is"), the tapped/untapped path must still
        // resolve correctly.
        let (rest, c) = parse_inner_condition("~ is tapped").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceIsTapped);
    }

    // CR 301.5a: SourceIsEquipped predicate across subjects.
    #[test]
    fn test_source_is_equipped() {
        let (rest, c) = parse_inner_condition("~ is equipped").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceIsEquipped);

        let (rest, c) = parse_inner_condition("this creature is equipped").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceIsEquipped);
    }

    // CR 701.37: SourceIsMonstrous predicate across subjects.
    #[test]
    fn test_source_is_monstrous() {
        let (rest, c) = parse_inner_condition("this creature is monstrous").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceIsMonstrous);

        let (rest, c) = parse_inner_condition("~ is monstrous").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceIsMonstrous);
    }

    // CR 301.5 + CR 303.4: SourceAttachedToCreature predicate.
    #[test]
    fn test_source_attached_to_creature() {
        let (rest, c) = parse_inner_condition("~ is attached to a creature").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceAttachedToCreature);

        let (rest, c) = parse_inner_condition("this creature is attached to a creature").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceAttachedToCreature);
    }

    // -- "You've [done X] this turn" tests (Phase 4) --

    #[test]
    fn test_youve_committed_crime() {
        let (rest, c) = parse_inner_condition("you've committed a crime this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::CrimesCommittedThisTurn,
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {}
            other => panic!("expected CrimesCommittedThisTurn GE 1, got {other:?}"),
        }
    }

    #[test]
    fn test_youve_gained_life() {
        let (rest, c) = parse_inner_condition("you've gained life this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::LifeGainedThisTurn { .. },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {}
            other => panic!("expected LifeGainedThisTurn GE 1, got {other:?}"),
        }
    }

    /// CR 119.4 + CR 603.4 (Π-4): "an opponent gained life this turn" must
    /// parse to `LifeGainedThisTurn { Opponent { Sum } } ≥ 1` — the
    /// opponent-axis dual to the existing `you've gained` controller-axis
    /// reading. Unlocks Needlebite Trap class.
    #[test]
    fn test_parse_condition_an_opponent_gained_life_this_turn() {
        let (rest, c) = parse_inner_condition("an opponent gained life this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: PlayerScope::Opponent {
                            aggregate: AggregateFunction::Sum,
                        },
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        );
    }

    #[test]
    fn test_youve_lost_life() {
        let (rest, c) = parse_inner_condition("you've lost life this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::LifeLostThisTurn { .. },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {}
            other => panic!("expected LifeLostThisTurn GE 1, got {other:?}"),
        }
    }

    // -- Entered-this-turn tests (Phase 3) --

    #[test]
    fn test_entered_this_turn_count() {
        let (rest, c) = parse_inner_condition(
            "two or more creatures entered the battlefield under your control this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::EnteredThisTurn { .. },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            } => {}
            other => panic!("expected EnteredThisTurn GE 2, got {other:?}"),
        }
    }

    #[test]
    fn test_entered_this_turn_singular() {
        let (rest, c) = parse_inner_condition(
            "a creature entered the battlefield under your control this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::EnteredThisTurn { .. },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {}
            other => panic!("expected EnteredThisTurn GE 1, got {other:?}"),
        }
    }

    // -- "There are" graveyard threshold tests (Phase 2) --

    #[test]
    fn test_there_are_cards_in_graveyard() {
        let (rest, c) =
            parse_inner_condition("there are seven or more cards in your graveyard").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::GraveyardSize,
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 7 },
            } => {}
            other => panic!("expected GraveyardSize GE 7, got {other:?}"),
        }
    }

    /// CR 107.1: Comma-thousands-separator numeric literals must parse as a
    /// single integer in conditions. Motivating card: A Good Thing ("if you
    /// have 1,000 or more life, you lose the game").
    #[test]
    fn test_you_have_thousands_life() {
        let (rest, c) = parse_inner_condition("you have 1,000 or more life").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::LifeTotal {
                                player: PlayerScope::Controller,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1000 },
            } => {}
            other => panic!("expected LifeTotal GE 1000, got {other:?}"),
        }
    }

    /// CR 107.1a + CR 603.4: "there are N X" without "or more" → exact-value
    /// comparison (EQ). Motivating card: A-Nael, Avizoa Aeronaut ("Then if there
    /// are five basic land types among lands you control, draw a card").
    #[test]
    fn test_there_are_domain_exact_count() {
        let (rest, c) =
            parse_inner_condition("there are five basic land types among lands you control")
                .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::BasicLandTypeCount {
                                controller: ControllerRef::You,
                            },
                    },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 5 },
            } => {}
            other => panic!("expected BasicLandTypeCount EQ 5, got {other:?}"),
        }
    }

    #[test]
    fn test_there_are_card_types_delirium() {
        let (rest, c) = parse_inner_condition(
            "there are four or more card types among cards in your graveyard",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::DistinctCardTypes {
                                source: CardTypeSetSource::Zone { .. },
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            } => {}
            other => panic!("expected zone-scoped DistinctCardTypes GE 4, got {other:?}"),
        }
    }

    /// CR 122.1 + CR 603.4: "there are N or more counters among [filter]" —
    /// intervening-if variant used by Lux Artillery. `counter_type: None` means
    /// "sum across every counter type on the matching permanents."
    #[test]
    fn test_there_are_counters_among_filter() {
        let (rest, c) = parse_inner_condition(
            "there are thirty or more counters among artifacts and creatures you control, rest",
        )
        .unwrap();
        assert!(rest.starts_with(','), "remainder: {rest:?}");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::CountersOnObjects {
                                counter_type,
                                filter,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 30 },
            } => {
                assert!(counter_type.is_none(), "got {counter_type:?}");
                assert!(matches!(filter, TargetFilter::Or { .. }), "got {filter:?}");
            }
            other => panic!("expected CountersOnObjects GE 30, got {other:?}"),
        }
    }

    #[test]
    fn test_there_are_card_types_among_cards_exiled_with_source() {
        let (rest, c) =
            parse_inner_condition("there are four or more card types among cards exiled with ~")
                .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::DistinctCardTypes {
                                source: CardTypeSetSource::ExiledBySource,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            } => {}
            other => panic!("expected linked-exile DistinctCardTypes GE 4, got {other:?}"),
        }
    }

    #[test]
    fn test_there_are_subtype_cards_in_graveyard() {
        let (rest, c) =
            parse_inner_condition("there are three or more Lesson cards in your graveyard")
                .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ZoneCardCount {
                                zone: crate::types::ability::ZoneRef::Graveyard,
                                card_types,
                                scope: crate::types::ability::CountScope::Controller,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            } => {
                assert_eq!(card_types, vec![TypeFilter::Subtype("Lesson".to_string())]);
            }
            other => panic!("expected Lesson graveyard count GE 3, got {other:?}"),
        }
    }

    #[test]
    fn test_subject_first_land_cards_in_graveyard() {
        let (rest, c) =
            parse_inner_condition("seven or more land cards are in your graveyard").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ZoneCardCount {
                                zone: crate::types::ability::ZoneRef::Graveyard,
                                card_types,
                                scope: crate::types::ability::CountScope::Controller,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 7 },
            } => {
                assert_eq!(card_types, vec![TypeFilter::Land]);
            }
            other => panic!("expected land graveyard count GE 7, got {other:?}"),
        }
    }

    /// Singular existence form: "there's a X in your Y" ≡ count(X) >= 1.
    /// Covers Aang, A Lot to Learn — "has vigilance as long as there's a Lesson
    /// card in your graveyard." — and every other card with the same grammatical shape.
    #[test]
    fn test_there_exists_subtype_card_in_graveyard() {
        for phrase in [
            "there's a Lesson card in your graveyard",
            "there is a Lesson card in your graveyard",
        ] {
            let (rest, c) = parse_inner_condition(phrase).unwrap();
            assert_eq!(rest, "", "unconsumed input for {phrase:?}");
            match c {
                StaticCondition::QuantityComparison {
                    lhs:
                        QuantityExpr::Ref {
                            qty:
                                QuantityRef::ZoneCardCount {
                                    zone: crate::types::ability::ZoneRef::Graveyard,
                                    card_types,
                                    scope: crate::types::ability::CountScope::Controller,
                                },
                        },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                } => {
                    assert_eq!(card_types, vec![TypeFilter::Subtype("Lesson".to_string())]);
                }
                other => panic!("expected Lesson graveyard count GE 1, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_this_card_in_exile() {
        let (rest, c) = parse_inner_condition("this card is in exile").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::SourceInZone {
                zone: crate::types::zones::Zone::Exile
            }
        ));
    }

    // -- Source type matching (Figure of Fable pattern) --

    #[test]
    fn test_source_is_a_subtype() {
        let (rest, c) = parse_inner_condition("this creature is a scout").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::SourceMatchesFilter { .. }));
    }

    #[test]
    fn test_source_is_an_subtype() {
        let (rest, c) = parse_inner_condition("this creature is an elf").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::SourceMatchesFilter { .. }));
    }

    #[test]
    fn test_source_is_a_permanent_type() {
        let (rest, c) = parse_inner_condition("this permanent is a creature").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::SourceMatchesFilter { .. }));
    }

    #[test]
    fn test_source_is_not_a_type() {
        let (rest, c) = parse_inner_condition("this enchantment isn't a creature").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::Not { condition }
                if matches!(*condition, StaticCondition::SourceMatchesFilter { .. })
        ));
    }

    // -- Player-state conditions --

    #[test]
    fn test_youre_the_monarch() {
        let (rest, c) = parse_inner_condition("you're the monarch").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::IsMonarch);
    }

    #[test]
    fn test_you_are_the_monarch() {
        let (rest, c) = parse_inner_condition("you are the monarch").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::IsMonarch);
    }

    #[test]
    fn test_city_blessing() {
        let (rest, c) = parse_inner_condition("you have the city's blessing").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::HasCityBlessing);
    }

    // -- "you have N or less" conditions --

    #[test]
    fn test_you_have_5_or_less_life() {
        let (rest, c) = parse_inner_condition("you have five or less life").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::LifeTotal {
                                player: PlayerScope::Controller,
                            },
                    },
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 5 },
            } => {}
            other => panic!("expected LifeTotal LE 5, got {other:?}"),
        }
    }

    #[test]
    fn test_your_life_total_le_half_starting_life_total() {
        let (rest, c) = parse_inner_condition(
            "your life total is less than or equal to half your starting life total",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::LifeTotal {
                                player: PlayerScope::Controller,
                            },
                    },
                comparator: Comparator::LE,
                rhs:
                    QuantityExpr::DivideRounded {
                        inner,
                        divisor: 2,
                        rounding: RoundingMode::Down,
                    },
            } => {
                assert!(matches!(
                    inner.as_ref(),
                    QuantityExpr::Ref {
                        qty: QuantityRef::StartingLifeTotal
                    }
                ));
            }
            other => {
                panic!("expected LifeTotal LE DivideRounded(StartingLifeTotal), got {other:?}")
            }
        }
    }

    #[test]
    fn test_a_players_life_total_le_half_their_starting() {
        let (rest, c) = parse_inner_condition(
            "a player's life total is less than or equal to half their starting life total",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::LifeTotal {
                                player:
                                    PlayerScope::AllPlayers {
                                        aggregate: AggregateFunction::Min,
                                    },
                            },
                    },
                comparator: Comparator::LE,
                rhs:
                    QuantityExpr::DivideRounded {
                        inner,
                        divisor: 2,
                        rounding: RoundingMode::Down,
                    },
            } => {
                assert!(matches!(
                    inner.as_ref(),
                    QuantityExpr::Ref {
                        qty: QuantityRef::StartingLifeTotal
                    }
                ));
            }
            other => {
                panic!(
                    "expected AllPlayers(Min) LE DivideRounded(StartingLifeTotal), got {other:?}"
                )
            }
        }
    }

    #[test]
    fn test_an_opponents_life_total_lt_half_their_starting() {
        let (rest, c) = parse_inner_condition(
            "an opponent's life total is less than half their starting life total",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::LifeTotal {
                                player:
                                    PlayerScope::Opponent {
                                        aggregate: AggregateFunction::Min,
                                    },
                            },
                    },
                comparator: Comparator::LT,
                rhs:
                    QuantityExpr::DivideRounded {
                        inner,
                        divisor: 2,
                        rounding: RoundingMode::Down,
                    },
            } => {
                assert!(matches!(
                    inner.as_ref(),
                    QuantityExpr::Ref {
                        qty: QuantityRef::StartingLifeTotal
                    }
                ));
            }
            other => {
                panic!("expected Opponent(Min) LT DivideRounded(StartingLifeTotal), got {other:?}")
            }
        }
    }

    #[test]
    fn test_a_players_life_total_n_or_less() {
        let (rest, c) = parse_inner_condition("a player's life total is 5 or less").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::AllPlayers {
                            aggregate: AggregateFunction::Min,
                        },
                    },
                },
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 5 },
            }
        );
    }

    #[test]
    fn test_an_opponents_life_total_n_or_greater() {
        let (rest, c) = parse_inner_condition("an opponent's life total is 10 or greater").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Opponent {
                            aggregate: AggregateFunction::Max,
                        },
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 10 },
            }
        );
    }

    #[test]
    fn test_your_life_total_comparator_variants() {
        for (text, expected_comparator, expected_rhs) in [
            (
                "your life total is less than 7",
                Comparator::LT,
                QuantityExpr::Fixed { value: 7 },
            ),
            (
                "your life total is greater than your starting life total",
                Comparator::GT,
                QuantityExpr::Ref {
                    qty: QuantityRef::StartingLifeTotal,
                },
            ),
            (
                "your life total is greater than or equal to your starting life total",
                Comparator::GE,
                QuantityExpr::Ref {
                    qty: QuantityRef::StartingLifeTotal,
                },
            ),
        ] {
            let (rest, c) = parse_inner_condition(text).unwrap();
            assert_eq!(rest, "");
            match c {
                StaticCondition::QuantityComparison {
                    lhs:
                        QuantityExpr::Ref {
                            qty:
                                QuantityRef::LifeTotal {
                                    player: PlayerScope::Controller,
                                },
                        },
                    comparator,
                    rhs,
                } => {
                    assert_eq!(comparator, expected_comparator);
                    assert_eq!(rhs, expected_rhs);
                }
                other => panic!("expected life total comparison for {text}, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_you_have_fewer_cards_in_hand() {
        let (rest, c) = parse_inner_condition("you have two or fewer cards in hand").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::HandSize {
                                player: PlayerScope::Controller,
                            },
                    },
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 2 },
            } => {}
            other => panic!("expected HandSize LE 2, got {other:?}"),
        }
    }

    // -- Opponent comparison conditions --

    #[test]
    fn test_defending_player_controls_more_lands() {
        let (rest, c) =
            parse_inner_condition("defending player controls more lands than you").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter: lhs },
                    },
                comparator: Comparator::GT,
                rhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter: rhs },
                    },
            } => {
                let TargetFilter::Typed(lhs) = lhs else {
                    panic!("expected typed lhs filter");
                };
                assert_eq!(lhs.controller, Some(ControllerRef::DefendingPlayer));
                let TargetFilter::Typed(rhs) = rhs else {
                    panic!("expected typed rhs filter");
                };
                assert_eq!(rhs.controller, Some(ControllerRef::You));
            }
            other => panic!("expected ObjectCount GT ObjectCount, got {other:?}"),
        }
    }

    #[test]
    fn test_opponent_controls_more_creatures() {
        let (rest, c) =
            parse_inner_condition("an opponent controls more creatures than you").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { .. },
                    },
                comparator: Comparator::GT,
                rhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { .. },
                    },
            } => {}
            other => panic!("expected ObjectCount GT ObjectCount, got {other:?}"),
        }
    }

    #[test]
    fn test_opponent_has_more_life() {
        let (rest, c) = parse_inner_condition("an opponent has more life than you").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::LifeTotal {
                                player:
                                    PlayerScope::Opponent {
                                        aggregate: AggregateFunction::Max,
                                    },
                            },
                    },
                comparator: Comparator::GT,
                rhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::LifeTotal {
                                player: PlayerScope::Controller,
                            },
                    },
            } => {}
            other => panic!("expected OpponentLifeTotal GT LifeTotal, got {other:?}"),
        }
    }

    #[test]
    fn test_opponent_has_more_cards_in_hand() {
        let (rest, c) =
            parse_inner_condition("an opponent has more cards in hand than you").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::HandSize {
                                player:
                                    PlayerScope::Opponent {
                                        aggregate: AggregateFunction::Max,
                                    },
                            },
                    },
                comparator: Comparator::GT,
                rhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::HandSize {
                                player: PlayerScope::Controller,
                            },
                    },
            } => {}
            other => panic!("expected OpponentHandSize GT HandSize, got {other:?}"),
        }
    }

    // -- Unless pay conditions --

    #[test]
    fn test_unless_you_pay() {
        let (rest, c) = parse_inner_condition("you pay {2}").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::UnlessPay { cost, scaling } => {
                assert_eq!(
                    cost,
                    ManaCost::Cost {
                        shards: vec![],
                        generic: 2
                    }
                );
                assert_eq!(scaling, crate::types::ability::UnlessPayScaling::Flat);
            }
            other => panic!("expected UnlessPay, got {other:?}"),
        }
    }

    #[test]
    fn test_unless_their_controller_pays() {
        let (rest, c) = parse_inner_condition("their controller pays {1}").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::UnlessPay { .. }));
    }

    #[test]
    fn test_unless_condition_with_pay() {
        let (rest, c) = parse_condition("unless you pay {2}").unwrap();
        assert_eq!(rest, "");
        // "unless X" wraps inner in Not
        match c {
            StaticCondition::Not { condition } => {
                assert!(matches!(*condition, StaticCondition::UnlessPay { .. }));
            }
            other => panic!("expected Not(UnlessPay), got {other:?}"),
        }
    }

    // -- Source power/toughness comparison conditions --

    #[test]
    fn test_its_power_is_3_or_less() {
        let (rest, c) = parse_inner_condition("its power is three or less").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::Power {
                                scope: crate::types::ability::ObjectScope::Source,
                            },
                    },
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 3 },
            } => {}
            other => panic!("expected SelfPower LE 3, got {other:?}"),
        }
    }

    #[test]
    fn test_enchanted_creature_power_ge() {
        let (rest, c) =
            parse_inner_condition("enchanted creature's power is four or greater").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::Power {
                                scope: crate::types::ability::ObjectScope::Source,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            } => {}
            other => panic!("expected SelfPower GE 4, got {other:?}"),
        }
    }

    // -- "as long as" with new conditions --

    #[test]
    fn test_as_long_as_you_control_a_swamp() {
        let (rest, c) = parse_condition("as long as you control a swamp").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::IsPresent { filter: Some(_) }));
    }

    #[test]
    fn another_filtered_spell_this_turn_counts_current_spell_context() {
        let (rest, c) =
            parse_inner_condition("you've cast another instant or sorcery spell this turn")
                .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::SpellsCastThisTurn {
                                scope: CountScope::Controller,
                                filter: Some(TargetFilter::Or { filters }),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            } => assert!(
                filters.iter().any(|filter| matches!(
                    filter,
                    TargetFilter::Typed(TypedFilter { type_filters, .. })
                        if type_filters == &vec![TypeFilter::Instant]
                )) && filters.iter().any(|filter| matches!(
                    filter,
                    TargetFilter::Typed(TypedFilter { type_filters, .. })
                        if type_filters == &vec![TypeFilter::Sorcery]
                ))
            ),
            other => panic!("expected filtered SpellsCastThisTurn GE 2, got {other:?}"),
        }
    }

    #[test]
    fn opponent_cast_color_spell_this_turn_counts_opponents() {
        let (rest, c) =
            parse_inner_condition("an opponent has cast a blue or black spell this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::SpellsCastThisTurn {
                                scope: CountScope::Opponents,
                                filter: Some(TargetFilter::Or { filters }),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => assert_eq!(filters.len(), 2),
            other => panic!("expected opponent scoped filtered SpellsCastThisTurn, got {other:?}"),
        }
    }

    #[test]
    fn opponent_cast_spell_with_mana_value_this_turn_counts_opponents() {
        let (rest, c) = parse_inner_condition(
            "an opponent has cast a spell with mana value 3 or less this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::SpellsCastThisTurn {
                                scope: CountScope::Opponents,
                                filter: Some(TargetFilter::Typed(TypedFilter { properties, .. })),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => assert!(properties.iter().any(|property| matches!(
                property,
                FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: 3 },
                }
            ))),
            other => panic!("expected opponent scoped mana-value spell condition, got {other:?}"),
        }
    }

    #[test]
    fn test_as_long_as_power_3_or_less() {
        let (rest, c) = parse_condition("as long as its power is three or less").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::QuantityComparison {
                comparator: Comparator::LE,
                ..
            }
        ));
    }

    // -- "you didn't" negated event patterns --

    #[test]
    fn test_you_didnt_cast_a_spell_this_turn() {
        let (rest, c) = parse_inner_condition("you didn't cast a spell this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert!(matches!(
                    lhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::SpellsCastThisTurn {
                            scope: CountScope::Controller,
                            filter: None
                        }
                    }
                ));
                assert_eq!(comparator, Comparator::EQ);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 0 });
            }
            _ => panic!("expected QuantityComparison, got {c:?}"),
        }
    }

    #[test]
    fn test_you_didnt_lose_life_this_turn() {
        let (rest, c) = parse_inner_condition("you didn't lose life this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert!(matches!(
                    lhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::LifeLostThisTurn { .. }
                    }
                ));
                assert_eq!(comparator, Comparator::EQ);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 0 });
            }
            _ => panic!("expected QuantityComparison, got {c:?}"),
        }
    }

    #[test]
    fn test_you_didnt_attack_this_turn() {
        let (rest, c) = parse_inner_condition("you didn't attack this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert!(matches!(
                    lhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::AttackedThisTurn
                    }
                ));
                assert_eq!(comparator, Comparator::EQ);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 0 });
            }
            _ => panic!("expected QuantityComparison, got {c:?}"),
        }
    }

    // -- "no [type] are on the battlefield" --

    #[test]
    fn test_no_creatures_on_battlefield() {
        let (rest, c) = parse_inner_condition("no creatures are on the battlefield").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert!(matches!(
                    lhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { .. }
                    }
                ));
                assert_eq!(comparator, Comparator::EQ);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 0 });
            }
            _ => panic!("expected QuantityComparison, got {c:?}"),
        }
    }

    // -- "a nonland permanent left the battlefield this turn" --

    #[test]
    fn test_nonland_permanent_left_battlefield() {
        let (rest, c) =
            parse_inner_condition("a nonland permanent left the battlefield this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert!(matches!(
                    lhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ZoneChangeCountThisTurn {
                            from: Some(Zone::Battlefield),
                            to: None,
                            ..
                        }
                    }
                ));
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 1 });
            }
            _ => panic!("expected QuantityComparison, got {c:?}"),
        }
    }

    #[test]
    fn test_creature_left_battlefield_under_your_control() {
        let (rest, c) =
            parse_inner_condition("a creature left the battlefield under your control this turn")
                .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ZoneChangeCountThisTurn {
                                from: Some(Zone::Battlefield),
                                to: None,
                                filter: TargetFilter::Typed(filter),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {
                assert!(filter
                    .type_filters
                    .iter()
                    .any(|filter| matches!(filter, TypeFilter::Creature)));
                assert_eq!(filter.controller, Some(ControllerRef::You));
            }
            other => panic!("expected controlled creature zone-change count, got {other:?}"),
        }
    }

    #[test]
    fn day_night_designation_condition_parses() {
        let (rest, c) = parse_inner_condition("it's night").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::DayNightIs {
                state: DayNight::Night
            }
        );
    }

    // -- "you control your commander" --

    #[test]
    fn test_you_control_your_commander() {
        let (rest, c) = parse_inner_condition("you control your commander").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::ControlsCommander);
    }

    // -- "a creature died under your control this turn" --

    #[test]
    fn test_creature_died_under_your_control() {
        let (rest, c) =
            parse_inner_condition("a creature died under your control this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert!(matches!(
                    lhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ZoneChangeCountThisTurn {
                            from: Some(Zone::Battlefield),
                            to: Some(Zone::Graveyard),
                            ..
                        }
                    }
                ));
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 1 });
            }
            _ => panic!("expected QuantityComparison, got {c:?}"),
        }
    }

    /// CR 601.2h + CR 603.4: Increment intervening-if parses as `Or` over two
    /// `QuantityComparison`s — mana spent vs self power, mana spent vs self toughness.
    #[test]
    fn test_parse_condition_increment_mana_spent_vs_self_pt() {
        let (rest, c) = parse_condition(
            "if the amount of mana you spent is greater than this creature's power or toughness",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::Or { conditions } => {
                assert_eq!(conditions.len(), 2, "expected two disjuncts");
                let expected_lhs = QuantityExpr::Ref {
                    qty: QuantityRef::ManaSpentToCast {
                        scope: crate::types::ability::CastManaObjectScope::TriggeringSpell,
                        metric: crate::types::ability::CastManaSpentMetric::Total,
                    },
                };
                let pt_refs: Vec<QuantityRef> = conditions
                    .iter()
                    .filter_map(|cond| match cond {
                        StaticCondition::QuantityComparison {
                            lhs,
                            comparator,
                            rhs,
                        } => {
                            assert_eq!(*lhs, expected_lhs);
                            assert_eq!(*comparator, Comparator::GT);
                            match rhs {
                                QuantityExpr::Ref { qty } => Some(qty.clone()),
                                _ => None,
                            }
                        }
                        _ => None,
                    })
                    .collect();
                assert!(pt_refs.contains(&QuantityRef::Power {
                    scope: crate::types::ability::ObjectScope::Source
                }));
                assert!(pt_refs.contains(&QuantityRef::Toughness {
                    scope: crate::types::ability::ObjectScope::Source
                }));
            }
            other => panic!("expected Or, got {other:?}"),
        }
    }

    /// Single-property form ("greater than this creature's power") parses as
    /// a single `QuantityComparison`, not an `Or`.
    #[test]
    fn test_parse_source_qualified_mana_spent_condition() {
        let (rest, c) = parse_inner_condition("mana from a treasure was spent to cast it").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert_eq!(comparator, Comparator::GT);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 0 });
                match lhs {
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ManaSpentToCast {
                                scope: CastManaObjectScope::TriggeringSpell,
                                metric: CastManaSpentMetric::FromSource { source_filter },
                            },
                    } => match source_filter {
                        TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                            assert_eq!(type_filters, vec![TypeFilter::Subtype("Treasure".into())]);
                        }
                        other => panic!("expected typed source filter, got {other:?}"),
                    },
                    other => panic!("expected source-qualified mana spent lhs, got {other:?}"),
                }
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_source_qualified_mana_spent_threshold() {
        let (rest, c) =
            parse_inner_condition("three or more mana from creatures was spent to cast it")
                .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 3 });
                match lhs {
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ManaSpentToCast {
                                scope: CastManaObjectScope::TriggeringSpell,
                                metric: CastManaSpentMetric::FromSource { source_filter },
                            },
                    } => match source_filter {
                        TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                            assert_eq!(type_filters, vec![TypeFilter::Creature]);
                        }
                        other => panic!("expected typed source filter, got {other:?}"),
                    },
                    other => panic!("expected source-qualified mana spent lhs, got {other:?}"),
                }
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_condition_mana_spent_vs_self_power_only() {
        let (rest, c) = parse_condition(
            "if the amount of mana you spent is greater than this creature's power",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert_eq!(
                    lhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ManaSpentToCast {
                            scope: crate::types::ability::CastManaObjectScope::TriggeringSpell,
                            metric: crate::types::ability::CastManaSpentMetric::Total
                        }
                    }
                );
                assert_eq!(comparator, Comparator::GT);
                assert_eq!(
                    rhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: crate::types::ability::ObjectScope::Source
                        }
                    }
                );
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    /// CR 601.2h: "N or more mana was spent to cast that spell" — threshold
    /// intervening-if used by Expressive Firedancer's Opus rider, Mana Sculpt's
    /// Wizard-gated delayed mana, and any future card gating on triggering-spell
    /// cost magnitude.
    #[test]
    fn test_parse_condition_mana_spent_threshold_that_spell() {
        let (rest, c) =
            parse_condition("if five or more mana was spent to cast that spell").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert_eq!(
                    lhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ManaSpentToCast {
                            scope: crate::types::ability::CastManaObjectScope::TriggeringSpell,
                            metric: crate::types::ability::CastManaSpentMetric::Total
                        }
                    }
                );
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 5 });
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    /// "or less" inverse form produces LE comparator.
    #[test]
    fn test_parse_condition_mana_spent_threshold_or_less() {
        let (rest, c) = parse_condition("if three or less mana was spent to cast it").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                comparator, rhs, ..
            } => {
                assert_eq!(comparator, Comparator::LE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 3 });
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    // ── CR 122.1: `parse_source_has_counters` ──────────────────────────
    //
    // Building-block tests for the counter-gated static condition family.
    // Covers the full grammar axis: subject × quantity × counter-type-or-bare.

    use crate::types::counter::{CounterMatch, CounterType};

    // --- Bare-counter (CounterMatch::Any) variants ---------------------------

    #[test]
    fn has_counters_bare_any_tilde_subject() {
        // Demon Wall: "as long as ~ has a counter on it"
        let (rest, c) = parse_inner_condition("~ has a counter on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::Any,
                minimum: 1,
                maximum: None,
            }
        );
    }

    #[test]
    fn has_counters_bare_any_this_creature_subject() {
        // Printed Oracle form for Demon Wall after "as long as " is consumed.
        let (rest, c) = parse_inner_condition("this creature has a counter on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::Any,
                minimum: 1,
                maximum: None,
            }
        );
    }

    #[test]
    fn has_counters_no_counters_bare() {
        // "no counters on it" → minimum 0, maximum 0 (i.e. must have zero).
        let (rest, c) = parse_inner_condition("~ has no counters on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::Any,
                minimum: 0,
                maximum: Some(0),
            }
        );
    }

    /// Bound-pronoun subject `"it "` — used by `parse_for_as_long_as_condition`
    /// in oracle_effect (duration clauses like "has flying for as long as it
    /// has a flood counter on it").
    #[test]
    fn has_counters_pronoun_subject_it_any() {
        let (rest, c) = parse_source_has_counters("it has a counter on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::Any,
                minimum: 1,
                maximum: None,
            }
        );
    }

    // --- Typed-counter (CounterMatch::OfType) variants -----------------------

    /// Unleash / Outlast body: "it has a +1/+1 counter on it" (article → min 1).
    #[test]
    fn test_parse_condition_it_has_a_p1p1_counter() {
        let (rest, c) = parse_condition("as long as it has a +1/+1 counter on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                minimum: 1,
                maximum: None,
            }
        );
    }

    /// "~" subject form — leveler-style source reference.
    #[test]
    fn test_parse_condition_tilde_has_a_counter() {
        let (rest, c) = parse_condition("as long as ~ has a +1/+1 counter on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                minimum: 1,
                maximum: None,
            }
        );
    }

    #[test]
    fn has_counters_typed_loyalty() {
        let (rest, c) = parse_inner_condition("~ has a loyalty counter on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Loyalty),
                minimum: 1,
                maximum: None,
            }
        );
    }

    /// Primordial Hydra's trample gate: "it has ten or more +1/+1 counters on it".
    #[test]
    fn test_parse_condition_it_has_ten_or_more_p1p1_counters() {
        let (rest, c) =
            parse_condition("as long as it has ten or more +1/+1 counters on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                minimum: 10,
                maximum: None,
            }
        );
    }

    /// Angelic Cub form: "this creature has three or more +1/+1 counters on it".
    #[test]
    fn test_parse_condition_this_creature_has_three_or_more_p1p1() {
        let (rest, c) =
            parse_condition("as long as this creature has three or more +1/+1 counters on it")
                .unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                minimum: 3,
                maximum: None,
            }
        );
    }

    #[test]
    fn has_counters_typed_plus_one_plus_one_n_or_more() {
        let (rest, c) = parse_inner_condition("~ has 3 or more +1/+1 counters on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                minimum: 3,
                maximum: None,
            }
        );
    }

    #[test]
    fn has_counters_one_or_more_typed() {
        let (rest, c) = parse_inner_condition("~ has one or more +1/+1 counters on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                minimum: 1,
                maximum: None,
            }
        );
    }

    /// Named counter type: "it has three or more charge counters on it".
    #[test]
    fn test_parse_condition_it_has_three_or_more_charge_counters() {
        let (rest, c) =
            parse_condition("as long as it has three or more charge counters on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Generic("charge".to_string())),
                minimum: 3,
                maximum: None,
            }
        );
    }

    #[test]
    fn has_counters_pronoun_subject_it_typed_generic() {
        // "flood" is a Generic counter type — verifies the terminator-anchored
        // parser in `parse_typed_counter_noun` falls through to Generic via
        // the canonical mapping rather than failing on unknown named types.
        let (rest, c) = parse_source_has_counters("it has a flood counter on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Generic("flood".to_string())),
                minimum: 1,
                maximum: None,
            }
        );
    }

    /// "exactly N" variant.
    #[test]
    fn test_parse_condition_it_has_exactly_two_counters() {
        let (rest, c) =
            parse_condition("as long as it has exactly 2 +1/+1 counters on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                minimum: 2,
                maximum: Some(2),
            }
        );
    }

    /// "N or fewer" variant.
    #[test]
    fn test_parse_condition_it_has_two_or_fewer_counters() {
        let (rest, c) =
            parse_condition("as long as it has two or fewer +1/+1 counters on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                minimum: 0,
                maximum: Some(2),
            }
        );
    }

    /// "no" variant — zero counters (min 0, max 0).
    #[test]
    fn test_parse_condition_it_has_no_counters() {
        let (rest, c) = parse_condition("as long as it has no +1/+1 counters on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                minimum: 0,
                maximum: Some(0),
            }
        );
    }

    /// CR 603.4: Valakut's "at least five other Mountains" must parse as an
    /// `ObjectCount >= 5` with `controller = You`, `Subtype::Mountain`, and
    /// `FilterProp::Another` (rewritten to `OtherThanTriggerObject` by the
    /// trigger bridge). The "at least" idiom shares a parse path with "N or
    /// more" via `parse_ge_threshold`.
    #[test]
    fn test_parse_condition_you_control_at_least_n_other_type() {
        use crate::types::ability::{FilterProp, TypedFilter};
        let (_rest, c) =
            parse_inner_condition("you control at least five other mountains").unwrap();
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 5 },
            } => match filter {
                TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::You),
                    properties,
                    ..
                }) => {
                    assert!(
                        properties.iter().any(|p| matches!(p, FilterProp::Another)),
                        "expected Another prop, got {properties:?}"
                    );
                }
                other => panic!("expected Typed filter You, got {other:?}"),
            },
            other => panic!("expected ObjectCount GE 5, got {other:?}"),
        }
    }

    /// CR 109.3 + CR 603.4: Defense of the Heart's "if an opponent controls
    /// three or more creatures" parses as `ObjectCount(controller=Opponent,
    /// Creature) >= 3`.
    #[test]
    fn test_parse_condition_an_opponent_controls_n_or_more_type() {
        use crate::types::ability::TypedFilter;
        let (_rest, c) =
            parse_inner_condition("an opponent controls three or more creatures").unwrap();
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            } => match filter {
                TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::Opponent),
                    ..
                }) => {}
                other => panic!("expected Typed filter Opponent, got {other:?}"),
            },
            other => panic!("expected ObjectCount GE 3, got {other:?}"),
        }
    }

    /// CR 109.3: "an opponent controls at least N <filter>" must share the
    /// threshold idiom with "N or more".
    #[test]
    fn test_parse_condition_an_opponent_controls_at_least_n_type() {
        let (_rest, c) =
            parse_inner_condition("an opponent controls at least two artifacts").unwrap();
        assert!(matches!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { .. }
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            }
        ));
    }

    /// CR 119.3 + CR 603.4: Y'shtola's "a player lost 4 or more life this
    /// turn" must parse to `LifeLostThisTurn { player: AllPlayers { Max } } ≥ 4`
    /// — the per-player-max semantic, not the cross-opponent sum semantic of
    /// `Opponent { Sum }`.
    #[test]
    fn test_parse_condition_a_player_lost_four_or_more_life() {
        let (rest, c) = parse_inner_condition("a player lost 4 or more life this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeLostThisTurn {
                        player: PlayerScope::AllPlayers {
                            aggregate: AggregateFunction::Max,
                        },
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            }
        );
    }

    #[test]
    fn test_parse_condition_an_opponent_lost_two_or_more_life() {
        let (rest, c) = parse_inner_condition("an opponent lost 2 or more life this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeLostThisTurn {
                        player: PlayerScope::Opponent {
                            aggregate: AggregateFunction::Max,
                        },
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            }
        );
    }

    /// CR 119.3 + CR 603.4: Same idiom must parse via the "if " prefix
    /// (intervening-if reading) — confirming `parse_condition` reaches
    /// `parse_player_lost_life_this_turn` through the dispatcher.
    #[test]
    fn test_parse_condition_if_a_player_lost_two_or_more_life() {
        let (rest, c) = parse_condition("if a player lost 2 or more life this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeLostThisTurn {
                        player: PlayerScope::AllPlayers {
                            aggregate: AggregateFunction::Max,
                        },
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            }
        );
    }

    /// CR 119.3 + CR 603.4: The "at least N" idiom must share the threshold
    /// alternative with "N or more" — `parse_ge_threshold` is the single
    /// authority. Future cards using the synonym compose without per-card
    /// code.
    #[test]
    fn test_parse_condition_a_player_lost_at_least_n_life() {
        let (rest, c) = parse_inner_condition("a player lost at least 5 life this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeLostThisTurn {
                        player: PlayerScope::AllPlayers {
                            aggregate: AggregateFunction::Max,
                        },
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 5 },
            }
        );
    }

    // ---- parse_zone_changed_this_way_clause ----
    //
    // CR 400.7 + CR 608.2c: this is the shared "noun-anaphoric this way"
    // combinator — every present/past tense + every verb listed in the
    // function's `alt` chain must round-trip.

    /// CR 614.1a-style past-tense "was destroyed this way" — the original
    /// shape used by Shredder's Technique. Establishes the negative-control
    /// baseline before extending to present tense / multi-word verbs.
    #[test]
    fn test_zone_changed_this_way_was_destroyed_top_level_type() {
        let (rest, (filter, negated)) = parse_zone_changed_this_way_clause(
            "an enchantment was destroyed this way, you lose 2 life",
        )
        .unwrap();
        assert_eq!(rest, ", you lose 2 life");
        assert!(!negated);
        match filter {
            TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                assert_eq!(type_filters, vec![TypeFilter::Enchantment]);
            }
            other => panic!("expected Typed Enchantment, got {other:?}"),
        }
    }

    /// CR 303.4f / CR 301.5b: present-tense "is put onto the battlefield"
    /// with subtype filter — the Armored Skyhunter / Vault 101 / Quest for
    /// the Holy Relic / Stonehewer Giant case.
    #[test]
    fn test_zone_changed_this_way_is_put_onto_battlefield_equipment() {
        let (rest, (filter, negated)) = parse_zone_changed_this_way_clause(
            "an equipment is put onto the battlefield this way, you may attach it to a creature you control",
        )
        .unwrap();
        assert_eq!(rest, ", you may attach it to a creature you control");
        assert!(!negated);
        // Subtype Equipment must round-trip (parse_type_phrase canonicalizes
        // "equipment" → Subtype::Equipment via the oracle subtype dictionary).
        match filter {
            TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                assert!(
                    type_filters
                        .iter()
                        .any(|f| matches!(f, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Equipment"))),
                    "expected Subtype Equipment, got {type_filters:?}"
                );
            }
            other => panic!("expected Typed Equipment, got {other:?}"),
        }
    }

    /// CR 303.4f: Aura subtype mirrors the Equipment branch — same combinator.
    #[test]
    fn test_zone_changed_this_way_is_put_onto_battlefield_aura() {
        let (rest, (filter, negated)) = parse_zone_changed_this_way_clause(
            "an aura is put onto the battlefield this way, do something",
        )
        .unwrap();
        assert_eq!(rest, ", do something");
        assert!(!negated);
        match filter {
            TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                assert!(
                    type_filters.iter().any(
                        |f| matches!(f, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Aura"))
                    ),
                    "expected Subtype Aura, got {type_filters:?}"
                );
            }
            other => panic!("expected Typed Aura, got {other:?}"),
        }
    }

    /// CR 400.7: "wasn't" negation must flip the boolean — used by future
    /// "if a creature wasn't destroyed this way" patterns.
    #[test]
    fn test_zone_changed_this_way_wasnt_negated() {
        let (rest, (_filter, negated)) =
            parse_zone_changed_this_way_clause("a creature wasn't destroyed this way, do x")
                .unwrap();
        assert_eq!(rest, ", do x");
        assert!(negated);
    }

    /// Every imperative verb in the `alt` chain must round-trip; this guards
    /// against regression when someone reorders the alternatives.
    #[test]
    fn test_zone_changed_this_way_each_imperative_verb() {
        for verb in &[
            "destroyed",
            "exiled",
            "sacrificed",
            "returned",
            "discarded",
            "milled",
            "countered",
        ] {
            let input = format!("a creature was {verb} this way, x");
            let (rest, (_filter, negated)) = parse_zone_changed_this_way_clause(&input)
                .unwrap_or_else(|e| {
                    panic!("verb {verb} failed to parse: {e:?}");
                });
            assert_eq!(rest, ", x", "verb {verb} produced wrong remainder");
            assert!(!negated);
        }
    }

    /// Negative: rejects unrecognized type phrases (returns `Any`) — the
    /// caller should not get a synthetic match.
    #[test]
    fn test_zone_changed_this_way_rejects_unrecognized_type() {
        // "a thing" — type_phrase returns Any → combinator must error.
        assert!(parse_zone_changed_this_way_clause("a thing was destroyed this way").is_err());
    }

    // ---------------------------------------------------------------------
    // CR 122.1 + CR 608.2c: parse_there_are_counters_on_source
    // ---------------------------------------------------------------------

    /// Gemstone Mine and the depletion-land cycle: "if there are no <type>
    /// counters on ~" — the canonical motivating case.
    #[test]
    fn test_there_are_no_typed_counters_on_self() {
        let (rest, c) = parse_condition("if there are no mining counters on ~, sacrifice").unwrap();
        assert_eq!(rest, ", sacrifice");
        match c {
            StaticCondition::HasCounters {
                counters,
                minimum,
                maximum,
            } => {
                assert_eq!(minimum, 0);
                assert_eq!(maximum, Some(0));
                match counters {
                    CounterMatch::OfType(ct) => assert_eq!(ct.as_str(), "mining"),
                    other => panic!("expected OfType(mining), got {other:?}"),
                }
            }
            other => panic!("expected HasCounters, got {other:?}"),
        }
    }

    /// Budoka Pupil / Callow Jushi: "if there are two or more ki counters on
    /// this creature, you may flip it." Source subject is the long form.
    #[test]
    fn test_there_are_n_or_more_counters_on_this_creature() {
        let (rest, c) =
            parse_condition("if there are two or more ki counters on this creature, flip").unwrap();
        assert_eq!(rest, ", flip");
        match c {
            StaticCondition::HasCounters {
                counters,
                minimum,
                maximum,
            } => {
                assert_eq!(minimum, 2);
                assert_eq!(maximum, None);
                match counters {
                    CounterMatch::OfType(ct) => assert_eq!(ct.as_str(), "ki"),
                    other => panic!("expected OfType(ki), got {other:?}"),
                }
            }
            other => panic!("expected HasCounters, got {other:?}"),
        }
    }

    /// Brass's Tunnel-Grinder: "Then if there are three or more bore counters
    /// on it" — bare "it" continuation form.
    #[test]
    fn test_there_are_n_or_more_counters_on_it() {
        let (rest, c) =
            parse_condition("if there are three or more bore counters on it, transform").unwrap();
        assert_eq!(rest, ", transform");
        match c {
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(ct),
                minimum,
                maximum,
            } => {
                assert_eq!(minimum, 3);
                assert_eq!(maximum, None);
                assert_eq!(ct.as_str(), "bore");
            }
            other => panic!("expected HasCounters OfType, got {other:?}"),
        }
    }

    /// "this aura" subject (Tourach's Gate): "if there are no time counters
    /// on this Aura". Lowercased before parsing.
    #[test]
    fn test_there_are_no_counters_on_this_aura() {
        let (rest, c) =
            parse_condition("if there are no time counters on this aura, sacrifice").unwrap();
        assert_eq!(rest, ", sacrifice");
        assert!(matches!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(_),
                minimum: 0,
                maximum: Some(0),
            }
        ));
    }

    /// "this enchantment" subject (Celestial Convergence).
    #[test]
    fn test_there_are_no_counters_on_this_enchantment() {
        let (rest, c) =
            parse_condition("if there are no omen counters on this enchantment, win the game")
                .unwrap();
        assert_eq!(rest, ", win the game");
        assert!(matches!(
            c,
            StaticCondition::HasCounters {
                minimum: 0,
                maximum: Some(0),
                ..
            }
        ));
    }

    /// "as long as" prefix should also flow through the same combinator.
    #[test]
    fn test_as_long_as_there_are_counters() {
        let (rest, c) =
            parse_condition("as long as there are five or more growth counters on ~, pump")
                .unwrap();
        assert_eq!(rest, ", pump");
        match c {
            StaticCondition::HasCounters {
                minimum, maximum, ..
            } => {
                assert_eq!(minimum, 5);
                assert_eq!(maximum, None);
            }
            other => panic!("expected HasCounters, got {other:?}"),
        }
    }

    /// Bare "counter[s]" (no type token) → CounterMatch::Any.
    #[test]
    fn test_there_are_no_counters_any_type() {
        let (rest, c) = parse_condition("if there are no counters on ~, sacrifice").unwrap();
        assert_eq!(rest, ", sacrifice");
        assert!(matches!(
            c,
            StaticCondition::HasCounters {
                counters: CounterMatch::Any,
                minimum: 0,
                maximum: Some(0),
            }
        ));
    }

    // -- "have total {power|toughness|mana value} N or {greater|less}" predicate --
    //
    // CR 107.3e + CR 208.1 + CR 202.3: Building-block predicate for
    // aggregate-property thresholds across a filter (Sum function). Single
    // combinator parameterized over `ObjectProperty` so it covers total power,
    // total toughness, and total mana value uniformly. The motivating card is
    // Betor, Kin to All ("if creatures you control have total toughness 10 or
    // greater"), but the building block extends to any "<filter> have total
    // <property> <comparator> N" phrase.
    fn assert_total_property_ge(
        text: &str,
        expected_property: AggregateProperty,
        expected_threshold: i32,
    ) {
        let (rest, c) = parse_inner_condition(text).unwrap_or_else(|e| {
            panic!("parse_inner_condition({text:?}) failed: {e:?}");
        });
        assert_eq!(rest, "", "input fully consumed for {text:?}");
        match c {
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(
                    rhs,
                    QuantityExpr::Fixed {
                        value: expected_threshold
                    }
                );
                match lhs {
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::Aggregate {
                                function,
                                property,
                                filter,
                            },
                    } => {
                        assert_eq!(function, AggregateFunction::Sum);
                        assert_eq!(property, expected_property.0);
                        match filter {
                            TargetFilter::Typed(t) => {
                                assert_eq!(t.controller, Some(ControllerRef::You));
                                assert!(t.type_filters.contains(&TypeFilter::Creature));
                            }
                            other => panic!(
                                "expected Typed(Creature, controller=You) filter, got {other:?}"
                            ),
                        }
                    }
                    other => panic!("expected QuantityRef::Aggregate, got {other:?}"),
                }
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    /// Tiny newtype to avoid importing `ObjectProperty` at every call site of
    /// the helper without leaking `crate::types::ability::ObjectProperty`
    /// directly into the test surface.
    struct AggregateProperty(crate::types::ability::ObjectProperty);

    /// CR 208.1 + CR 107.3e: Betor's first tier — "if creatures you control
    /// have total toughness 10 or greater" must parse to a Sum-Toughness
    /// QuantityComparison so the trigger-level intervening-if hoist works.
    #[test]
    fn test_creatures_you_control_have_total_toughness_ge() {
        assert_total_property_ge(
            "creatures you control have total toughness 10 or greater",
            AggregateProperty(crate::types::ability::ObjectProperty::Toughness),
            10,
        );
    }

    /// CR 208.1: Betor's second tier — same shape with threshold 20.
    #[test]
    fn test_creatures_you_control_have_total_toughness_ge_20() {
        assert_total_property_ge(
            "creatures you control have total toughness 20 or greater",
            AggregateProperty(crate::types::ability::ObjectProperty::Toughness),
            20,
        );
    }

    /// CR 208.1: Betor's third tier — same shape with threshold 40.
    #[test]
    fn test_creatures_you_control_have_total_toughness_ge_40() {
        assert_total_property_ge(
            "creatures you control have total toughness 40 or greater",
            AggregateProperty(crate::types::ability::ObjectProperty::Toughness),
            40,
        );
    }

    /// CR 208.1: Building-block coverage — total power must parse via the same
    /// combinator (parameterization, not proliferation).
    #[test]
    fn test_creatures_you_control_have_total_power_ge() {
        assert_total_property_ge(
            "creatures you control have total power 7 or greater",
            AggregateProperty(crate::types::ability::ObjectProperty::Power),
            7,
        );
    }

    /// CR 202.3: Building-block coverage — total mana value via the same combinator.
    #[test]
    fn test_creatures_you_control_have_total_mana_value_ge() {
        assert_total_property_ge(
            "creatures you control have total mana value 12 or greater",
            AggregateProperty(crate::types::ability::ObjectProperty::ManaValue),
            12,
        );
    }

    /// "or more" alias for the GE comparator must parse identically — Oracle
    /// uses both "or greater" and "or more" interchangeably for thresholds.
    #[test]
    fn test_creatures_you_control_have_total_toughness_or_more_alias() {
        assert_total_property_ge(
            "creatures you control have total toughness 10 or more",
            AggregateProperty(crate::types::ability::ObjectProperty::Toughness),
            10,
        );
    }
}
