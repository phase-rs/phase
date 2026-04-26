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

use super::error::OracleResult;
use super::primitives::{parse_article, parse_mana_cost, parse_number};
use super::quantity as nom_quantity;
use crate::parser::oracle_target::parse_type_phrase;
use crate::types::ability::{
    Comparator, ControllerRef, QuantityExpr, QuantityRef, StaticCondition, TargetFilter,
};
use crate::types::counter::CounterMatch;

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
        parse_control_conditions,
        parse_opponent_poison_conditions,
        parse_opponent_comparison_conditions,
        parse_life_conditions,
        parse_zone_conditions,
        parse_there_are_counters_on_source,
        parse_there_are_conditions,
        parse_there_exists_condition,
        parse_entered_this_turn,
        parse_youve_this_turn,
        parse_event_state_conditions,
        parse_mana_spent_vs_source_pt,
        parse_mana_spent_threshold,
        parse_combat_context_conditions,
        parse_unless_pay_condition,
    ))
    .parse(input)
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
        return Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::TakeUntil),
            )],
        }));
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
    let (rest, _) = alt((
        tag("this creature is "),
        tag("this permanent is "),
        tag("~ is "),
    ))
    .parse(input)?;
    let (rest, _) = parse_article(rest)?;
    let (filter, remainder) = parse_type_phrase(rest);
    Ok((remainder, StaticCondition::SourceMatchesFilter { filter }))
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
        value(QuantityRef::SelfPower, tag("power is ")),
        value(QuantityRef::SelfToughness, tag("toughness is ")),
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
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("no cards in hand").parse(rest)
    {
        return Ok((
            rest,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize,
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            },
        ));
    }

    // "you have N or more [quantity-suffix]"
    let (rest, n) = parse_number(rest)?;

    // Try each quantity suffix
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>(" or more cards in hand").parse(rest)
    {
        return Ok((rest, make_quantity_ge(QuantityRef::HandSize, n)));
    }
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>(" or more cards in your graveyard")
            .parse(rest)
    {
        return Ok((rest, make_quantity_ge(QuantityRef::GraveyardSize, n)));
    }
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>(" or more life").parse(rest)
    {
        return Ok((rest, make_quantity_ge(QuantityRef::LifeTotal, n)));
    }
    // "you have N or less life" → LifeTotal LE N
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>(" or less life").parse(rest)
    {
        return Ok((
            rest,
            make_quantity_comparison(QuantityRef::LifeTotal, Comparator::LE, n),
        ));
    }
    // "you have N or fewer cards in hand" → HandSize LE N
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>(" or fewer cards in hand").parse(rest)
    {
        return Ok((
            rest,
            make_quantity_comparison(QuantityRef::HandSize, Comparator::LE, n),
        ));
    }

    Err(nom::Err::Error(nom_language::error::VerboseError {
        errors: vec![(
            input,
            nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
        )],
    }))
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
        return Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
            )],
        }));
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
        return Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
            )],
        }));
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
        return Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
            )],
        }));
    }
    let (filter, remainder) = parse_type_phrase(rest);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
            )],
        }));
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
        return Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
            )],
        }));
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
        return Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
            )],
        }));
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
        return Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
            )],
        }));
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

/// Inject `ControllerRef::You` into a TargetFilter produced by `parse_type_phrase`.
fn inject_controller_you(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(tf) => TargetFilter::Typed(tf.controller(ControllerRef::You)),
        other => other,
    }
}

