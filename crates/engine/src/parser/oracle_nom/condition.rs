//! Condition combinators for Oracle text parsing.
//!
//! Parses condition phrases: "if [condition]", "as long as [condition]",
//! "unless [condition]" into typed `StaticCondition` values.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::bytes::complete::take_until;
use nom::combinator::{map, value};
use nom::sequence::preceded;
use nom::Parser;

use super::error::OracleResult;
use super::primitives::{parse_mana_cost, parse_number};
use crate::parser::oracle_target::parse_type_phrase;
use crate::types::ability::{
    Comparator, ControllerRef, CountScope, QuantityExpr, QuantityRef, StaticCondition, TargetFilter,
};

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
        parse_there_are_conditions,
        parse_entered_this_turn,
        parse_youve_this_turn,
        parse_event_state_conditions,
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

/// CR 724.1 / CR 702.131a: Parse player-state conditions.
///
/// Handles "you're the monarch" and "you have the city's blessing".
fn parse_player_state_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        // CR 724.1: Monarch status
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
    ))
    .parse(input)
}

fn parse_opponent_poison_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("an opponent has ").parse(input)?;
    let (rest, count) = parse_number(rest)?;
    let (rest, _) = tag(" or more poison counters").parse(rest)?;
    Ok((
        rest,
        StaticCondition::OpponentPoisonAtLeast {
            count: count as u32,
        },
    ))
}

/// CR 611.2b: Compose subject × predicate for tapped/untapped.
///
/// Subject: "~ is ", "this creature is ", "this permanent is ", "this land is ",
/// "this artifact is ", "equipped creature is ", "enchanted creature is "
/// Predicate: "tapped" → SourceIsTapped, "untapped" → Not(SourceIsTapped)
fn parse_tapped_untapped(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((
        tag("~ is "),
        tag("this creature is "),
        tag("this permanent is "),
        tag("this land is "),
        tag("this artifact is "),
        tag("this enchantment is "),
        tag("equipped creature is "),
        tag("enchanted creature is "),
    ))
    .parse(input)?;
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

/// CR 611.2b: Parse source-state conditions (tapped, untapped, entered this turn).
fn parse_source_state_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        // CR 611.2b: Tapped/untapped — composed as subject × predicate.
        // Parse subject ("~ is", "this creature is", etc.) then branch on "tapped"/"untapped".
        parse_tapped_untapped,
        // CR 400.7: Entered this turn
        value(
            StaticCondition::SourceEnteredThisTurn,
            tag("~ entered the battlefield this turn"),
        ),
        parse_this_type_entered_this_turn,
        value(StaticCondition::IsRingBearer, tag("~ is your ring-bearer")),
        parse_source_is_type,
        parse_source_power_toughness_condition,
    ))
    .parse(input)
}

/// CR 608.2c: Parse "this creature/permanent is a [type]" → SourceMatchesFilter.
/// Used by leveler-style cards (Figure of Fable, Figure of Destiny) where each
/// activation level gates on the source's current subtype.
fn parse_source_is_type(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((
        tag("this creature is a "),
        tag("this creature is an "),
        tag("this permanent is a "),
        tag("this permanent is an "),
        tag("~ is a "),
        tag("~ is an "),
    ))
    .parse(input)?;
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

/// Canonical combinator: "you control N or more [type]" → QuantityComparison.
///
/// Single authority for this pattern — called from `oracle_static.rs` and
/// `oracle_trigger.rs` to avoid three-way duplication.
/// Returns the remainder after the type phrase (may be non-empty for trailing text).
pub fn parse_control_count_ge(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you control ").parse(input)?;
    let (rest, n) = parse_number(rest)?;
    let rest = rest.trim_start();
    let (rest, _) = tag("or more ").parse(rest)?;
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
    let (rest, _) =
        alt((tag("you don't control a "), tag("you don't control an "))).parse(input)?;
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
        // "a creature died this turn" (Morbid) → CreaturesDiedThisTurn >= 1
        value(
            make_quantity_ge(QuantityRef::CreaturesDiedThisTurn, 1),
            tag("a creature died this turn"),
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
    ))
    .parse(input)
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
    let (rest, _) = alt((tag("a "), tag("an "))).parse(rest)?;
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
    let (rest, _) = alt((tag("a "), tag("an "))).parse(rest)?;
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
    let (type_and_rest, _) = alt((tag("a "), tag("an "))).parse(input)?;
    let (rest, type_text) = take_until(entered_suffix).parse(type_and_rest)?;
    let (rest, _) = tag(entered_suffix).parse(rest)?;
    let (filter, _) = parse_type_phrase(type_text.trim());
    let filter = inject_controller_you(filter);
    Ok((
        rest,
        make_quantity_ge(QuantityRef::EnteredThisTurn { filter }, 1),
    ))
}

/// Parse "there are N or more [things] in your graveyard" conditions.
///
/// Covers threshold ("seven or more cards"), delirium ("four or more card types"),
/// mana values ("five or more mana values"), and typed cards ("creature cards",
/// "instant and/or sorcery cards", "land cards", "historic cards", etc.).
fn parse_there_are_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("there are ").parse(input)?;
    let (rest, n) = parse_number(rest)?;
    let (rest, _) = tag(" or more ").parse(rest)?;

    // Delirium: "card types among cards in your graveyard"
    if let Ok((rest, _)) = tag::<_, _, nom_language::error::VerboseError<&str>>(
        "card types among cards in your graveyard",
    )
    .parse(rest)
    {
        return Ok((
            rest,
            make_quantity_ge(
                QuantityRef::CardTypesInGraveyards {
                    scope: CountScope::Controller,
                },
                n,
            ),
        ));
    }

    // General: "[anything] in your graveyard" — use take_until to consume the
    // descriptor, then match the "in your graveyard" suffix.
    // Covers: "cards", "creature cards", "land cards", "instant and/or sorcery cards",
    // "permanent cards", "historic cards", "mana values among cards".
    // All map to GraveyardSize (typed qualification is a runtime concern).
    let (rest, _descriptor) =
        take_until::<_, _, nom_language::error::VerboseError<&str>>("in your graveyard")
            .parse(rest)?;
    let (rest, _) = tag("in your graveyard").parse(rest)?;
    Ok((rest, make_quantity_ge(QuantityRef::GraveyardSize, n)))
}

/// Parse "an opponent controls more [type] than you" → QuantityComparison.
/// Also handles "an opponent has more life/cards in hand than you".
///
/// These are cross-player quantity comparisons where the opponent's quantity
/// exceeds the controller's. Composed as QuantityComparison with opponent-scoped
/// refs on the LHS and controller-scoped refs on the RHS.
fn parse_opponent_comparison_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("an opponent ").parse(input)?;

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
    Ok((rest, StaticCondition::UnlessPay { cost }))
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

#[cfg(test)]
mod tests {
    use super::*;
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
                        qty: QuantityRef::CardTypesInGraveyards { .. },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            } => {}
            other => panic!("expected CardTypesInGraveyards GE 4, got {other:?}"),
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
            StaticCondition::UnlessPay { cost } => {
                assert_eq!(
                    cost,
                    ManaCost::Cost {
                        shards: vec![],
                        generic: 2
                    }
                );
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
}