/// Parse "your life total is N or less/greater" conditions.
///
/// Note: "you have N or more life" is handled by `parse_you_have_conditions`.
fn parse_life_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("your life total is ").parse(input)?;
    let (rest, n) = parse_number(rest)?;
    // Try "or less" then "or greater"
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>(" or less").parse(rest)
    {
        return Ok((
            rest,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal,
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
                qty: QuantityRef::LifeTotal,
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
}

/// CR 113.6b: Parse zone-based source conditions.
/// Handles all player-specific zones (graveyard, hand, library) with "your",
/// and the shared exile zone (no "your").
fn parse_zone_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    use crate::types::zones::Zone;

    alt((
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

/// Parse "you've [done X] this turn" conditions.
///
/// CR 119: Life gain/loss event conditions.
/// CR 700.13: Crime tracking.
fn parse_youve_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you've ").parse(input)?;
    alt((
        value(
            make_quantity_ge(QuantityRef::CrimesCommittedThisTurn, 1),
            tag("committed a crime this turn"),
        ),
        value(
            make_quantity_ge(QuantityRef::LifeGainedThisTurn, 1),
            tag("gained life this turn"),
        ),
        value(
            make_quantity_ge(QuantityRef::LifeLostThisTurn, 1),
            tag("lost life this turn"),
        ),
        // "you've cast another spell this turn" → SpellsCastThisTurn >= 2
        value(
            make_quantity_ge(QuantityRef::SpellsCastThisTurn { filter: None }, 2),
            alt((
                tag("cast another spell this turn"),
                tag("cast two or more spells this turn"),
            )),
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
        // "a creature died this turn" (Morbid) → CreaturesDiedThisTurn >= 1
        value(
            make_quantity_ge(QuantityRef::CreaturesDiedThisTurn, 1),
            alt((
                tag("a creature died this turn"),
                tag("a creature died under your control this turn"),
            )),
        ),
        // "a nonland permanent left the battlefield this turn" (Revolt variant)
        value(
            make_quantity_ge(QuantityRef::NonlandPermanentsLeftBattlefieldThisTurn, 1),
            tag("a nonland permanent left the battlefield this turn"),
        ),
        // "a permanent you controlled left the battlefield this turn" (Revolt)
        value(
            make_quantity_ge(QuantityRef::PermanentsLeftBattlefieldThisTurn, 1),
            alt((
                tag("a permanent you controlled left the battlefield this turn"),
                tag("a permanent left the battlefield under your control this turn"),
            )),
        ),
        // "an opponent lost life this turn"
        value(
            make_quantity_ge(QuantityRef::OpponentLifeLostThisTurn, 1),
            alt((
                tag("an opponent lost life this turn"),
                tag("that player lost life this turn"),
            )),
        ),
        // CR 119.3 + CR 603.4: "a player lost N or more life this turn"
        // (Y'shtola, Night's Blessed; Knight of the Ebon Legion). The "a
        // player" quantifier covers controller + opponents; the threshold
        // semantic is "any single player crossed N", not "sum across
        // players" — see QuantityRef::MaxLifeLostThisTurnAcrossPlayers
        // doc comment.
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
        value(QuantityRef::SelfPower, tag("power")),
        value(QuantityRef::SelfToughness, tag("toughness")),
    ))
    .parse(rest)?;
    // Optional " or <other property>" disjunction — natural-language OR.
    let (rest, second) = opt(preceded(
        tag(" or "),
        alt((
            value(QuantityRef::SelfPower, tag("power")),
            value(QuantityRef::SelfToughness, tag("toughness")),
        )),
    ))
    .parse(rest)?;

    let lhs = QuantityExpr::Ref {
        qty: QuantityRef::ManaSpentOnTriggeringSpell,
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
/// `ManaSpentOnTriggeringSpell` that bridges to `TriggerCondition::QuantityComparison`
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
                qty: QuantityRef::ManaSpentOnTriggeringSpell,
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
        return Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
            )],
        }));
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
        match v {
            "gained" => Some(QuantityRef::LifeGainedThisTurn),
            "lost" => Some(QuantityRef::LifeLostThisTurn),
            _ => None,
        }
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

    Err(nom::Err::Error(nom_language::error::VerboseError {
        errors: vec![(
            input,
            nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
        )],
    }))
}

/// Parse "you gained [N or more] life this turn".
fn parse_you_gained_life_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("you gained "), tag("you've gained "))).parse(input)?;
    // Try "N or more life this turn"
    if let Ok((after_n, n)) = parse_number(rest) {
        let after_n = after_n.trim_start();
        if let Ok((rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>("or more life this turn")
                .parse(after_n)
        {
            return Ok((rest, make_quantity_ge(QuantityRef::LifeGainedThisTurn, n)));
        }
    }
    // "life this turn" (minimum 1)
    let (rest, _) = tag("life this turn").parse(rest)?;
    Ok((rest, make_quantity_ge(QuantityRef::LifeGainedThisTurn, 1)))
}

/// CR 119.3 + CR 603.4: Parse "a player lost N or more life this turn".
///
/// Y'shtola, Night's Blessed and Knight of the Ebon Legion use this idiom for
/// the intervening-`if` clause of a phase trigger. The "a player" quantifier
/// covers controller + opponents (not just opponents), and the per-player max
/// semantic is enforced by `QuantityRef::MaxLifeLostThisTurnAcrossPlayers`
/// (one player must individually have lost ≥ N — not the sum across players).
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
        make_quantity_ge(QuantityRef::MaxLifeLostThisTurnAcrossPlayers, n),
    ))
}

/// Parse "you cast another spell this turn" / "you cast a [type] spell this turn".
fn parse_you_cast_spell_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("you cast "), tag("you've cast "))).parse(input)?;
    // "another spell this turn" → >= 2
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("another spell this turn").parse(rest)
    {
        return Ok((
            rest,
            make_quantity_ge(QuantityRef::SpellsCastThisTurn { filter: None }, 2),
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
                        filter: Some(filter),
                    },
                    1,
                ),
            ));
        }
    }
    Err(nom::Err::Error(nom_language::error::VerboseError {
        errors: vec![(
            input,
            nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
        )],
    }))
}

/// Parse "two or more spells were cast last turn" / "a player cast two or more spells last turn".
fn parse_spells_cast_last_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    // "two or more spells were cast last turn"
    if let Ok((rest, _)) = tag::<_, _, nom_language::error::VerboseError<&str>>(
        "two or more spells were cast last turn",
    )
    .parse(input)
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

/// Parse "you [put/ve put] [a counter/one or more counters] on a [permanent/creature] this turn".
/// Composes prefix × quantity × target × suffix via chained combinators.
fn parse_counter_added_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("you put "), tag("you've put "))).parse(input)?;
    let (rest, _) = alt((tag("one or more counters"), tag("a counter"))).parse(rest)?;
    let (rest, _) = tag(" on a ").parse(rest)?;
    let (rest, _) = alt((tag("permanent"), tag("creature"))).parse(rest)?;
    let (rest, _) = tag(" this turn").parse(rest)?;
    Ok((rest, make_quantity_ge(QuantityRef::CounterAddedThisTurn, 1)))
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
                QuantityRef::SpellsCastThisTurn { filter: None },
                Comparator::EQ,
                0,
            ),
            tag("cast a spell this turn"),
        ),
        value(
            make_quantity_comparison(QuantityRef::LifeLostThisTurn, Comparator::EQ, 0),
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
    Err(nom::Err::Error(nom_language::error::VerboseError {
        errors: vec![(
            input,
            nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
        )],
    }))
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
        if let Ok((type_and_rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>("or more ").parse(after_n)
        {
            if let Ok((rest, type_text)) =
                take_until::<_, _, nom_language::error::VerboseError<&str>>(entered_suffix)
                    .parse(type_and_rest)
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
    if let Ok((rest2, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("controls ").parse(rest)
    {
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
    if let Ok((rest2, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("controls more ").parse(rest)
    {
        if let Ok((rest3, type_text)) =
            take_until::<_, _, nom_language::error::VerboseError<&str>>(" than you").parse(rest2)
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
    if let Ok((rest2, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("has more life than you").parse(rest)
    {
        return Ok((
            rest2,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::OpponentLifeTotal,
                },
                comparator: Comparator::GT,
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal,
                },
            },
        ));
    }

    // "an opponent has more cards in hand than you"
    if let Ok((rest2, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("has more cards in hand than you")
            .parse(rest)
    {
        return Ok((
            rest2,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::OpponentHandSize,
                },
                comparator: Comparator::GT,
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize,
                },
            },
        ));
    }

    Err(nom::Err::Error(nom_language::error::VerboseError {
        errors: vec![(
            input,
            nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
        )],
    }))
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
        return Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
            )],
        }));
    }

    // `parse_type_phrase` returns a slice of `rest`; trim any leading whitespace
    // it left between the type phrase and the tense verb so the next `tag`
    // matches cleanly.
    let after_filter = after_filter.trim_start();

    // tense: "is" | "was" | "wasn't" | "is not" | "was not" | "isn't"
    let (rest, negated) = alt((
        value(
            true,
            tag::<_, _, nom_language::error::VerboseError<&str>>("wasn't "),
        ),
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
        tag::<_, _, nom_language::error::VerboseError<&str>>("put onto the battlefield"),
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
    use crate::types::ability::{TypeFilter, TypedFilter};
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
                        qty: QuantityRef::LifeGainedThisTurn,
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {}
            other => panic!("expected LifeGainedThisTurn GE 1, got {other:?}"),
        }
    }

    #[test]
    fn test_youve_lost_life() {
        let (rest, c) = parse_inner_condition("you've lost life this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::LifeLostThisTurn,
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
                        qty: QuantityRef::LifeTotal,
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
                        qty: QuantityRef::BasicLandTypeCount,
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
                        qty: QuantityRef::DistinctCardTypesInZone { .. },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            } => {}
            other => panic!("expected DistinctCardTypesInZone GE 4, got {other:?}"),
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
                        qty: QuantityRef::DistinctCardTypesExiledBySource,
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            } => {}
            other => panic!("expected DistinctCardTypesExiledBySource GE 4, got {other:?}"),
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
                        qty: QuantityRef::LifeTotal,
                    },
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 5 },
            } => {}
            other => panic!("expected LifeTotal LE 5, got {other:?}"),
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
                        qty: QuantityRef::HandSize,
                    },
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 2 },
            } => {}
            other => panic!("expected HandSize LE 2, got {other:?}"),
        }
    }

    // -- Opponent comparison conditions --

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
                        qty: QuantityRef::OpponentLifeTotal,
                    },
                comparator: Comparator::GT,
                rhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::LifeTotal,
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
                        qty: QuantityRef::OpponentHandSize,
                    },
                comparator: Comparator::GT,
                rhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::HandSize,
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
                        qty: QuantityRef::SelfPower,
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
                        qty: QuantityRef::SelfPower,
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
                        qty: QuantityRef::SpellsCastThisTurn { filter: None }
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
                        qty: QuantityRef::LifeLostThisTurn
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
                        qty: QuantityRef::NonlandPermanentsLeftBattlefieldThisTurn
                    }
                ));
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 1 });
            }
            _ => panic!("expected QuantityComparison, got {c:?}"),
        }
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
                        qty: QuantityRef::CreaturesDiedThisTurn
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
                    qty: QuantityRef::ManaSpentOnTriggeringSpell,
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
                assert!(pt_refs.contains(&QuantityRef::SelfPower));
                assert!(pt_refs.contains(&QuantityRef::SelfToughness));
            }
            other => panic!("expected Or, got {other:?}"),
        }
    }

    /// Single-property form ("greater than this creature's power") parses as
    /// a single `QuantityComparison`, not an `Or`.
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
                        qty: QuantityRef::ManaSpentOnTriggeringSpell
                    }
                );
                assert_eq!(comparator, Comparator::GT);
                assert_eq!(
                    rhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::SelfPower
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
                        qty: QuantityRef::ManaSpentOnTriggeringSpell
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
    /// turn" must parse to MaxLifeLostThisTurnAcrossPlayers ≥ 4 — the
    /// per-player-max semantic, not the cross-opponent sum semantic of
    /// `OpponentLifeLostThisTurn`.
    #[test]
    fn test_parse_condition_a_player_lost_four_or_more_life() {
        let (rest, c) = parse_inner_condition("a player lost 4 or more life this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::MaxLifeLostThisTurnAcrossPlayers,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
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
                    qty: QuantityRef::MaxLifeLostThisTurnAcrossPlayers,
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
                    qty: QuantityRef::MaxLifeLostThisTurnAcrossPlayers,
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
}
