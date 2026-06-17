//! Condition combinators for Oracle text parsing.
//!
//! Parses condition phrases: "if [condition]", "as long as [condition]",
//! "unless [condition]" into typed `StaticCondition` values.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::bytes::complete::take_until;
use nom::character::complete::multispace1;
use nom::combinator::{map, opt, value};
use nom::multi::many0;
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::error::{oracle_err, OracleError, OracleResult};
use super::primitives::{
    parse_article, parse_color, parse_keyword_name, parse_mana_cost, parse_number,
};
use super::quantity as nom_quantity;
use crate::parser::oracle_target::{
    cast_capable_zones_except, parse_type_phrase, parse_zone_suffix, parse_zone_word,
    peek_zone_boundary,
};
use crate::parser::oracle_util::parse_subtype;
use crate::types::ability::{
    AbilityCondition, AggregateFunction, CastManaObjectScope, CastManaSpentMetric,
    CommanderOwnership, Comparator, ControllerRef, CountScope, DamageGroupKey, DamageKindFilter,
    FilterProp, ObjectProperty, ObjectScope, PlayerFilter, PlayerRelation, PlayerScope,
    QuantityExpr, QuantityRef, SharedQuality, StaticCondition, TargetFilter, TypeFilter,
    TypedFilter, ZoneRef,
};
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::events::PlayerActionKind;
use crate::types::game_state::DayNight;
use crate::types::keywords::Keyword;
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
    alt((parse_condition_disjunction, parse_single_inner_condition)).parse(input)
}

/// CR 608.2c: "<condition A> or <condition B>" — a natural-language disjunction
/// of two game-state conditions (Plasma Bolt's Void clause: "a nonland
/// permanent left the battlefield this turn or a spell was warped this turn").
/// Each side is parsed by the non-disjunction dispatcher (`parse_single_inner_
/// condition`) to avoid left-recursion, and the result is wrapped in the
/// existing `StaticCondition::Or` combinator. Tried before the single-condition
/// dispatcher so the longer `A or B` phrase wins.
fn parse_condition_disjunction(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, lhs) = parse_single_inner_condition(input)?;
    let (rest, _) = tag(" or ").parse(rest)?;
    let (rest, rhs) = parse_single_inner_condition(rest)?;
    Ok((
        rest,
        StaticCondition::Or {
            conditions: vec![lhs, rhs],
        },
    ))
}

fn parse_single_inner_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        parse_state_presence_conditions,
        parse_event_history_conditions,
        parse_resolution_context_conditions,
    ))
    .parse(input)
}

fn parse_state_presence_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        parse_turn_conditions,
        // CR 208.1 + CR 603.4 + CR 109.3: Superlative-comparison gate
        // ("if its power is greater than each other creature's power" /
        // "if it has the greatest power among creatures on the battlefield").
        // Must precede `parse_source_state_conditions` so the longer phrase
        // wins over the fixed-N "its power is N or greater" combinator inside
        // that group (which only matches numeric thresholds).
        parse_subject_property_superlative_comparison,
        parse_attached_object_is_filter_condition,
        parse_recipient_is_filter_condition,
        parse_source_state_conditions,
        parse_player_state_conditions,
        parse_you_have_conditions,
        parse_that_player_has_conditions,
        // CR 201.2 + CR 603.4: Named-pair MUST precede the generic compound
        // control combinator so " and " between named cards binds to the
        // names list, not interpreted as a second `you control` clause.
        parse_control_named_pair,
        parse_compound_control_presence,
        parse_filter_have_total_property,
        parse_control_conditions,
        parse_remaining_state_presence_conditions,
    ))
    .parse(input)
}

fn parse_remaining_state_presence_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        parse_opponent_poison_conditions,
        parse_defending_player_more_life_than_another_opponent,
        parse_defending_player_comparison_conditions,
        parse_that_player_controls_more_comparison,
        parse_no_opponent_comparison_conditions,
        parse_triggering_player_has_unattacked_opponent,
        parse_opponent_comparison_conditions,
        parse_life_conditions,
        parse_quantity_quantity_comparison,
        parse_zone_conditions,
        parse_there_are_counters_on_source,
        parse_card_exiled_with_source_condition,
        parse_there_are_conditions,
        parse_there_exists_compound_zone_condition,
        parse_there_exists_condition,
        parse_subject_first_zone_count,
    ))
    .parse(input)
}

fn parse_event_history_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        parse_damage_dealt_this_turn_conditions,
        parse_source_damage_threshold_this_turn,
        parse_source_didnt_this_turn,
        parse_was_cast_condition,
        parse_entered_this_turn,
        parse_opponent_cast_spell_this_turn,
        parse_youve_this_turn,
        parse_first_spell_this_game_condition,
        parse_event_state_conditions,
    ))
    .parse(input)
}

/// CR 601.2 + CR 611.3a: "as long as it was cast" — cast-origin gate for
/// continuous statics (The Tarrasque). Zone-specific "was cast from <zone>"
/// must be tried before the zoneless form.
fn parse_was_cast_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        map(
            alt((
                tag::<_, _, OracleError<'_>>("it wasn't cast"),
                tag("it wasn\u{2019}t cast"),
            )),
            |_| StaticCondition::Not {
                condition: Box::new(StaticCondition::WasCast { zone: None }),
            },
        ),
        map(
            (
                alt((
                    tag::<_, _, OracleError<'_>>("it was cast from "),
                    tag("~ was cast from "),
                    tag("this creature was cast from "),
                    tag("this permanent was cast from "),
                )),
                parse_zone_word,
            ),
            |(_, zone)| StaticCondition::WasCast { zone: Some(zone) },
        ),
        value(
            StaticCondition::WasCast { zone: None },
            alt((
                tag::<_, _, OracleError<'_>>("it was cast"),
                tag("~ was cast"),
                tag("this creature was cast"),
                tag("this permanent was cast"),
            )),
        ),
    ))
    .parse(input)
}

fn parse_damage_dealt_this_turn_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        parse_player_was_dealt_damage_threshold_this_turn,
        parse_player_dealt_combat_damage_by_source_this_turn,
        parse_source_dealt_damage_to_opponent_this_turn,
        parse_source_was_dealt_damage_this_turn,
    ))
    .parse(input)
}

/// CR 603.4 + CR 120.3: "you were/an opponent was dealt N or more damage this
/// turn" — Boarded Window and Phoenix Chick-style end-step intervening-if
/// predicates. Any-source damage to the matching player set.
fn parse_player_was_dealt_damage_threshold_this_turn(
    input: &str,
) -> OracleResult<'_, StaticCondition> {
    let (rest, (target, passive_verb)) = alt((
        value(
            (
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
                " were dealt ",
            ),
            tag("you"),
        ),
        value((TargetFilter::Player, " was dealt "), tag("a player")),
        value(
            (
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                " was dealt ",
            ),
            tag("an opponent"),
        ),
    ))
    .parse(input)?;
    let (rest, _) = tag(passive_verb).parse(rest)?;
    let (rest, amount) = parse_number(rest)?;
    let (rest, _) = tag(" or more damage this turn").parse(rest)?;
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::Any),
                target: Box::new(target),
                aggregate: AggregateFunction::Sum,
                group_by: None,
                damage_kind: DamageKindFilter::Any,
            },
            amount,
        ),
    ))
}

/// CR 603.4 + CR 120.2a + CR 608.2i: Parse the intervening-`if` predicate
/// "a player/an opponent was dealt combat damage by a <source> this turn".
///
/// Lost Monarch of Ifnir: "At the beginning of your second main phase, if a
/// player was dealt combat damage by a Zombie this turn, …". The predicate is
/// satisfied when at least one recipient matching the subject was dealt combat
/// damage this turn by a source matching the `by` filter.
///
/// Builds for the class — the source is parsed by `parse_type_phrase`, so any
/// "by a <creature type / creature / permanent>" qualifier is covered, and the
/// subject covers both "a player" and "an opponent" recipients. The resulting
/// `QuantityRef::DamageDealtThisTurn` carries `damage_kind: CombatOnly`
/// (CR 120.2a) and resolves against `state.damage_dealt_this_turn`, matching
/// the source's CR 608.2i look-back type snapshot recorded at damage time, so a
/// later type change or the source leaving the battlefield still answers the
/// rules-correct question.
fn parse_player_dealt_combat_damage_by_source_this_turn(
    input: &str,
) -> OracleResult<'_, StaticCondition> {
    // Recipient subject: "a player" (any player) or "an opponent".
    let (rest, recipient) = alt((
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            tag("an opponent"),
        ),
        value(TargetFilter::Player, tag("a player")),
    ))
    .parse(input)?;
    let (rest, _) = tag(" was dealt combat damage by ").parse(rest)?;
    // CR 608.2i: the `by` source qualifier — "a Zombie", "a creature", etc.
    let (source, after_source) = parse_type_phrase(rest);
    if matches!(source, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let (rest, _) = tag(" this turn").parse(after_source)?;
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::DamageDealtThisTurn {
                source: Box::new(source),
                target: Box::new(recipient),
                aggregate: AggregateFunction::Sum,
                group_by: None,
                damage_kind: DamageKindFilter::CombatOnly,
            },
            1,
        ),
    ))
}

fn parse_source_dealt_damage_to_opponent_this_turn(
    input: &str,
) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("~"), tag("this creature"))).parse(input)?;
    let (rest, _) = tag(" dealt damage to ").parse(rest)?;
    let (rest, target) = alt((
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            alt((tag("an opponent"), tag("opponent"))),
        ),
        value(TargetFilter::Player, alt((tag("a player"), tag("player")))),
    ))
    .parse(rest)?;
    let (rest, _) = tag(" this turn").parse(rest)?;
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::SelfRef),
                target: Box::new(target),
                aggregate: AggregateFunction::Sum,
                group_by: None,
                damage_kind: DamageKindFilter::Any,
            },
            1,
        ),
    ))
}

fn parse_source_was_dealt_damage_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    // Subject determines the damage target: the source itself, or an opponent.
    let (rest, target) = alt((
        value(
            TargetFilter::SelfRef,
            alt((tag("~"), tag("this creature"), tag("this permanent"))),
        ),
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            tag("an opponent"),
        ),
    ))
    .parse(input)?;
    // Accept both passive-voice tense forms: "was dealt" and "has been dealt".
    let (rest, _) = alt((
        tag(" was dealt damage this turn"),
        tag(" has been dealt damage this turn"),
    ))
    .parse(rest)?;
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::Any),
                target: Box::new(target),
                aggregate: AggregateFunction::Sum,
                group_by: None,
                damage_kind: DamageKindFilter::Any,
            },
            1,
        ),
    ))
}

fn parse_resolution_context_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        parse_source_qualified_mana_spent_condition,
        parse_source_qualified_mana_spent_threshold,
        parse_mana_spent_vs_source_pt,
        parse_mana_spent_threshold,
        parse_combat_context_conditions,
        parse_put_onto_battlefield_this_way,
        parse_unless_pay_condition,
    ))
    .parse(input)
}

/// CR 608.2c: "you put fewer than/more than <N> <noun> onto the battlefield
/// this way" — a resolution-context comparison gating a follow-up effect on
/// how many objects the immediately preceding effect placed onto the
/// battlefield (Expand the Sphere's "If you put fewer than two lands onto the
/// battlefield this way, …").
///
/// The noun is parsed only to consume text — `QuantityRef::TrackedSetSize` is
/// a unit reference to the count of objects moved by the preceding sub_ability
/// effect, with no per-noun filter, so the noun threads nowhere.
fn parse_put_onto_battlefield_this_way(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you put ").parse(input)?;
    let (rest, comparator) = alt((
        value(Comparator::LT, tag("fewer than ")),
        value(Comparator::GT, tag("more than ")),
    ))
    .parse(rest)?;
    let (rest, n) = parse_number(rest)?;
    let (rest, _) = tag(" ").parse(rest)?;
    // CR 608.2c: "this way" scopes to objects moved by this resolution.
    let (rest, _) = take_until(" onto the battlefield this way").parse(rest)?;
    let (rest, _) = tag(" onto the battlefield this way").parse(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::TrackedSetSize,
            },
            comparator,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
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

/// CR 201.2 + CR 603.4: Parse "you control [type] named [Name1] and [Name2]"
/// as a conjunction of two single-named presence checks. Each named card is its
/// own control predicate; the AND in the source phrase joins the two names, not
/// the type word.
///
/// Empires cycle canonical: Scepter of Empires' "if you control artifacts named
/// Crown of Empires and Throne of Empires" — semantically requires you control
/// one artifact named "Crown of Empires" AND one artifact named "Throne of
/// Empires". Distinct from `parse_compound_control_presence` (which requires
/// "you control" twice and joins distinct typed filters); here the bare type
/// word is shared across both names.
///
/// Must precede `parse_compound_control_presence` so the trailing " and "
/// is bound to the names list, not interpreted as a second `you control` clause.
fn parse_control_named_pair(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you control ").parse(input)?;
    // Split on " named " — the type-phrase head precedes it, the names list follows.
    let (after_named, type_text) = take_until(" named ").parse(rest)?;
    let (after_named, _) = tag(" named ").parse(after_named)?;
    let (filter_base, type_remainder) = parse_type_phrase(type_text);
    if matches!(filter_base, TargetFilter::Any) || !type_remainder.trim().is_empty() {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    // Strip any FilterProp::Named that parse_type_phrase may have attached so the
    // synthesized per-name conjuncts carry exactly one Named property each.
    let filter_base = strip_filter_named_property(filter_base);
    // First name extends to " and "; second name extends to end-of-clause
    // (period or end of input). Both use take_until-style scanning to avoid
    // string-method dispatch.
    let (after_first_name, first_name) = take_until(" and ").parse(after_named)?;
    let (after_first_name, _) = tag(" and ").parse(after_first_name)?;
    // Second name: stop at period or end. parse_until_clause_end consumes the
    // remainder up to a sentence boundary so trailing punctuation does not bleed
    // into the captured name.
    let (rest_after_pair, second_name) = parse_until_clause_end(after_first_name)?;
    let first_name = first_name.trim();
    let second_name = second_name.trim();
    if first_name.is_empty() || second_name.is_empty() {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let first_filter = with_named_property(filter_base.clone(), first_name);
    let second_filter = with_named_property(filter_base, second_name);
    let first = StaticCondition::IsPresent {
        filter: Some(inject_controller_you(first_filter)),
    };
    let second = StaticCondition::IsPresent {
        filter: Some(inject_controller_you(second_filter)),
    };
    Ok((
        rest_after_pair,
        StaticCondition::And {
            conditions: vec![first, second],
        },
    ))
}

/// Consume bytes up to a clause boundary (period, comma, or end of input).
/// Returns the captured slice and the remainder positioned at the boundary.
fn parse_until_clause_end(input: &str) -> OracleResult<'_, &str> {
    use nom::bytes::complete::take_till;
    take_till(|c| c == '.' || c == ',').parse(input)
}

/// Append a `FilterProp::Named { name }` to a typed filter. Used by
/// `parse_control_named_pair` to materialize per-name conjuncts.
fn with_named_property(filter: TargetFilter, name: &str) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut tf) => {
            tf.properties.push(FilterProp::Named {
                name: name.to_string(),
            });
            TargetFilter::Typed(tf)
        }
        other => other,
    }
}

/// Remove any `FilterProp::Named` from a typed filter. Used to clean up the
/// shared base filter before the per-name conjuncts attach their own name
/// property in `parse_control_named_pair`.
fn strip_filter_named_property(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut tf) => {
            tf.properties
                .retain(|prop| !matches!(prop, FilterProp::Named { .. }));
            TargetFilter::Typed(tf)
        }
        other => other,
    }
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
/// Handles "you're the monarch", "you have the initiative", and "you have the city's blessing".
fn parse_player_state_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        // CR 725.1: Monarch status
        value(
            StaticCondition::IsMonarch,
            alt((tag("you're the monarch"), tag("you are the monarch"))),
        ),
        // CR 726.3: Initiative status
        value(
            StaticCondition::IsInitiative,
            tag("you have the initiative"),
        ),
        // CR 725.1: "there is no monarch" — no player holds the designation.
        value(
            StaticCondition::NoMonarch,
            alt((tag("there is no monarch"), tag("there's no monarch"))),
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
        // CR 103.1: Starting-player status. "you weren't the starting player"
        // (Radiant Smite, Cindercone Smite, Sylvan Smite) is the dominant
        // idiom; the affirmative form composes the same variant. Negation is
        // tried first so the longer "weren't" tag wins over "were".
        map(
            alt((
                tag("you weren't the starting player"),
                tag("you were not the starting player"),
            )),
            |_| StaticCondition::Not {
                condition: Box::new(StaticCondition::WasStartingPlayer {
                    controller: ControllerRef::You,
                }),
            },
        ),
        value(
            StaticCondition::WasStartingPlayer {
                controller: ControllerRef::You,
            },
            tag("you were the starting player"),
        ),
        // CR 903.3 + CR 109.5: "your commander" — owner-scoped (Lieutenant).
        value(
            StaticCondition::ControlsCommander {
                ownership: CommanderOwnership::Own,
            },
            tag("you control your commander"),
        ),
        // CR 903.3d: "a commander" — controller-only, any owner.
        value(
            StaticCondition::ControlsCommander {
                ownership: CommanderOwnership::Any,
            },
            tag("you control a commander"),
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
    parse_opponent_poison_at_least(input)
}

fn parse_opponent_poison_at_least(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("an opponent has ").parse(input)?;
    let (rest, count) = parse_number(rest)?;
    let (rest, _) = tag(" or more poison counters").parse(rest)?;
    Ok((rest, StaticCondition::OpponentPoisonAtLeast { count }))
}

#[derive(Clone)]
struct AttachedConditionSubject {
    type_filter: TypeFilter,
    attachment_prop: FilterProp,
}

fn parse_attached_condition_subject_core(
    input: &str,
) -> OracleResult<'_, AttachedConditionSubject> {
    alt((
        value(
            AttachedConditionSubject {
                type_filter: TypeFilter::Permanent,
                attachment_prop: FilterProp::EnchantedBy,
            },
            tag("enchanted permanent"),
        ),
        value(
            AttachedConditionSubject {
                type_filter: TypeFilter::Creature,
                attachment_prop: FilterProp::EnchantedBy,
            },
            tag("enchanted creature"),
        ),
        value(
            AttachedConditionSubject {
                type_filter: TypeFilter::Artifact,
                attachment_prop: FilterProp::EnchantedBy,
            },
            tag("enchanted artifact"),
        ),
        value(
            AttachedConditionSubject {
                type_filter: TypeFilter::Land,
                attachment_prop: FilterProp::EnchantedBy,
            },
            tag("enchanted land"),
        ),
        value(
            AttachedConditionSubject {
                type_filter: TypeFilter::Creature,
                attachment_prop: FilterProp::EquippedBy,
            },
            tag("equipped creature"),
        ),
    ))
    .parse(input)
}

fn parse_attached_condition_subject(input: &str) -> OracleResult<'_, AttachedConditionSubject> {
    terminated(parse_attached_condition_subject_core, multispace1).parse(input)
}

fn attached_subject_typed_filter(subject: &AttachedConditionSubject) -> TypedFilter {
    TypedFilter::new(subject.type_filter.clone()).properties(vec![subject.attachment_prop.clone()])
}

pub(crate) fn parse_attached_subject_target_filter(input: &str) -> OracleResult<'_, TargetFilter> {
    let (rest, subject) = parse_attached_condition_subject_core(input)?;
    Ok((
        rest,
        TargetFilter::Typed(attached_subject_typed_filter(&subject)),
    ))
}

/// CR 508.1a + CR 509.1a + CR 611.3a: Parse "enchanted/equipped creature is
/// attacking|blocking" into the attached-subject's `TargetFilter` plus the
/// combat-state `FilterProp`. Unlike `parse_attached_subject_is_filter` (which
/// folds a STATIC characteristic — color/type/supertype — into the subject
/// filter), combat state is re-evaluated each layer cycle (CR 611.3a), so the
/// caller must bind it as a `RecipientMatchesFilter` GATE on the recipient (the
/// attached creature), NOT fold it into the affected filter.
///
/// "blocked" is intentionally NOT a branch: `FilterProp` has no recipient-side
/// "blocked" prop (only `Attacking`, `Blocking`, `BlockingSource`,
/// `CombatRelation`, `Unblocked`), and there are no in-class cards. Inventing a
/// `Blocked` prop is a new-variant decision routed through /add-engine-variant.
pub(crate) fn parse_attached_subject_combat_state(
    input: &str,
) -> OracleResult<'_, (TargetFilter, FilterProp)> {
    let (rest, subject) = parse_attached_condition_subject(input)?;
    let (rest, _) = tag("is ").parse(rest)?;
    let (rest, prop) = alt((
        value(FilterProp::Attacking { defender: None }, tag("attacking")),
        value(FilterProp::Blocking, tag("blocking")),
    ))
    .parse(rest)?;
    let filter = TargetFilter::Typed(attached_subject_typed_filter(&subject));
    Ok((rest, (filter, prop)))
}

/// Parse a positive attached-subject characteristic predicate
/// ("enchanted creature is white", "equipped creature is an artifact",
/// "enchanted creature is legendary") into the merged attached-subject
/// `TargetFilter` (e.g. `creature + EnchantedBy + HasColor{White}`).
///
/// This is the positive, filter-only counterpart of
/// `parse_attached_object_is_filter_condition`, which wraps the same merged
/// filter in an `IsPresent`/`Not` `StaticCondition`. The inverted
/// attached-subject grant path ("As long as enchanted creature is X, it gets
/// …") uses it to bind the grant's `affected` filter to the enchanted/equipped
/// permanent, so the buff lands on the host for the whole characteristic class
/// (color, type, subtype, supertype) — not just `legendary`.
pub(crate) fn parse_attached_subject_is_filter(input: &str) -> OracleResult<'_, TargetFilter> {
    let (rest, subject) = parse_attached_condition_subject(input)?;
    let (rest, _) = tag("is ").parse(rest)?;
    parse_attached_predicate_filter(rest, &subject)
}

fn merge_attached_predicate_filter(
    subject: &AttachedConditionSubject,
    predicate: TargetFilter,
) -> Option<TargetFilter> {
    let TargetFilter::Typed(predicate) = predicate else {
        return None;
    };

    let mut filter = attached_subject_typed_filter(subject);
    for type_filter in predicate.type_filters {
        if !filter.type_filters.contains(&type_filter) {
            filter.type_filters.push(type_filter);
        }
    }
    filter.controller = predicate.controller;
    for property in predicate.properties {
        if !filter.properties.contains(&property) {
            filter.properties.push(property);
        }
    }
    Some(TargetFilter::Typed(filter))
}

/// Parse a bare predicate tail — the type/subtype/color/supertype that follows a
/// subject's copula ("is"/"'s") — into a `TargetFilter::Typed` carrying ONLY the
/// predicate's own props (no attachment prop, no subject type). Shared by the
/// literal-subject attached path (`parse_attached_predicate_single`, which merges
/// the result into the subject filter) and the anaphoric "it" recipient path
/// (`parse_recipient_is_filter_condition`, which uses the bare filter directly as
/// the recipient match). Uses the same color / legendary-basic supertype /
/// `parse_type_phrase` recognition the attached path historically used, so the
/// downstream merged output is preserved byte-for-byte.
fn parse_bare_predicate_tail(input: &str) -> OracleResult<'_, TargetFilter> {
    let (rest, _) = opt(parse_article).parse(input)?;
    if let Ok((rest, color)) = parse_color(rest) {
        return Ok((
            rest,
            TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::HasColor { color }]),
            ),
        ));
    }

    if let Ok((rest, property)) = alt((
        value(
            FilterProp::HasSupertype {
                value: crate::types::card_type::Supertype::Legendary,
            },
            tag::<_, _, OracleError<'_>>("legendary"),
        ),
        value(
            FilterProp::HasSupertype {
                value: crate::types::card_type::Supertype::Basic,
            },
            tag::<_, _, OracleError<'_>>("basic"),
        ),
    ))
    .parse(rest)
    {
        if rest.is_empty() {
            return Ok((
                rest,
                TargetFilter::Typed(TypedFilter::default().properties(vec![property])),
            ));
        }
    }

    let (filter, remainder) = parse_type_phrase(rest);
    if remainder.len() == rest.len() {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((remainder, filter))
}

fn parse_attached_predicate_single<'a>(
    input: &'a str,
    subject: &AttachedConditionSubject,
) -> OracleResult<'a, TargetFilter> {
    let (remainder, bare) = parse_bare_predicate_tail(input)?;
    let Some(filter) = merge_attached_predicate_filter(subject, bare) else {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    };
    Ok((remainder, filter))
}

fn parse_attached_predicate_filter<'a>(
    input: &'a str,
    subject: &AttachedConditionSubject,
) -> OracleResult<'a, TargetFilter> {
    if let Ok((rest, left_text)) = take_until::<_, _, OracleError<'_>>(" or ").parse(input) {
        let (right_text, _) = tag::<_, _, OracleError<'_>>(" or ").parse(rest)?;
        let (left_rest, first) = parse_attached_predicate_single(left_text, subject)?;
        if left_rest.is_empty() {
            let (rest, second) = parse_attached_predicate_single(right_text, subject)?;
            return Ok((
                rest,
                TargetFilter::Or {
                    filters: vec![first, second],
                },
            ));
        }
    }

    let (rest, first) = parse_attached_predicate_single(input, subject)?;
    Ok((rest, first))
}

fn attached_filter_condition(filter: TargetFilter) -> StaticCondition {
    match filter {
        TargetFilter::Or { filters } => StaticCondition::Or {
            conditions: filters
                .into_iter()
                .map(|filter| StaticCondition::IsPresent {
                    filter: Some(filter),
                })
                .collect(),
        },
        filter => StaticCondition::IsPresent {
            filter: Some(filter),
        },
    }
}

/// Parse a predicate tail with optional N-way `" or "` disjunction into one or
/// more bare predicate filters. Shared shape between the attached and recipient
/// paths; the recipient path maps each bare filter to a `RecipientMatchesFilter`.
/// `separated_list1` folds arbitrary arity ("a Zombie or a Skeleton or a Spirit")
/// because `parse_bare_predicate_tail` stops at the first non-type token, leaving
/// the `" or "` separator for the next iteration.
fn parse_bare_predicate_disjunction(input: &str) -> OracleResult<'_, Vec<TargetFilter>> {
    nom::multi::separated_list1(tag(" or "), parse_bare_predicate_tail).parse(input)
}

/// CR 611.3a: "it's a Zombie" / "it isn't white" / "it's a Zombie or a Skeleton" —
/// the anaphoric "it" binds to the recipient (effective subject) of the continuous
/// effect. Emits `RecipientMatchesFilter` (affirmative), `Not(RecipientMatchesFilter)`
/// (negated), or `Or([RecipientMatchesFilter, …])` (disjunction). The pronoun subject
/// is scoped to this combinator only (it is NOT added to the shared source-subject
/// dispatcher, mirroring `parse_counter_condition_subject`). A terminal-boundary guard
/// rejects non-clause-ending predicates (e.g. "attacking alone") so the alt backtracks
/// to the combat combinator.
fn parse_recipient_is_filter_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("it").parse(input)?;
    // Negated copulae (" isn't ", " is not ") MUST be tried before the affirmative
    // " is " so " is not " is not greedily split into " is " + "not …".
    let (rest, negated) = alt((
        value(true, alt((tag(" isn't "), tag(" is not ")))),
        value(false, alt((tag("'s "), tag(" is ")))),
    ))
    .parse(rest)?;
    let (rest, filters) = parse_bare_predicate_disjunction(rest)?;

    // Pronoun-form boundary guard: the predicate must end at a clause boundary
    // (end of input or one of ",", ".", ";"). Otherwise leftover words (e.g.
    // "alone" from "it's attacking alone") mean a longer combinator owns the
    // phrase — backtrack via nom Err so the alt falls through to it.
    if !(rest.is_empty()
        || alt((tag::<_, _, OracleError<'_>>(","), tag(";"), tag(".")))
            .parse(rest)
            .is_ok())
    {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }

    let to_condition = |filter: TargetFilter| StaticCondition::RecipientMatchesFilter { filter };
    let condition = if filters.len() > 1 {
        StaticCondition::Or {
            conditions: filters.into_iter().map(to_condition).collect(),
        }
    } else {
        to_condition(filters.into_iter().next().expect("non-empty"))
    };
    let condition = if negated {
        StaticCondition::Not {
            condition: Box::new(condition),
        }
    } else {
        condition
    };
    Ok((rest, condition))
}

fn parse_attached_object_is_filter_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, subject) = parse_attached_condition_subject(input)?;
    let (rest, negated) = alt((
        value(true, alt((tag("isn't "), tag("is not ")))),
        value(false, tag("is ")),
    ))
    .parse(rest)?;
    let (rest, filter) = parse_attached_predicate_filter(rest, &subject)?;
    let condition = attached_filter_condition(filter);
    let condition = if negated {
        StaticCondition::And {
            conditions: vec![
                StaticCondition::IsPresent {
                    filter: Some(TargetFilter::Typed(attached_subject_typed_filter(&subject))),
                },
                StaticCondition::Not {
                    condition: Box::new(condition),
                },
            ],
        }
    } else {
        condition
    };
    Ok((rest, condition))
}

/// Shared subject dispatcher for source-referential predicates.
///
/// Consumes `"<subject> "` — the trailing `"is"` / `"isn't"` is dispatched by the
/// caller so negation (`"~ isn't attacking"`) composes cleanly.
///
/// Subjects: "~", "this creature", "this permanent", "this land", "this artifact",
/// "this enchantment", "equipped creature", "enchanted creature".
///
/// DEFER: the "equipped creature " / "enchanted creature " prefixes collapse to
/// `Source*` checks for the HOST creature across the tapped/monstrous/saddled/
/// equipped/attached-to-creature predicates that share this dispatcher too. For
/// those the host creature (not the Equipment/Aura) is the real subject, so
/// emitting a `Source*` condition is a suspected latent bug needing a dedicated
/// audit + recipient-gating pass (CR 611.3a). Only the combat-state predicate is
/// narrowed here — it uses `parse_self_source_subject` (below), which excludes
/// the attached prefixes, because an Equipment/Aura is never an attacker.
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

/// CR 611.3a: Like `parse_source_subject` but WITHOUT the attached-subject
/// prefixes ("equipped creature " / "enchanted creature "). The combat-state
/// predicate references the static's SOURCE; for an Equipment/Aura the source is
/// the attachment, which is never itself an attacker/blocker. Folding
/// "equipped creature is attacking" into `SourceIsAttacking` would gate on the
/// Equipment's (impossible) combat state instead of the host creature's. The
/// attached-subject combat form is owned by the inverted-grant path, which binds
/// it as a `RecipientMatchesFilter` gate on the host (see
/// `parse_attached_subject_combat_state`).
fn parse_self_source_subject(input: &str) -> OracleResult<'_, &str> {
    alt((
        tag("~ "),
        tag("this creature "),
        tag("this permanent "),
        tag("this land "),
        tag("this artifact "),
        tag("this enchantment "),
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
    // CR 611.3a: combat state references the SOURCE permanent. Exclude the
    // attached-subject prefixes ("equipped/enchanted creature") so they are NOT
    // collapsed into a `Source*` combat condition (an Equipment/Aura is never an
    // attacker); the attached-subject combat form is owned by the inverted-grant
    // path via `parse_attached_subject_combat_state`.
    let (rest, _) = parse_self_source_subject(input)?;
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

/// CR 702.171b: Parse "<subject> is saddled" → SourceIsSaddled.
/// Affirmative only — Saddle has no negated Oracle idiom; "as long as ~ is not
/// saddled" would compose `Not { SourceIsSaddled }` but no current card prints it.
fn parse_source_is_saddled(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_source_subject(input)?;
    value(StaticCondition::SourceIsSaddled, tag("is saddled")).parse(rest)
}

/// CR 301.5 + CR 303.4: Parse "<subject> is attached to a creature [you control]"
/// → SourceAttachedToCreature.
///
/// The optional " you control" suffix covers bestow-trigger gates like Springheart
/// Nantuko ("if this permanent is attached to a creature you control"). All printed
/// Oracle uses controller=You for this gate (the host of an Aura/bestow card is
/// always under its controller by CR 303.4d/CR 702.103b), so the controller axis
/// is parameter-free at the AST layer — the runtime evaluator checks the host's
/// controller against the ability's controller.
fn parse_source_attached_to_creature(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_source_subject(input)?;
    let (rest, _) = tag("is attached to a creature").parse(rest)?;
    // Optional trailing " you control" — consumed but not represented in the AST.
    let (rest, _) = opt(tag(" you control")).parse(rest)?;
    Ok((rest, StaticCondition::SourceAttachedToCreature))
}

/// CR 120.3 + CR 702.11b: Parse "<subject> hasn't dealt damage yet" into
/// `Not(SourceHasDealtDamage)` — the negated form of the sticky "has dealt
/// damage since entering" gate (Palladia-Mors, the Ruiner; Karakyk Guardian:
/// "has hexproof if it hasn't dealt damage yet"). Accepts the self-referential
/// subjects the conditional-keyword templates use ("it", "~", "this creature",
/// "this permanent"). Only the negated "hasn't" idiom appears in Oracle text, so
/// no affirmative form is produced here.
fn parse_source_hasnt_dealt_damage(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((
        tag("it "),
        tag("~ "),
        tag("this creature "),
        tag("this permanent "),
    ))
    .parse(input)?;
    value(
        StaticCondition::Not {
            condition: Box::new(StaticCondition::SourceHasDealtDamage),
        },
        tag("hasn't dealt damage yet"),
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
        // CR 702.171b: "~ is saddled" / "this creature is saddled" / etc.
        parse_source_is_saddled,
        // CR 301.5 + CR 303.4: "~ is attached to a creature" / "this equipment is attached to a creature".
        // Must precede `parse_source_is_type` so the specific "is attached to a creature"
        // predicate wins over generic "is <type>" dispatch.
        parse_source_attached_to_creature,
        // CR 303.4 + CR 604.1 + CR 613.1g: "~ is enchanted by exactly N
        // Aura(s)" / "N or more Auras" (Timber Paladin tiered static P/T gates).
        parse_source_enchanted_by_aura_count,
        // CR 122.1: "<subject> has <quantity> <counter_type> counter(s) on it"
        // — covers Unleash/Outlast/Renown bodies, Primordial Hydra's trample gate,
        // and every "as long as it has …" counter-comparator static.
        // Must precede `parse_source_is_type` so "has … counters on it" wins over
        // any other interpretation.
        parse_source_has_counters,
        // CR 120.3 + CR 702.11b: "<subject> hasn't dealt damage yet" →
        // Not(SourceHasDealtDamage). Specific full-phrase tag; placed before the
        // generic predicates so it is not shadowed.
        parse_source_hasnt_dealt_damage,
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

/// CR 303.4: Parse "<subject> is enchanted by exactly N Aura(s)" or
/// "N or more Auras" into an `ObjectCount` + `AttachedToSource` gate.
fn parse_source_enchanted_by_aura_count(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_source_subject(input)?;
    let (rest, _) = tag("is enchanted by ").parse(rest)?;
    let (rest, (comparator, n)) = alt((
        map((tag("exactly "), parse_number), |(_, n)| {
            (Comparator::EQ, n)
        }),
        map(parse_ge_threshold, |n| (Comparator::GE, n)),
    ))
    .parse(rest)?;
    let (rest, _) = alt((tag("Auras"), tag("Aura"))).parse(rest.trim_start())?;
    let aura_filter = TargetFilter::Typed(TypedFilter {
        type_filters: vec![
            TypeFilter::Enchantment,
            TypeFilter::Subtype("Aura".to_string()),
        ],
        controller: None,
        properties: vec![FilterProp::AttachedToSource],
    });
    Ok((
        rest,
        make_quantity_comparison(
            QuantityRef::ObjectCount {
                filter: aura_filter,
            },
            comparator,
            n,
        ),
    ))
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
    // The shared condition path (intervening-"if" triggers and static gates that
    // delegate to `parse_inner_condition`) reads the subject as
    // source-referential: "whenever ~ attacks, if it has three +1/+1 counters on
    // it" (Ayara's Oathsworn) means the triggering source itself. The
    // recipient-bound "for as long as it has a counter" reading is the duration
    // grammar's job — see `parse_recipient_has_counters`.
    let (rest, (_subject, counters, minimum, maximum)) = parse_has_counters_axes(input)?;
    Ok((
        rest,
        StaticCondition::HasCounters {
            counters,
            minimum,
            maximum,
        },
    ))
}

/// Recipient-bound counterpart to [`parse_source_has_counters`] for
/// `Duration::ForAsLongAs` clauses. CR 122.1 + CR 611.2b: in "for as long as it
/// has a shield counter" (Shield Broker) the bound pronoun "it" is the object
/// the effect applies to (the *recipient* — the controlled creature), not the
/// source. The recipient variant is evaluated against the affected object by the
/// layer system (`evaluate_condition_with_recipient`); a source subject ("~" /
/// "this creature", Demon Wall) still yields `HasCounters`.
pub(crate) fn parse_recipient_has_counters(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, (subject, counters, minimum, maximum)) = parse_has_counters_axes(input)?;
    let condition = match subject {
        CounterConditionSubject::Recipient => StaticCondition::RecipientHasCounters {
            counters,
            minimum,
            maximum,
        },
        CounterConditionSubject::Source => StaticCondition::HasCounters {
            counters,
            minimum,
            maximum,
        },
    };
    Ok((rest, condition))
}

/// Shared grammar axes for the counter-has condition family: subject × quantity
/// × counter-type noun × `"counter[s]"` × `"on it"`. Each axis is a single
/// `alt()` so new variants add one arm rather than enumerating permutations.
fn parse_has_counters_axes(
    input: &str,
) -> OracleResult<'_, (CounterConditionSubject, CounterMatch, u32, Option<u32>)> {
    let (rest, subject) = parse_counter_condition_subject(input)?;
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

    Ok((rest, (subject, counters, minimum, maximum)))
}

/// Subject axis for counter-has conditions. Accepts the canonical
/// source-referential subjects and the bound pronoun `"it "` used in
/// `"for as long as it has a counter on it"` style clauses. Kept separate
/// from `parse_source_subject` because `"it "` would be ambiguous in the
/// tapped/combat predicate family (which already uses `"it"` as part of
/// longer phrases) — scoping the pronoun branch to this combinator avoids
/// that coupling.
#[derive(Clone, Copy)]
enum CounterConditionSubject {
    Source,
    Recipient,
}

fn parse_counter_condition_subject(input: &str) -> OracleResult<'_, CounterConditionSubject> {
    alt((
        // "~" / "this creature" — the source permanent carrying the static
        // (Demon Wall).
        value(CounterConditionSubject::Source, parse_source_subject),
        // The bound pronoun "it" — the recipient/affected object, e.g. the
        // creature controlled "for as long as it has a counter".
        value(CounterConditionSubject::Recipient, tag("it ")),
    ))
    .parse(input)
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
    if type_slice
        .chars()
        .any(|c| matches!(c, ',' | '.' | ';' | ':'))
    {
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
/// Two grammar forms compose through a shared comparator suffix:
/// - Possessive subject + linking verb: `its power is N`,
///   `enchanted creature's toughness is N`, `equipped creature's power is N`.
/// - Source subject + `has`: `~ has power N`, `this creature has toughness N`,
///   etc. — every subject accepted by `parse_source_subject`.
///
/// The `~ has power N` form is the canonical templating used by intervening-if
/// continuations such as "Then if ~ has power 7 or greater, …" (Cloud,
/// Ex-SOLDIER). Without it, those clauses silently swallow the condition and
/// the gated sub-ability fires unconditionally.
fn parse_source_power_toughness_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, qty) = alt((parse_possessive_property, parse_subject_has_property)).parse(input)?;
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

/// Possessive-subject form: `<possessive> <property> is `, leaving the threshold
/// number on the remaining input.
fn parse_possessive_property(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((
        tag("its "),
        tag("enchanted creature's "),
        tag("equipped creature's "),
    ))
    .parse(input)?;
    alt((
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
    .parse(rest)
}

/// Source-subject form: `<subject> has <property> `, leaving the threshold
/// number on the remaining input. Reuses `parse_source_subject` so every
/// canonical source phrasing (`~`, `this creature`, `this permanent`, …,
/// `enchanted creature`, `equipped creature`) composes identically.
fn parse_subject_has_property(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((
        parse_source_subject,
        // CR 201.5: Pronouns in self-referential granted abilities refer to
        // the object that has the ability. Keep this scoped to the property
        // grammar so it does not steal recipient-bound "it has a counter"
        // duration clauses from `parse_recipient_has_counters`.
        tag("it "),
    ))
    .parse(input)?;
    let (rest, _) = tag("has ").parse(rest)?;
    alt((
        value(
            QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Source,
            },
            tag("power "),
        ),
        value(
            QuantityRef::Toughness {
                scope: crate::types::ability::ObjectScope::Source,
            },
            tag("toughness "),
        ),
    ))
    .parse(rest)
}

/// Parse hand-size predicates after a `<subject> has ` prefix has been
/// consumed. Returns `Some(condition)` on match.
///
/// Shared by "you have ..." and "that player has ..." dispatchers — the only
/// axis that varies is the `PlayerScope` of the resulting `HandSize` ref, so
/// the suffixes themselves compose cleanly with any subject. Also accepts
/// the canonical "their hand" form for plural-friendly readings.
fn consume_cards_in_hand_suffix(input: &str) -> Option<&str> {
    tag::<_, _, OracleError<'_>>(" cards in hand")
        .parse(input)
        .ok()
        .map(|(rest, _)| rest)
        .or_else(|| {
            tag::<_, _, OracleError<'_>>(" cards in your hand")
                .parse(input)
                .ok()
                .map(|(rest, _)| rest)
        })
}

fn parse_hand_size_predicate(rest: &str, player: PlayerScope) -> Option<(&str, StaticCondition)> {
    // "no cards in hand" → HandSize EQ 0
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("no cards in hand"),
        tag::<_, _, OracleError<'_>>("no cards in your hand"),
    ))
    .parse(rest)
    {
        return Some((
            rest,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize { player },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            },
        ));
    }

    // CR 402: "fewer than N cards in hand" → HandSize LT N
    // "fewer than" is a strict inequality (e.g. "fewer than seven" excludes seven),
    // used by Kozilek, the Great Distortion and Iymrith, Desert Doom.
    if let Ok((after_n, n)) =
        nom::sequence::preceded(tag::<_, _, OracleError<'_>>("fewer than "), parse_number)
            .parse(rest)
    {
        if let Some(rest) = consume_cards_in_hand_suffix(after_n) {
            return Some((
                rest,
                make_quantity_comparison(QuantityRef::HandSize { player }, Comparator::LT, n),
            ));
        }
    }

    // "exactly N cards in hand" → HandSize EQ N (Triskaidekaphile).
    if let Ok((after_exactly, _)) = tag::<_, _, OracleError<'_>>("exactly ").parse(rest) {
        if let Ok((after_n, n)) = parse_number(after_exactly) {
            if let Some(rest) = consume_cards_in_hand_suffix(after_n) {
                return Some((
                    rest,
                    make_quantity_comparison(QuantityRef::HandSize { player }, Comparator::EQ, n),
                ));
            }
        }
    }

    // CR 402: "more cards in hand than you" → HandSize(player) GT HandSize(Controller)
    // Used in cross-player comparisons like "that player has more cards in hand than you"
    // (Slithermuse, Sandstone Oracle, Balance of Power).
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("more cards in hand than you").parse(rest) {
        return Some((
            rest,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize { player },
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

    // "N or more cards in hand" → HandSize GE N
    let (after_n, n) = parse_number(rest).ok()?;
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>(" or more cards in hand"),
        tag::<_, _, OracleError<'_>>(" or more cards in your hand"),
    ))
    .parse(after_n)
    {
        return Some((rest, make_quantity_ge(QuantityRef::HandSize { player }, n)));
    }
    // "N or fewer cards in hand" → HandSize LE N
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>(" or fewer cards in hand"),
        tag::<_, _, OracleError<'_>>(" or fewer cards in your hand"),
    ))
    .parse(after_n)
    {
        return Some((
            rest,
            make_quantity_comparison(QuantityRef::HandSize { player }, Comparator::LE, n),
        ));
    }
    None
}

/// CR 208.1 + CR 603.4 + CR 109.3:
/// Parse superlative-comparison conditions of the form
/// "its <property> is <comparator> each other <type>'s <property>" and the
/// equivalent surface forms "it has the [greatest|lowest] <property> among
/// <filter>" / "...or is tied for [greatest|lowest] <property> among
/// <filter>". The subject anaphor ("its" / "it") binds to the triggering
/// object (CR 603.4 + CR 109.3), the right-hand side aggregates the same
/// property across every OTHER object of the filtered class via
/// `FilterProp::OtherThanTriggerObject` (CR 603.4 + CR 109.3 — see the
/// `OtherThanTriggerObject` doc on `FilterProp`). The comparator-aggregate
/// pairing (Max for "greater than"/"greatest"; Min for "less than"/"lowest")
/// is grammatical coupling, not a CR-defined rule. Used by Selvala, Heart of
/// the Wilds' ETB draw gate.
///
/// Outputs `StaticCondition::QuantityComparison` with:
/// - LHS `QuantityRef::Power|Toughness|ObjectManaValue { scope:
///   ObjectScope::EventSource }` — the triggering object's current property.
/// - RHS `QuantityRef::Aggregate { function: Max|Min, property, filter }`
///   where `filter` carries `FilterProp::OtherThanTriggerObject` to exclude
///   the triggering object from the aggregate population at runtime.
///
/// The combinator emits `OtherThanTriggerObject` directly (not `Another`)
/// because the pattern is semantically anchored to a trigger context: the
/// "each other" phrase only makes sense relative to a single anchored
/// subject (the triggering object). This sidesteps the static→ability
/// condition bridge, which passes filters through unchanged.
fn parse_subject_property_superlative_comparison(input: &str) -> OracleResult<'_, StaticCondition> {
    // Two surface forms are accepted:
    //   A. "its <prop> is <comparator phrase> each/every other <type>'s <prop>"
    //   B. "it has the [greatest|lowest] <prop> among <filter>"
    //      (with optional "or is tied for [greatest|lowest] <prop> among
    //      <filter>" extension that relaxes strict > to >=)
    //
    // Status: Form A is reached by the trigger intervening-if path
    // (`extract_if_condition` → `parse_inner_condition`) for Selvala-class
    // cards. Form B is wired into the same `parse_inner_condition` entry but
    // is not yet reached by real cards: Strength-Testing Hammer and Wretched
    // Banquet route through sub-clause/trailing-suffix paths that don't
    // currently delegate to this combinator. Form B is retained so that the
    // follow-up routing fix (extending `strip_property_conditional` to accept
    // aggregate RHS, or routing the "then if" sub-clause body through
    // `try_nom_condition_as_ability_condition`) lands a one-line change
    // rather than re-deriving the grammar.
    alt((
        parse_subject_property_inequality_form,
        parse_subject_has_superlative_form,
    ))
    .parse(input)
}

/// Surface form A: "its <prop> is <comparator phrase> each other <type>'s <prop>".
fn parse_subject_property_inequality_form(input: &str) -> OracleResult<'_, StaticCondition> {
    // Subject anaphor: "its " — binds to the triggering object.
    let (rest, _) = tag("its ").parse(input)?;
    // Property on the LHS.
    let (rest, lhs_property) = parse_property_keyword(rest)?;
    // Connective: " is ".
    let (rest, _) = tag(" is ").parse(rest)?;
    // Comparator phrase yields (Comparator, AggregateFunction) — the aggregate
    // function on the RHS is coupled to the comparator direction so the
    // semantics are existential: GT/GE pair with Max, LT/LE pair with Min.
    let (rest, (comparator, aggregate)) = parse_superlative_comparator_phrase(rest)?;
    // Aggregate scope: "each other <type>'s <prop>" / "every other <type>'s <prop>".
    let (rest, _) = alt((tag("each other "), tag("every other "))).parse(rest)?;
    // <type> phrase. parse_type_phrase consumes "creature", "creature you
    // control", etc. — without the "other" prefix (already stripped above so
    // we control the exclusion semantics through OtherThanTriggerObject).
    let (filter, remainder) = parse_type_phrase(rest);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let consumed = rest.len() - remainder.len();
    let rest = &rest[consumed..];
    // Possessive "'s " + property keyword (must match LHS property).
    let (rest, _) = tag("'s ").parse(rest)?;
    let (rest, rhs_property) = parse_property_keyword(rest)?;
    if lhs_property != rhs_property {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((
        rest,
        build_superlative_comparison(filter, lhs_property, comparator, aggregate),
    ))
}

/// Surface form B: "it has the greatest <prop> among <filter>" and the
/// "...or is tied for greatest <prop> among <filter>" relaxation. The
/// "among <filter>" clause is shared by both halves of the disjunction
/// (when present), so it appears at the end of the full phrase.
/// "lowest" / "least" map to Min; "greatest" / "highest" map to Max.
fn parse_subject_has_superlative_form(input: &str) -> OracleResult<'_, StaticCondition> {
    // Subject: "it has the " or "~ has the ".
    let (rest, _) = alt((tag("it has the "), tag("~ has the "))).parse(input)?;
    // Superlative adjective → AggregateFunction.
    let (rest, aggregate) = parse_superlative_adjective(rest)?;
    let (rest, _) = tag(" ").parse(rest)?;
    // Property.
    let (rest, property) = parse_property_keyword(rest)?;
    // Optional "or is tied for <same superlative> <same property>" tail
    // relaxes strict GT/LT to GE/LE. The tail does NOT carry its own
    // "among <filter>" — the filter clause is shared and comes after.
    let (rest, comparator) = parse_optional_tied_for_tail(rest, aggregate, property)?;
    // " among <filter>".
    let (rest, _) = tag(" among ").parse(rest)?;
    let (filter, remainder) = parse_type_phrase(rest);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let consumed = rest.len() - remainder.len();
    let rest = &rest[consumed..];
    Ok((
        rest,
        build_superlative_comparison(filter, property, comparator, aggregate),
    ))
}

/// Parse a superlative adjective into its corresponding `AggregateFunction`.
fn parse_superlative_adjective(input: &str) -> OracleResult<'_, AggregateFunction> {
    alt((
        value(AggregateFunction::Max, tag("greatest")),
        value(AggregateFunction::Max, tag("highest")),
        value(AggregateFunction::Min, tag("lowest")),
        value(AggregateFunction::Min, tag("least")),
    ))
    .parse(input)
}

/// Property keyword parser — used by both LHS and RHS of the comparison.
fn parse_property_keyword(input: &str) -> OracleResult<'_, ObjectProperty> {
    alt((
        value(ObjectProperty::Power, tag("power")),
        value(ObjectProperty::Toughness, tag("toughness")),
        value(ObjectProperty::ManaValue, tag("mana value")),
    ))
    .parse(input)
}

/// Parse the comparator phrase between "is " and "each other ...".
///
/// The aggregate function is coupled to the comparator direction by the
/// grammar (not a CR rule): GT/GE compare against the Max of the population
/// (∃ object with greater property than each ⟺ subject > Max of others);
/// LT/LE compare against Min.
pub(crate) fn parse_superlative_comparator_phrase(
    input: &str,
) -> OracleResult<'_, (Comparator, AggregateFunction)> {
    // Order matters: longer phrases ("greater than or equal to") must precede
    // their prefixes ("greater than") so the longer form wins.
    alt((
        value(
            (Comparator::GE, AggregateFunction::Max),
            tag("greater than or equal to "),
        ),
        value(
            (Comparator::LE, AggregateFunction::Min),
            tag("less than or equal to "),
        ),
        value(
            (Comparator::GT, AggregateFunction::Max),
            tag("greater than "),
        ),
        value((Comparator::LT, AggregateFunction::Min), tag("less than ")),
    ))
    .parse(input)
}

/// Parse the optional "or is tied for [greatest|lowest] [property]" tail.
/// Presence relaxes strict GT/LT to GE/LE. The matched superlative and
/// property must agree with the leading clause. The shared trailing
/// "among <filter>" is parsed by the caller.
fn parse_optional_tied_for_tail(
    input: &str,
    aggregate: AggregateFunction,
    property: ObjectProperty,
) -> OracleResult<'_, Comparator> {
    let strict_comparator = match aggregate {
        AggregateFunction::Max => Comparator::GT,
        AggregateFunction::Min => Comparator::LT,
        // Sum aggregate is not produced by this combinator; default to GT
        // for completeness (this arm is dead).
        AggregateFunction::Sum => Comparator::GT,
    };
    // The leading clause may end here (no "or is tied for" tail) — return GT/LT.
    let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(" or is tied for ").parse(input) else {
        return Ok((input, strict_comparator));
    };
    // Match the same superlative as the leading clause.
    let (rest, tied_aggregate) = parse_superlative_adjective(rest)?;
    if tied_aggregate != aggregate {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let (rest, _) = tag(" ").parse(rest)?;
    let (rest, tied_property) = parse_property_keyword(rest)?;
    if tied_property != property {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    // Strict-greater + tied = non-strict (>=); same for less-than + tied =
    // (<=). This is grammatical relaxation, not a CR-defined rule.
    let relaxed = match strict_comparator {
        Comparator::GT => Comparator::GE,
        Comparator::LT => Comparator::LE,
        other => other,
    };
    Ok((rest, relaxed))
}

/// Build the `StaticCondition::QuantityComparison` for a superlative-comparison
/// condition once all parts have been parsed.
///
/// `filter` is the population for the aggregate side; this function attaches
/// `FilterProp::OtherThanTriggerObject` so the runtime aggregate resolver
/// excludes the triggering object (CR 603.4 + CR 109.3).
fn build_superlative_comparison(
    filter: TargetFilter,
    property: ObjectProperty,
    comparator: Comparator,
    aggregate: AggregateFunction,
) -> StaticCondition {
    let lhs_qty = match property {
        ObjectProperty::Power => QuantityRef::Power {
            scope: ObjectScope::EventSource,
        },
        ObjectProperty::Toughness => QuantityRef::Toughness {
            scope: ObjectScope::EventSource,
        },
        ObjectProperty::ManaValue => QuantityRef::ObjectManaValue {
            scope: ObjectScope::EventSource,
        },
    };
    let aggregate_filter = attach_other_than_trigger_object(filter);
    StaticCondition::QuantityComparison {
        lhs: QuantityExpr::Ref { qty: lhs_qty },
        comparator,
        rhs: QuantityExpr::Ref {
            qty: QuantityRef::Aggregate {
                function: aggregate,
                property,
                filter: aggregate_filter,
            },
        },
    }
}

/// Attach `FilterProp::OtherThanTriggerObject` to a `TargetFilter`'s property
/// list so the runtime aggregate resolver excludes the triggering object.
///
/// CR 603.4 + CR 109.3: "each other <type>" in a trigger-anchored context
/// means "every <type> except the triggering object." `OtherThanTriggerObject`
/// is the established typed marker the resolver reads to perform the
/// subtraction (see its doc on `FilterProp`).
fn attach_other_than_trigger_object(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut tf) => {
            if !tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::OtherThanTriggerObject))
            {
                tf.properties.push(FilterProp::OtherThanTriggerObject);
            }
            TargetFilter::Typed(tf)
        }
        other => other,
    }
}

/// Parse "you have" quantity conditions: hand size, graveyard size, life.
///
/// Composable: "you have " + threshold/absence + quantity suffix.
/// Handles "you have no cards in hand", "you have N or more/fewer cards in hand",
/// "you have N or more cards in your graveyard", "you have N or more/less life".
fn parse_you_have_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you have ").parse(input)?;

    // Hand-size predicates compose for any player scope; "you have" → Controller.
    if let Some((rest, cond)) = parse_hand_size_predicate(rest, PlayerScope::Controller) {
        return Ok((rest, cond));
    }

    // CR 700.8c: "you have a full party" — controller's party size is 4 (the
    // cap defined in CR 700.8a). Composes through `parse_inner_condition` so
    // every consumer — static gates ("as long as you have a full party"),
    // trigger intervening-ifs (Nalia, Linvala), and clause-level conditions
    // ("if you have a full party, ... instead") — shares one parse path.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("a full party").parse(rest) {
        return Ok((
            rest,
            make_quantity_ge(
                QuantityRef::PartySize {
                    player: PlayerScope::Controller,
                },
                4,
            ),
        ));
    }

    // "you have exactly N life" → LifeTotal EQ N
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("exactly ").parse(rest) {
        let (rest, n) = parse_number(rest)?;
        let (rest, _) = tag(" life").parse(rest)?;
        return Ok((
            rest,
            make_quantity_comparison(
                QuantityRef::LifeTotal {
                    player: PlayerScope::Controller,
                },
                Comparator::EQ,
                n,
            ),
        ));
    }

    // CR 119: "you have at least N life more than your starting life total"
    // (Angel of Destiny intervening-if; also the "as long as" static gate) →
    // LifeAboveStarting ≥ N. Reuses the `LifeAboveStarting` building block
    // (current life − starting life total) so the trigger and static paths share
    // one canonical condition shape. Both "at least N" and "N or more" wordings
    // map to GE; the trailing-suffix tag gates this branch, so non-matching
    // "at least"/"or more" life phrases fall through to the bare-life arms below.
    if let Ok((after_n, n)) = alt((
        preceded(tag::<_, _, OracleError<'_>>("at least "), parse_number),
        terminated(parse_number, tag::<_, _, OracleError<'_>>(" or more")),
    ))
    .parse(rest)
    {
        if let Ok((rest, _)) =
            tag::<_, _, OracleError<'_>>(" life more than your starting life total").parse(after_n)
        {
            return Ok((rest, make_quantity_ge(QuantityRef::LifeAboveStarting, n)));
        }
    }

    // "you have N or more [you-only quantity-suffix]"
    let (rest, n) = parse_number(rest)?;

    if let Ok((after_or_more, _)) = tag::<_, _, OracleError<'_>>(" or more ").parse(rest) {
        // CR 603.4 + CR 404.2: Oversold Cemetery's intervening-if predicate
        // counts face-up creature cards in its controller's graveyard.
        if let Ok((rest, type_filters)) =
            parse_you_have_typed_cards_in_your_graveyard(after_or_more)
        {
            return Ok((
                rest,
                make_quantity_ge(
                    QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Graveyard,
                        card_types: type_filters,
                        filter: None,
                        scope: CountScope::Controller,
                    },
                    n,
                ),
            ));
        }
        // CR 603.4 + CR 404.2: Generic "you have N or more cards in your
        // graveyard" intervening-if predicates use the controller's graveyard
        // size.
        if let Ok((rest, _)) =
            tag::<_, _, OracleError<'_>>("cards in your graveyard").parse(after_or_more)
        {
            return Ok((
                rest,
                make_quantity_ge(
                    QuantityRef::GraveyardSize {
                        player: PlayerScope::Controller,
                    },
                    n,
                ),
            ));
        }
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

    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
}

/// CR 404.2: Parse the typed card-count tail of a controller graveyard predicate.
fn parse_you_have_typed_cards_in_your_graveyard(input: &str) -> OracleResult<'_, Vec<TypeFilter>> {
    let (rest, type_text) =
        take_until::<_, _, OracleError<'_>>(" cards in your graveyard").parse(input)?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" cards in your graveyard").parse(rest)?;
    let type_filters = parse_zone_card_type_text(type_text.trim());
    if type_filters.is_empty() {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((rest, type_filters))
}

/// Parse "that player has" / "that opponent has" quantity conditions.
///
/// CR 603.2b + CR 603.4 + CR 102.1: "that player" inside a Phase trigger's
/// intervening-if binds to the player whose phase fired the trigger
/// (CR 603.2b: phase-begin triggers fire at phase start; CR 102.1: that
/// phase belongs to the active player). The resulting
/// `PlayerScope::ScopedPlayer` is bound to the active player at trigger
/// fire time (see `triggers::build_triggered_ability`) and threaded into
/// trigger-condition quantity resolution
/// (`quantity::resolve_quantity_for_trigger_check`). CR 603.4 covers the
/// intervening-if recheck at resolution.
///
/// Covers the hand-size suffix family used by Ghirapur Orrery and related
/// "if that player has no cards in hand" / "N or more / N or fewer" patterns,
/// plus the life-total suffix family ("N or less life" / "N or more life")
/// used by Ezio Auditore da Firenze's combat-damage trigger. Graveyard
/// variants will compose in here as more cards exercise them.
fn parse_that_player_has_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    // CR 115.1 + CR 603.4: "that/target player/opponent has" decomposes the
    // reference axis ("that" vs. "target") from the subject noun
    // ("player" vs. "opponent"). "That" binds to the scoped event/iteration
    // player; "target" binds to the first player target of the ability.
    let (rest, player) = alt((
        value(
            PlayerScope::ScopedPlayer,
            tag::<_, _, OracleError<'_>>("that "),
        ),
        value(PlayerScope::Target, tag::<_, _, OracleError<'_>>("target ")),
    ))
    .parse(input)?;
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("player has "),
        tag::<_, _, OracleError<'_>>("opponent has "),
    ))
    .parse(rest)?;

    if let Some((rest, cond)) = parse_hand_size_predicate(rest, player.clone()) {
        return Ok((rest, cond));
    }
    // CR 119 + CR 603.4: life-total intervening-if predicates for the scoped
    // player ("if that player has 10 or less life"). The aggregate-less
    // `PlayerScope::ScopedPlayer` / `PlayerScope::Target` already names a
    // single player, so the comparison is a direct scalar (no existential
    // aggregate needed). Canonical card: Ezio Auditore da Firenze.
    if let Some((rest, cond)) = parse_life_predicate(rest, player) {
        return Ok((rest, cond));
    }
    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
}

/// Parse life-total predicates after a `<subject> has ` prefix has been
/// consumed. Returns `Some(condition)` on match.
///
/// Mirrors `parse_hand_size_predicate`: the only axis that varies is the
/// `PlayerScope` of the resulting `LifeTotal` ref. Used by
/// `parse_that_player_has_conditions` so any single-player subject ("that
/// player", "target player") composes with these life-total tails.
///
/// CR 119 (Life), CR 603.4 (intervening-if), CR 603.7c ("that player" anaphora
/// binds to the player event-context for damage triggers).
fn parse_life_predicate(rest: &str, player: PlayerScope) -> Option<(&str, StaticCondition)> {
    // CR 119: "no life" → LifeTotal EQ 0 (defensive, mirrors hand-size's
    // "no cards in hand"). Not currently printed on cards but kept symmetric
    // so the predicate covers the full grammatical family.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("no life").parse(rest) {
        return Some((
            rest,
            make_quantity_comparison(QuantityRef::LifeTotal { player }, Comparator::EQ, 0),
        ));
    }

    // CR 119 + CR 603.4: "N or less life" / "N or more life" → scalar
    // comparison against the scoped player's life total. Ezio Auditore da
    // Firenze canonical for the LE arm.
    let (after_n, n) = parse_number(rest).ok()?;
    if let Ok((rest, comparator)) = alt((
        value(
            Comparator::LE,
            tag::<_, _, OracleError<'_>>(" or less life"),
        ),
        value(
            Comparator::GE,
            tag::<_, _, OracleError<'_>>(" or more life"),
        ),
    ))
    .parse(after_n)
    {
        return Some((
            rest,
            make_quantity_comparison(QuantityRef::LifeTotal { player }, comparator, n),
        ));
    }
    None
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

/// Parse "you control" condition patterns. Exposed for rule-static parsers that
/// attach a trailing "unless you control <X>" clause as a negated condition.
pub(crate) fn parse_control_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        // CR 201.2 + CR 603.4: "you control N or more [type] with different names"
        // → QuantityComparison(ObjectCountDistinct[Name] >= N). Tried before the
        // plain ObjectCount arm so the `with different names` suffix is not
        // mis-classified as a raw count threshold. Field of the Dead canonical.
        parse_control_count_ge_distinct_quality,
        parse_control_count_ge_toughness_gt_power,
        parse_control_count_ge_subtype_disjunction,
        // "you control N or more [type]" → QuantityComparison(ObjectCount >= N)
        parse_control_count_ge,
        // "you control N or fewer [type]" → QuantityComparison(ObjectCount <= N)
        parse_control_count_le,
        // "you control exactly N [type]" → QuantityComparison(ObjectCount == N)
        parse_control_count_eq,
        // "you control a/an/another [type]" → IsPresent with filter
        parse_you_control_a,
        // "you don't control a/an [type]" → Not(IsPresent)
        parse_you_dont_control_a,
        // "you control no [type]" → Not(IsPresent)
        parse_you_control_no,
        // CR 702: "a creature you control has <keyword>" — subject-first
        // presence check (Odric, Lunarch Marshal). Grouped into the control
        // family so the parent dispatcher's `alt` arity stays within bounds.
        parse_creature_has_keyword,
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

/// CR 201.2 + CR 208.1 + CR 603.4: Parse
/// "you control N or more [type] with different [quality]" →
/// `QuantityComparison { ObjectCountDistinct[quality](filter) >= N }`.
///
/// Field of the Dead: "if you control seven or more lands with different
/// names". Coven cards: "if you control three or more creatures with different
/// powers". The quality axis is shared with search selection constraints and
/// `QuantityRef::ObjectCountDistinct`.
fn parse_control_count_ge_distinct_quality(input: &str) -> OracleResult<'_, StaticCondition> {
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
    let trimmed = remainder.trim_start();
    let (after_suffix, quality) = preceded(
        tag("with different "),
        alt((
            value(SharedQuality::Name, tag("names")),
            value(SharedQuality::Power, tag("powers")),
        )),
    )
    .parse(trimmed)?;
    let filter = inject_controller_you(filter);
    let consumed = after_suffix.as_ptr() as usize - input.as_ptr() as usize;
    Ok((
        &input[consumed..],
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCountDistinct {
                    filter,
                    qualities: vec![quality],
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
}

/// CR 208.1 + CR 603.4: Parse
/// "you control N or more creatures that each have toughness greater than their power"
/// as an `ObjectCount` threshold over the existing `ToughnessGTPower` filter property.
fn parse_control_count_ge_toughness_gt_power(input: &str) -> OracleResult<'_, StaticCondition> {
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
    let trimmed = remainder.trim_start();
    let (after_suffix, _) =
        tag("that each have toughness greater than their power").parse(trimmed)?;
    let filter = inject_controller_you(add_filter_property(filter, FilterProp::ToughnessGTPower));
    let consumed = after_suffix.as_ptr() as usize - input.as_ptr() as usize;
    Ok((
        &input[consumed..],
        make_quantity_comparison(QuantityRef::ObjectCount { filter }, Comparator::GE, n),
    ))
}

/// CR 205.3m + CR 603.4: Parse
/// "you control N or more [subtype] and/or [subtype]" threshold gates.
///
/// Tovolar's "Wolves and/or Werewolves" is the canonical surface form: the
/// threshold counts objects matching either creature subtype, not two separate
/// thresholds.
fn parse_control_count_ge_subtype_disjunction(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you control ").parse(input)?;
    let (rest, n) = parse_ge_threshold(rest)?;
    let (first, first_len) = parse_subtype(rest).ok_or_else(|| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Fail))
    })?;
    let rest = &rest[first_len..];
    let (rest, _) = alt((tag(" and/or "), tag(" or "))).parse(rest)?;
    let (second, second_len) = parse_subtype(rest).ok_or_else(|| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Fail))
    })?;
    let rest = &rest[second_len..];
    let filter = TargetFilter::Or {
        filters: vec![
            controlled_battlefield_subtype_filter(first),
            controlled_battlefield_subtype_filter(second),
        ],
    };
    Ok((
        rest,
        make_quantity_comparison(QuantityRef::ObjectCount { filter }, Comparator::GE, n),
    ))
}

fn controlled_battlefield_subtype_filter(subtype: String) -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::new(TypeFilter::Subtype(subtype))
            .controller(ControllerRef::You)
            .properties(vec![FilterProp::InZone {
                zone: Zone::Battlefield,
            }]),
    )
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

/// CR 702: Parse "[a/an] <type-phrase> has <keyword>" → `IsPresent` whose
/// filter carries `FilterProp::WithKeyword`.
///
/// Subject-first presence check (Odric, Lunarch Marshal: "a creature you
/// control has first strike"). Distinct from `parse_you_control_a` — here the
/// type phrase leads ("a creature you control") and is followed by a `has
/// <keyword>` predicate, rather than the verb leading ("you control a
/// creature"). Generalized over every evergreen keyword in the `KEYWORDS`
/// table and every type phrase `parse_type_phrase` recognizes, so it covers
/// the whole class of "a/an <permanent> <controller-clause> has <keyword>"
/// conditions, not one card.
fn parse_creature_has_keyword(input: &str) -> OracleResult<'_, StaticCondition> {
    // Optional leading article — `parse_type_phrase` also strips it, but the
    // article may precede a non-type word, so guard it explicitly first.
    let (rest, _) = opt(parse_article).parse(input)?;
    // `parse_type_phrase` consumes the type word AND any "you control" /
    // "an opponent controls" controller suffix, setting `controller` on the
    // returned filter. The remainder begins at the `has <keyword>` predicate.
    let (filter, remainder) = parse_type_phrase(rest);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let (after_has, _) = preceded(opt(tag(" ")), tag("has ")).parse(remainder)?;
    let (after_kw, keyword_name) = parse_keyword_name(after_has)?;
    let keyword: Keyword = keyword_name
        .parse()
        .map_err(|_| nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Fail)))?;
    let filter = add_filter_property(filter, FilterProp::WithKeyword { value: keyword });
    Ok((
        after_kw,
        StaticCondition::IsPresent {
            filter: Some(filter),
        },
    ))
}

fn add_filter_property(filter: TargetFilter, prop: FilterProp) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            typed.properties.push(prop);
            TargetFilter::Typed(typed)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|filter| add_filter_property(filter, prop.clone()))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|filter| add_filter_property(filter, prop.clone()))
                .collect(),
        },
        other => TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(TypedFilter::default().properties(vec![prop])),
            ],
        },
    }
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

/// Parse "you control exactly N [type]" → QuantityComparison(ObjectCount == N).
fn parse_control_count_eq(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you control exactly ").parse(input)?;
    let (rest, n) = parse_number(rest)?;
    let rest = rest.trim_start();
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
        make_quantity_comparison(QuantityRef::ObjectCount { filter }, Comparator::EQ, n),
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
pub(crate) fn inject_controller_you(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut tf) => {
            tf.controller = Some(ControllerRef::You);
            if !tf
                .properties
                .iter()
                .any(|prop| matches!(prop, FilterProp::InZone { .. }))
            {
                tf.properties.push(FilterProp::InZone {
                    zone: Zone::Battlefield,
                });
            }
            TargetFilter::Typed(tf)
        }
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
                exclude: None,
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

/// CR 107.3 + CR 608.2c + CR 700.5: "X is <comparator> <quantity>"
/// intervening-if where the LHS is the spell-bound variable X (CR 107.3).
/// Pairs with the X-substitution post-pass in `apply_where_x_ability_condition`
/// so that, after `compute_sentence_where_x` forward-fills the binding into
/// the trailing sentence, the LHS resolves to the bound dynamic ref (e.g.
/// Thassa's Oracle's Devotion{Blue}) at AST-finalization time.
///
/// Scope is intentionally restricted to LHS=Variable("X") (not a general
/// "<quantity> is <comp> <quantity>" combinator) so that the legacy
/// devotion / hand-size fallback paths in `parse_static_condition`
/// (oracle_static.rs) — which produce `DevotionGE` and `HandSize`-based
/// `QuantityComparison` shapes for "your devotion to <color> is less than N"
/// and "the number of cards in your hand is greater than your life total" —
/// still win for those patterns. A broader `parse_quantity` LHS would steal
/// those phrases and rewrite their AST shape, breaking the derived-state
/// dirty tracker (see `game/derived.rs:65`) that scans for `DevotionGE`.
fn parse_quantity_quantity_comparison(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("x").parse(input)?;
    // Require word-boundary after 'x' so we don't consume the leading 'x' in
    // "x is" but reject e.g. "x or more" / "xyz".
    let (rest, _) = tag(" is ").parse(rest)?;
    let (rest, comparator) = parse_life_total_comparator(rest)?;
    let (rest, rhs) = nom_quantity::parse_quantity(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            comparator,
            rhs,
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

/// CR 113.6b: Self-referential subject tokens that anchor a zone condition.
/// `~` is the canonical card-name placeholder; the `this <type>` variants are
/// equivalent self-references for the printed types that may appear in
/// "as long as this <type> is ..." clauses.
fn parse_source_self_token(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag("~"),
            tag("this card"),
            tag("this enchantment"),
            tag("this permanent"),
            tag("this creature"),
            tag("this artifact"),
        )),
    )
    .parse(input)
}

/// CR 113.6b: A zone phrase that follows the verb "is" in a source-referential
/// condition — e.g., " on the battlefield", " in your graveyard",
/// " in the command zone". Returns the typed `Zone` referenced.
///
/// Composes the shared `parse_zone_word` building block (the canonical
/// zone-token vocabulary in `oracle_target.rs`) with the preposition +
/// qualifier glue that printed Oracle text uses for source-referential
/// conditions. New zone tokens MUST be added to `parse_zone_word`, not here —
/// the per-qualifier arms below pick them up automatically.
///
/// CR-correct qualifier mapping (printed Oracle text always uses exactly one
/// of these forms per zone):
///   - " on the <Z>"  — only Battlefield (CR 400.1).
///   - " in the <Z>"  — shared zones with definite article (CR 408 command).
///   - " in your <Z>" — player-specific zones (CR 401 / 402 / 403).
///   - " in <Z>"      — Exile (shared zone with no possessive; CR 406).
fn parse_zone_phrase(input: &str) -> OracleResult<'_, Zone> {
    // Parse a zone token and assert it matches `allowed`. Composes
    // `parse_zone_word` (the canonical zone vocabulary) with the per-
    // qualifier CR constraint, so adding a new zone is a single edit in
    // `oracle_target.rs`.
    fn zone_in<F>(i: &str, allowed: F) -> OracleResult<'_, Zone>
    where
        F: Fn(&Zone) -> bool,
    {
        let (rest, zone) = parse_zone_word(i)?;
        if !allowed(&zone) {
            return Err(nom::Err::Error(nom::error::Error::new(
                i,
                nom::error::ErrorKind::Fail,
            )));
        }
        let (rest, _) = peek_zone_boundary(rest)?;
        Ok((rest, zone))
    }

    alt((
        // " on the <Z>" — CR 400.1: only the battlefield uses "on".
        preceded(tag(" on the "), |i| {
            zone_in(i, |z| matches!(z, Zone::Battlefield))
        }),
        // " in the <Z>" — CR 408: shared zones (command zone) take "the".
        // Bare-word player zones (graveyard/hand/library) print "in your <Z>",
        // not "in the <Z>" — rejecting them here keeps the qualifier→zone
        // mapping CR-faithful.
        preceded(tag(" in the "), |i| {
            zone_in(i, |z| matches!(z, Zone::Command))
        }),
        // " in your <Z>" — CR 401/402/403: player-specific owned zones.
        preceded(tag(" in your "), |i| {
            zone_in(i, |z| {
                matches!(z, Zone::Graveyard | Zone::Hand | Zone::Library)
            })
        }),
        // " in <Z>" — CR 406: exile is a shared zone with no possessive.
        preceded(tag(" in "), |i| zone_in(i, |z| matches!(z, Zone::Exile))),
    ))
    .parse(input)
}

/// CR 113.6b: Parse "<source> is <zone-phrase> [or <zone-phrase> ...]" into a
/// typed `StaticCondition`. A single zone produces `SourceInZone { zone }`;
/// a disjunction produces `StaticCondition::Or` over the per-zone arms — the
/// shape `populate_active_zones_from_condition` walks to seed `active_zones`.
///
/// Covers the class of Eminence-style "as long as ~ is in the command zone or
/// on the battlefield" statics (The Ur-Dragon, Edgar Markov, Oloro, Inalla,
/// etc.) without enumerating each (zone × zone) permutation.
fn parse_source_in_zone_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_source_self_token(input)?;
    // CR 113.6b vs CR 113.6c: the copula polarity decides which zone-function
    // rule applies. Affirmative ("~ is on the battlefield") names the zones the
    // ability functions IN (CR 113.6b). Negated ("~ isn't on the battlefield" /
    // "~ is not on the battlefield") names the zones it does NOT function in,
    // i.e. it functions everywhere except those zones (CR 113.6c) — modeled by
    // wrapping the affirmative reading in `Not`. Negated copulae are tried first
    // so " is not " is not greedily split into " is " + "not …" (mirrors the
    // polarity alternation in `parse_recipient_is_filter_condition`).
    let (rest, negated) = alt((
        value(true, alt((tag(" isn't"), tag(" is not")))),
        value(false, tag(" is")),
    ))
    .parse(rest)?;
    let (rest, first) = parse_zone_phrase(rest)?;
    // CR 113.6b: a single ability that names multiple zones functions in each
    // of them — the "or"-separated zone list composes disjunctively across the
    // listed zones. ("or" is English grammar, not a CR construct; the rules
    // authority for the disjunction is the same CR 113.6b that authorizes the
    // zone clause itself.)
    let (rest, more) = many0(preceded(parse_zone_list_separator, parse_zone_phrase)).parse(rest)?;
    let condition = if more.is_empty() {
        StaticCondition::SourceInZone { zone: first }
    } else {
        let mut conditions = Vec::with_capacity(more.len() + 1);
        conditions.push(StaticCondition::SourceInZone { zone: first });
        for zone in more {
            conditions.push(StaticCondition::SourceInZone { zone });
        }
        StaticCondition::Or { conditions }
    };
    let condition = if negated {
        // CR 113.6c: an ability that states which zones it doesn't function in
        // functions everywhere except those zones.
        StaticCondition::Not {
            condition: Box::new(condition),
        }
    } else {
        condition
    };
    Ok((rest, condition))
}

fn parse_zone_list_separator(input: &str) -> OracleResult<'_, ()> {
    value((), alt((tag(", or"), tag(" or"), tag(",")))).parse(input)
}

/// CR 113.6b: Parse zone-based source conditions.
/// Handles all player-specific zones (graveyard, hand, library) with "your",
/// the shared exile and command zones (no "your"), and disjunctions across
/// any pair of those zones ("~ is in the command zone or on the battlefield").
fn parse_zone_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        // CR 702.62b: A card is suspended while it is in exile with a time
        // counter on it. The "has suspend" component is guaranteed by cards
        // that print this source-referential condition. Tried first so the
        // generic zone-phrase parser does not misclassify "~ is suspended"
        // as an unanchored "is" + zone match.
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
        // CR 104.2b + CR 104.3c: "your library has no cards in it" / "your
        // library is empty" — the empty-library antecedent shared by the
        // alternate-win-on-draw class (Laboratory Maniac, Jace, Wielder of
        // Mysteries: "If you would draw a card while your library has no cards
        // in it, you win the game instead"). Maps to a controller library
        // count of zero so the gate composes through `parse_inner_condition`
        // for replacement antecedents, trigger intervening-ifs, and statics.
        parse_library_empty_condition,
        // CR 113.6b: Generic "<source> is <zone> [or <zone>]" form.
        parse_source_in_zone_condition,
    ))
    .parse(input)
}

/// CR 401.1: Count of cards in the controller's library, compared against zero.
///
/// Recognizes "your library has no cards in it" and "your library is empty"
/// (the empty-library win antecedent). The library count is expressed with the
/// existing `ZoneCardCount` building block (zone = Library, no type filter,
/// controller scope) rather than a bespoke leaf, so the resolver path is shared
/// with every other zone-count condition.
fn parse_library_empty_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("your library ").parse(input)?;
    let (rest, _) = alt((tag("has no cards in it"), tag("is empty"))).parse(rest)?;
    Ok((
        rest,
        make_quantity_comparison(
            QuantityRef::ZoneCardCount {
                zone: ZoneRef::Library,
                card_types: Vec::new(),
                filter: None,
                scope: CountScope::Controller,
            },
            Comparator::EQ,
            0,
        ),
    ))
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

/// CR 117.1: "this is the first spell you've cast this game" / "this spell
/// is the first spell you've cast this game" — gates an instead-override on
/// the controller's per-game cast count being zero (i.e., this is the first
/// spell). The subject ("this" / "this spell") is anaphoric to the cast
/// itself; both forms compose with `QuantityRef::SpellsCastThisGame == 0`.
///
/// Maps to `StaticCondition::QuantityComparison` so the existing
/// `static_condition_to_ability_condition` bridge converts it to
/// `AbilityCondition::QuantityCheck` in instead-clause assembly.
fn parse_first_spell_this_game_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("this is "), tag("this spell is "))).parse(input)?;
    let (rest, _) = tag("the first spell you've cast this game").parse(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::SpellsCastThisGame {
                    scope: CountScope::Controller,
                    filter: None,
                },
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        },
    ))
}

/// Parse "you've [done X] this turn" conditions.
///
/// CR 119: Life gain/loss event conditions.
/// CR 700.13: Crime tracking.
fn parse_youve_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you've ").parse(input)?;
    if let Ok(parsed) = parse_youve_played_land_or_cast_spell_this_turn(rest) {
        return Ok(parsed);
    }
    alt((
        parse_youve_spell_history_condition,
        parse_youve_card_history_condition,
        parse_youve_zone_history_condition,
        parse_youve_life_history_condition,
        parse_youve_combat_history_condition,
        parse_youve_player_action_history_condition,
        // CR 305.2a + CR 603.4: "you've played a land [this turn]" — land-play
        // history condition. Backs intervening-if predicates like Spider-Man
        // 2099's "if you've played a land or cast a spell this turn from
        // anywhere other than your hand".
        // The " this turn" suffix is optional so the combinator also serves as
        // the LHS of `parse_condition_disjunction` when "played a land" is
        // followed by " or" rather than " this turn".
        map((tag("played a land"), opt(tag(" this turn"))), |_| {
            make_quantity_ge(
                QuantityRef::LandsPlayedThisTurn {
                    player: PlayerScope::Controller,
                    from_zones: None,
                },
                1,
            )
        }),
    ))
    .parse(rest)
}

fn parse_youve_played_land_or_cast_spell_this_turn(
    input: &str,
) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("played a land or cast ").parse(input)?;
    let (rest, spell_condition) = parse_one_spell_this_turn_after_cast(rest)?;
    let land_from_zones = spell_condition_origin_zones(&spell_condition);
    Ok((
        rest,
        StaticCondition::Or {
            conditions: vec![
                make_quantity_ge(
                    QuantityRef::LandsPlayedThisTurn {
                        player: PlayerScope::Controller,
                        from_zones: land_from_zones,
                    },
                    1,
                ),
                spell_condition,
            ],
        },
    ))
}

fn spell_condition_origin_zones(condition: &StaticCondition) -> Option<Vec<Zone>> {
    let StaticCondition::QuantityComparison {
        lhs:
            QuantityExpr::Ref {
                qty:
                    QuantityRef::SpellsCastThisTurn {
                        filter: Some(filter),
                        ..
                    },
            },
        ..
    } = condition
    else {
        return None;
    };
    target_filter_origin_zones(filter)
}

fn target_filter_origin_zones(filter: &TargetFilter) -> Option<Vec<Zone>> {
    match filter {
        TargetFilter::Typed(typed) => typed.properties.iter().find_map(|prop| match prop {
            FilterProp::InZone { zone } => Some(vec![*zone]),
            FilterProp::InAnyZone { zones } => Some(zones.clone()),
            _ => None,
        }),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().find_map(target_filter_origin_zones)
        }
        _ => None,
    }
}

fn parse_youve_spell_history_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        parse_cast_spell_count_this_turn,
        |input| parse_another_spell_cast_this_turn(input, 2),
        parse_cast_one_spell_this_turn,
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
    ))
    .parse(input)
}

fn parse_youve_card_history_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        parse_discarded_card_this_turn_after_actor,
        parse_youve_drawn_cards_this_turn,
    ))
    .parse(input)
}

fn parse_youve_zone_history_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        parse_sacrificed_this_turn_after_actor,
        // "you've descended this turn"
        value(
            make_quantity_ge(QuantityRef::DescendedThisTurn, 1),
            tag("descended this turn"),
        ),
    ))
    .parse(input)
}

fn parse_youve_life_history_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
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
    ))
    .parse(input)
}

fn parse_youve_combat_history_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    // "you've attacked this turn" / "you've attacked with a creature this turn"
    value(
        make_quantity_ge(
            QuantityRef::AttackedThisTurn {
                scope: CountScope::Controller,
                filter: None,
            },
            1,
        ),
        alt((
            tag("attacked with a creature this turn"),
            tag("attacked this turn"),
        )),
    )
    .parse(input)
}

fn parse_youve_player_action_history_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        value(
            make_quantity_ge(QuantityRef::CrimesCommittedThisTurn, 1),
            tag("committed a crime this turn"),
        ),
        parse_player_action_this_turn_body,
    ))
    .parse(input)
}

/// Parse event-state conditions: "a creature died this turn", "you attacked this turn",
/// "an opponent lost life this turn", "no spells were cast last turn", etc.
///
/// These are game-state boolean checks expressible as QuantityComparison.
fn parse_event_state_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        // Broad/negated event patterns must precede positive domain parsers.
        parse_compound_verb_condition,
        parse_you_didnt_this_turn,
        parse_zone_history_condition,
        parse_life_history_condition,
        parse_discard_history_condition,
        parse_combat_history_condition,
        parse_no_attacked_this_turn,
        parse_player_action_this_turn,
        parse_spell_history_condition,
        parse_counter_history_condition,
        parse_board_state_condition,
    ))
    .parse(input)
}

fn parse_zone_history_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        parse_card_left_your_graveyard_this_turn,
        parse_permanent_put_into_your_hand_from_battlefield_this_turn,
        parse_card_put_into_your_graveyard_from_anywhere_this_turn,
        parse_object_put_into_graveyard_from_battlefield_this_turn,
        parse_creature_died_this_turn_conditions,
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
        value(
            make_quantity_ge(QuantityRef::DescendedThisTurn, 1),
            tag("you descended this turn"),
        ),
        parse_you_created_token_this_turn,
        parse_you_sacrificed_this_turn,
    ))
    .parse(input)
}

fn parse_card_left_your_graveyard_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    value(
        make_quantity_ge(
            QuantityRef::ZoneChangeCountThisTurn {
                from: Some(Zone::Graveyard),
                to: None,
                filter: add_owned_you_with_props(TargetFilter::Any, &[FilterProp::NonToken]),
            },
            1,
        ),
        tag("a card left your graveyard this turn"),
    )
    .parse(input)
}

fn parse_permanent_put_into_your_hand_from_battlefield_this_turn(
    input: &str,
) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_article(input)?;
    let (rest, type_text) =
        take_until(" was put into your hand from the battlefield this turn").parse(rest)?;
    let (rest, _) = tag(" was put into your hand from the battlefield this turn").parse(rest)?;
    let (filter, leftover) = parse_type_phrase(type_text.trim());
    if !leftover.trim().is_empty() || filter == TargetFilter::Any {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }

    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::ZoneChangeCountThisTurn {
                from: Some(Zone::Battlefield),
                to: Some(Zone::Hand),
                filter: add_owned_you_with_props(filter, &[]),
            },
            1,
        ),
    ))
}

fn parse_card_put_into_your_graveyard_from_anywhere_this_turn(
    input: &str,
) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_article(input)?;
    let suffix = " card was put into your graveyard from anywhere this turn";
    let (rest, type_text) = take_until(suffix).parse(rest)?;
    let (rest, _) = tag(suffix).parse(rest)?;
    let (filter, leftover) = parse_type_phrase(type_text.trim());
    if !leftover.trim().is_empty() || filter == TargetFilter::Any {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }

    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::ZoneChangeCountThisTurn {
                from: None,
                to: Some(Zone::Graveyard),
                filter: add_owned_you_with_props(filter, &[FilterProp::NonToken]),
            },
            1,
        ),
    ))
}

fn parse_object_put_into_graveyard_from_battlefield_this_turn(
    input: &str,
) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_article(input)?;
    let suffix = " was put into a graveyard from the battlefield this turn";
    let (rest, type_text) = take_until(suffix).parse(rest)?;
    let (rest, _) = tag(suffix).parse(rest)?;
    let (filter, leftover) = parse_type_phrase(type_text.trim());
    if !leftover.trim().is_empty() || filter == TargetFilter::Any {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }

    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::ZoneChangeCountThisTurn {
                from: Some(Zone::Battlefield),
                to: Some(Zone::Graveyard),
                filter,
            },
            1,
        ),
    ))
}

/// CR 109.5: Append `Owned { controller: You }` plus any caller-supplied
/// `extras` to `filter`'s property set, skipping props whose variant tag
/// already appears (presence is variant-tag equality via `mem::discriminant`,
/// matching the original tag-only `matches!(p, FilterProp::X { .. })` checks).
/// Pass `&[]` for the bare "owned by you" case; pass `&[FilterProp::NonToken]`
/// for "you own a nontoken card" patterns. Wraps `TargetFilter::Any` into a
/// fresh `Typed` filter carrying the same property set; returns other variants
/// (`Player`, `SpecificObject`, …) unchanged because owner-tagging is
/// meaningless on non-typed shapes.
fn add_owned_you_with_props(filter: TargetFilter, extras: &[FilterProp]) -> TargetFilter {
    let owned = FilterProp::Owned {
        controller: ControllerRef::You,
    };
    let push_unique_by_tag = |props: &mut Vec<FilterProp>, prop: FilterProp| {
        let tag = std::mem::discriminant(&prop);
        if !props.iter().any(|p| std::mem::discriminant(p) == tag) {
            props.push(prop);
        }
    };
    match filter {
        TargetFilter::Typed(mut typed) => {
            push_unique_by_tag(&mut typed.properties, owned);
            for extra in extras {
                push_unique_by_tag(&mut typed.properties, extra.clone());
            }
            TargetFilter::Typed(typed)
        }
        TargetFilter::Any => {
            let mut props = vec![owned];
            props.extend(extras.iter().cloned());
            TargetFilter::Typed(TypedFilter::default().properties(props))
        }
        other => other,
    }
}

fn parse_life_history_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
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
        // "you gained life this turn" / "you gained N or more life this turn"
        parse_you_gained_life_this_turn,
    ))
    .parse(input)
}

fn parse_source_damage_threshold_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_article(input)?;
    let (rest, _) = tag("source ").parse(rest)?;
    let (rest, controller) = alt((
        value(ControllerRef::You, tag("you controlled")),
        value(ControllerRef::Opponent, tag("an opponent controlled")),
    ))
    .parse(rest)?;
    let (rest, _) = tag(" dealt ").parse(rest)?;
    let (rest, amount) = parse_number(rest)?;
    let (rest, _) = tag(" or more damage this turn").parse(rest)?;

    // CR 120.9: "by a specific source controlled by X" — group damage records
    // by source id then take the max per-source sum (matches "any one source"
    // wording; damage from multiple sources is not combined).
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::Typed(
                    TypedFilter::default().controller(controller),
                )),
                target: Box::new(TargetFilter::Any),
                aggregate: AggregateFunction::Max,
                group_by: Some(DamageGroupKey::SourceId),
                damage_kind: DamageKindFilter::Any,
            },
            amount,
        ),
    ))
}

fn parse_discard_history_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        // CR 701.9 + CR 603.4: "an opponent discarded a card this turn"
        value(
            make_quantity_ge(
                QuantityRef::CardsDiscardedThisTurn {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Sum,
                    },
                },
                1,
            ),
            alt((
                tag("an opponent discarded a card this turn"),
                tag("any opponent discarded a card this turn"),
            )),
        ),
        // CR 701.9 + CR 603.4: "a player discarded a card this turn" — any
        // player, including you (The Raven Man). Summing discards across all
        // players makes the threshold true whenever anyone discarded; without
        // this arm the intervening-if is dropped and the trigger fires even
        // when no discard occurred.
        value(
            make_quantity_ge(
                QuantityRef::CardsDiscardedThisTurn {
                    player: PlayerScope::AllPlayers {
                        aggregate: AggregateFunction::Sum,
                        exclude: None,
                    },
                },
                1,
            ),
            alt((
                tag("a player discarded a card this turn"),
                tag("any player discarded a card this turn"),
            )),
        ),
        parse_you_discarded_card_this_turn,
    ))
    .parse(input)
}

fn parse_combat_history_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        // "you attacked this turn" (without "you've" prefix)
        value(
            make_quantity_ge(
                QuantityRef::AttackedThisTurn {
                    scope: CountScope::Controller,
                    filter: None,
                },
                1,
            ),
            alt((
                tag("you attacked with a creature this turn"),
                tag("you attacked this turn"),
            )),
        ),
    ))
    .parse(input)
}

/// Parse "no [type] attacked this turn" → global AttackedThisTurn count EQ 0.
///
/// CR 508.1a + CR 603.4: Global absence of attackers this turn (Charging
/// Cinderhorn, Keldon Twilight). Composed as `AttackedThisTurn { scope: All,
/// filter: Some(type) } == 0` rather than a battlefield ObjectCount check so
/// attackers that left the battlefield still satisfy "attacked this turn".
fn parse_no_attacked_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("no ").parse(input)?;
    let (rest, type_text) = take_until(" attacked this turn").parse(rest)?;
    let (rest, _) = tag(" attacked this turn").parse(rest)?;
    let (filter, leftover) = parse_type_phrase(type_text.trim());
    if !leftover.trim().is_empty() || matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::AttackedThisTurn {
                    scope: CountScope::All,
                    filter: Some(filter),
                },
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        },
    ))
}

fn parse_spell_history_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        parse_you_drew_cards_this_turn,
        parse_opponent_drew_cards_this_turn,
        // "you cast another spell this turn" / "you cast a [type] spell this turn"
        parse_you_cast_spell_this_turn,
        // CR 606.1 + CR 603.4: "you activated a loyalty ability of a planeswalker this turn"
        // / "you activated a loyalty ability this turn" — The Chain Veil class.
        parse_you_activated_loyalty_this_turn,
        // "no spells were cast last turn" (werewolf)
        value(
            make_quantity_comparison(QuantityRef::SpellsCastLastTurn, Comparator::EQ, 0),
            tag("no spells were cast last turn"),
        ),
        // "two or more spells were cast last turn" / "a player cast two or more spells last turn"
        parse_spells_cast_last_turn,
        parse_you_cast_both_spell_kinds_this_turn,
        // CR 702.185c: "a spell was warped this turn" — any player cast a spell
        // for its warp cost this turn.
        value(
            StaticCondition::SpellCastWithVariantThisTurn {
                variant: crate::types::game_state::CastingVariant::Warp,
            },
            tag("a spell was warped this turn"),
        ),
    ))
    .parse(input)
}

/// CR 606.1 + CR 603.4: "you activated a loyalty ability of a planeswalker this
/// turn" / "you activated a loyalty ability this turn" / "you've activated ..."
/// — The Chain Veil class. Each loyalty activation increments the
/// `loyalty_abilities_activated_this_turn[controller]` counter
/// (see `planeswalker::record_loyalty_activation`); the intervening-if is true
/// whenever the counter is `>= 1`.
fn parse_you_activated_loyalty_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    value(
        make_quantity_ge(
            QuantityRef::LoyaltyAbilitiesActivatedThisTurn {
                player: PlayerScope::Controller,
            },
            1,
        ),
        (
            alt((tag("you activated "), tag("you've activated "))),
            tag("a loyalty ability"),
            opt(tag(" of a planeswalker")),
            tag(" this turn"),
        ),
    )
    .parse(input)
}

fn parse_counter_history_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    // "you put a counter on a permanent this turn"
    parse_counter_added_this_turn(input)
}

fn parse_board_state_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    // "no creatures are on the battlefield"
    parse_no_on_battlefield(input)
}

fn player_action_this_turn_condition(action: PlayerActionKind) -> StaticCondition {
    make_quantity_ge(
        QuantityRef::PlayerActionsThisTurn {
            player: PlayerScope::Controller,
            action,
        },
        1,
    )
}

fn parse_player_action_this_turn_body(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        value(
            player_action_this_turn_condition(PlayerActionKind::Surveil),
            tag("surveilled this turn"),
        ),
        value(
            player_action_this_turn_condition(PlayerActionKind::Scry),
            alt((tag("scried this turn"), tag("scryed this turn"))),
        ),
        value(
            player_action_this_turn_condition(PlayerActionKind::CollectEvidence),
            tag("collected evidence this turn"),
        ),
    ))
    .parse(input)
}

fn parse_player_action_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    preceded(
        alt((tag("you "), tag("you've "), tag("you have "))),
        parse_player_action_this_turn_body,
    )
    .parse(input)
}

fn parse_creature_died_this_turn_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        parse_died_under_your_control_this_turn,
        // "a creature died this turn" (Morbid) → zone-change count >= 1
        value(
            make_quantity_ge(creatures_died_this_turn_ref(), 1),
            alt((
                tag("a creature died this turn"),
                tag("a creature died under your control this turn"),
            )),
        ),
    ))
    .parse(input)
}

/// CR 106.3 + CR 601.2h + CR 603.4: Parse
/// "mana from [a/an] <source-filter> [source] was spent to cast <self>" as a
/// positive quantity check over the source-qualified spent-mana snapshots.
///
/// CR 400.7d: the subject anaphora selects the scope — "this spell" on a
/// resolving sorcery (e.g. Devour Intellect) is `SelfObject`, "that spell" on a
/// triggered ability is `TriggeringSpell`.
fn parse_source_qualified_mana_spent_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("mana from ").parse(input)?;
    let (rest, source_filter) = nom_quantity::parse_mana_source_filter(rest)?;
    let (rest, _) = tag(" was spent to cast ").parse(rest)?;
    let (rest, scope) = nom_quantity::parse_mana_spent_self_subject(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope,
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
///
/// CR 400.7d: the subject anaphora selects the scope (see
/// `parse_source_qualified_mana_spent_condition`).
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
    let (rest, scope) = nom_quantity::parse_mana_spent_self_subject(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope,
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
/// **or** `A > T`. The "this creature's" subject, including the normalized
/// "~'s" self-reference form, carries Increment's implicit source-is-creature
/// intervening-if; "this permanent's" stays as a plain P/T comparison for
/// non-Increment siblings.
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
    let (rest, requires_creature_source) = alt((
        value(true, tag("this creature's ")),
        value(false, tag("this permanent's ")),
        value(true, tag("~'s ")),
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
            // "the amount of mana you spent" is the triggering-spell intervening-if
            // (SOS Increment); the trigger event is in scope at evaluation time.
            scope: CastManaObjectScope::TriggeringSpell,
            metric: crate::types::ability::CastManaSpentMetric::Total,
        },
    };
    let build = |qty: QuantityRef| StaticCondition::QuantityComparison {
        lhs: lhs.clone(),
        comparator,
        rhs: QuantityExpr::Ref { qty },
    };
    let comparison = match second {
        Some(second) if second != first => StaticCondition::Or {
            conditions: vec![build(first), build(second)],
        },
        _ => build(first),
    };
    let result = if requires_creature_source {
        StaticCondition::And {
            conditions: vec![
                StaticCondition::SourceMatchesFilter {
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                },
                comparison,
            ],
        }
    } else {
        comparison
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
///
/// CR 400.7d: the subject anaphora selects the scope — "that spell" stays
/// `TriggeringSpell`; "this spell"/"it" on a resolving spell is `SelfObject`.
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
    let (rest, scope) = nom_quantity::parse_mana_spent_self_subject(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope,
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
/// "you [verb1] (and|or) [verb2] life this turn" where each verb is gained/lost.
/// Example: "you gained and lost life this turn" → And(LifeGainedThisTurn >= 1,
/// LifeLostThisTurn >= 1); "you gained or lost life this turn" → Or(...) (Star
/// Charter, Starseer Mentor, Starlit Soothsayer).
fn parse_compound_verb_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    // "gained"/"lost" → the matching controller-scoped "life this turn" QuantityRef.
    fn life_verb(i: &str) -> OracleResult<'_, QuantityRef> {
        alt((
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
        .parse(i)
    }

    let (rest, _) = alt((tag("you "), tag("you've "))).parse(input)?;
    let (rest, lhs) = life_verb(rest)?;
    // CR 119: the connective selects the boolean shape — "and" requires both
    // life changes, "or" requires either — over the shared LifeGained/LifeLost
    // ThisTurn QuantityRef building blocks.
    let (rest, is_or) = alt((value(false, tag(" and ")), value(true, tag(" or ")))).parse(rest)?;
    let (rest, rhs) = life_verb(rest)?;
    let (rest, _) = tag(" life this turn").parse(rest)?;

    let conditions = vec![make_quantity_ge(lhs, 1), make_quantity_ge(rhs, 1)];
    let condition = if is_or {
        StaticCondition::Or { conditions }
    } else {
        StaticCondition::And { conditions }
    };
    Ok((rest, condition))
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
                    exclude: None,
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

fn parse_youve_drawn_cards_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("drawn ").parse(input)?;
    parse_drawn_cards_this_turn(rest)
}

fn parse_drawn_cards_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, n) = parse_ge_threshold(input)?;
    let (rest, _) = tag("cards this turn").parse(rest)?;
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::CardsDrawnThisTurn {
                player: PlayerScope::Controller,
            },
            n,
        ),
    ))
}

fn parse_you_drew_cards_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("you drew "), tag("you've drawn "))).parse(input)?;
    parse_drawn_cards_this_turn(rest)
}

fn parse_opponent_drew_cards_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((
        tag("an opponent drew "),
        tag("an opponent has drawn "),
        tag("an opponent's drawn "),
    ))
    .parse(input)?;
    let (rest, n) = parse_ge_threshold(rest)?;
    let (rest, _) = tag("cards this turn").parse(rest)?;
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::CardsDrawnThisTurn {
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
    if let Ok((rest, condition)) = parse_spell_count_this_turn(rest) {
        return Ok((rest, condition));
    }
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
    parse_one_spell_this_turn_after_cast(rest)
}

fn parse_cast_one_spell_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("cast ").parse(input)?;
    parse_one_spell_this_turn_after_cast(rest)
}

fn parse_one_spell_this_turn_after_cast(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, filter) = parse_one_spell_this_turn_filter(input)?;
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Controller,
                filter,
            },
            1,
        ),
    ))
}

fn parse_one_spell_this_turn_filter(input: &str) -> OracleResult<'_, Option<TargetFilter>> {
    let (rest, _) = parse_article(input)?;
    let (rest, type_text) = take_until(" this turn").parse(rest)?;
    let (rest, _) = tag(" this turn").parse(rest)?;
    let (rest, origin_props) = parse_spell_history_post_this_turn_origin(rest);
    if let Ok((empty, _)) = tag::<_, _, OracleError<'_>>("spell").parse(type_text) {
        if empty.trim().is_empty() {
            return Ok((
                rest,
                origin_props
                    .map(|props| add_spell_history_filter_qualifiers(TargetFilter::Any, props)),
            ));
        }
    }
    let Some(filter) = parse_spell_history_filter(type_text) else {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    };
    Ok((
        rest,
        Some(
            origin_props
                .map(|props| add_spell_history_filter_qualifiers(filter.clone(), props))
                .unwrap_or(filter),
        ),
    ))
}

fn parse_you_cast_both_spell_kinds_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("you've cast both "), tag("you cast both "))).parse(input)?;
    let (rest, first_text) = take_until(" and ").parse(rest)?;
    let (rest, _) = tag(" and ").parse(rest)?;
    let (rest, second_text) = take_until(" this turn").parse(rest)?;
    let (rest, _) = tag(" this turn").parse(rest)?;
    let Some(first_filter) = parse_spell_history_filter_with_optional_article(first_text) else {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    };
    let Some(second_filter) = parse_spell_history_filter_with_optional_article(second_text) else {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    };
    Ok((
        rest,
        StaticCondition::And {
            conditions: vec![
                make_quantity_ge(
                    QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Controller,
                        filter: Some(first_filter),
                    },
                    1,
                ),
                make_quantity_ge(
                    QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Controller,
                        filter: Some(second_filter),
                    },
                    1,
                ),
            ],
        },
    ))
}

fn parse_discarded_card_this_turn_after_actor(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) =
        alt((tag("discarded a card"), tag("discarded one or more cards"))).parse(input)?;
    let (rest, _) = tag(" this turn").parse(rest)?;
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::CardsDiscardedThisTurn {
                player: PlayerScope::Controller,
            },
            1,
        ),
    ))
}

fn parse_you_discarded_card_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you ").parse(input)?;
    parse_discarded_card_this_turn_after_actor(rest)
}

fn parse_you_created_token_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you created ").parse(input)?;
    let (rest, _) = alt((tag("a token"), tag("one or more tokens"))).parse(rest)?;
    let (rest, _) = tag(" this turn").parse(rest)?;
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::TokensCreatedThisTurn {
                player: PlayerScope::Controller,
                filter: TargetFilter::Any,
            },
            1,
        ),
    ))
}

fn parse_you_sacrificed_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you ").parse(input)?;
    parse_sacrificed_this_turn_after_actor(rest)
}

fn parse_sacrificed_this_turn_after_actor(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("sacrificed ").parse(input)?;
    parse_sacrificed_this_turn_tail(rest)
}

fn parse_sacrificed_this_turn_tail(input: &str) -> OracleResult<'_, StaticCondition> {
    if let Ok((rest, n)) = parse_ge_threshold(input) {
        let (rest, type_text) = take_until(" this turn").parse(rest)?;
        let (rest, _) = tag(" this turn").parse(rest)?;
        let (filter, leftover) = parse_type_phrase(type_text.trim());
        if leftover.trim().is_empty() && filter != TargetFilter::Any {
            return Ok((
                rest,
                make_quantity_ge(
                    QuantityRef::SacrificedThisTurn {
                        player: PlayerScope::Controller,
                        filter,
                    },
                    n,
                ),
            ));
        }
    }

    let (rest, _) = parse_article(input)?;
    let (rest, type_text) = take_until(" this turn").parse(rest)?;
    let (rest, _) = tag(" this turn").parse(rest)?;
    let (filter, leftover) = parse_type_phrase(type_text.trim());
    if !leftover.trim().is_empty() || filter == TargetFilter::Any {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::SacrificedThisTurn {
                player: PlayerScope::Controller,
                filter,
            },
            1,
        ),
    ))
}

fn parse_died_under_your_control_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = parse_article(input)?;
    let (rest, type_text) = take_until(" died under your control this turn").parse(rest)?;
    let (rest, _) = tag(" died under your control this turn").parse(rest)?;
    let (filter, leftover) = parse_type_phrase(type_text);
    if !leftover.trim().is_empty() || filter == TargetFilter::Any {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((
        rest,
        make_quantity_ge(
            QuantityRef::ZoneChangeCountThisTurn {
                from: Some(Zone::Battlefield),
                to: Some(Zone::Graveyard),
                filter: inject_controller_you(filter),
            },
            1,
        ),
    ))
}

fn parse_cast_spell_count_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("cast ").parse(input)?;
    parse_spell_count_this_turn(rest)
}

fn parse_spell_count_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, n) = parse_ge_threshold(input)?;
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("spells this turn").parse(rest) {
        return Ok((
            rest,
            make_quantity_ge(
                QuantityRef::SpellsCastThisTurn {
                    scope: CountScope::Controller,
                    filter: None,
                },
                n,
            ),
        ));
    }

    let (rest, type_text) = take_until(" spells this turn").parse(rest)?;
    let (rest, _) = tag(" spells this turn").parse(rest)?;
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
            n,
        ),
    ))
}

fn parse_opponent_cast_spell_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("an opponent has cast "), tag("an opponent cast "))).parse(input)?;
    if let Ok((rest, n)) = parse_ge_threshold(rest) {
        let (rest, _) = tag("spells this turn").parse(rest)?;
        return Ok((
            rest,
            make_quantity_ge(
                QuantityRef::SpellsCastThisTurn {
                    scope: CountScope::Opponents,
                    filter: None,
                },
                n,
            ),
        ));
    }
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
    let (rest, type_text) = take_until(" this turn").parse(input)?;
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
                scope: CountScope::Controller,
                filter: Some(filter),
            },
            minimum,
        ),
    ))
}

fn parse_spell_history_filter_with_optional_article(type_text: &str) -> Option<TargetFilter> {
    let trimmed = type_text.trim();
    let filter_text = parse_article(trimmed)
        .ok()
        .map_or(trimmed, |(rest, _)| rest.trim());
    parse_spell_history_filter(filter_text)
}

pub(crate) fn parse_spell_history_filter(type_text: &str) -> Option<TargetFilter> {
    if let Some(filter) = parse_spell_history_filter_with_zone_suffix(type_text) {
        return Some(filter);
    }
    let type_text = strip_spell_history_noun(type_text);
    if let Ok((rest, filter)) = value(
        TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::Historic])),
        tag::<_, _, OracleError<'_>>("historic"),
    )
    .parse(type_text)
    {
        if rest.trim().is_empty() {
            return Some(filter);
        }
    }
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

fn parse_spell_history_filter_with_zone_suffix(type_text: &str) -> Option<TargetFilter> {
    let (suffix, base_text) = take_until::<_, _, OracleError<'_>>(" from ")
        .parse(type_text)
        .ok()?;
    let (props, consumed) = parse_spell_history_origin_props(suffix)?;
    // CR 601.2a + CR 400.1: The cast-origin qualifier ("from anywhere other than
    // your hand") and the timing qualifier ("this turn") are independent axes and
    // may appear in either order. The `SpellsCastThisTurn` ref already encodes the
    // "this turn" window, so a trailing time qualifier after the cast-origin zone
    // suffix carries no extra filter information — accept and discard it. This is
    // the qualifier-then-time word order (Impending Flux: "spells you've cast from
    // anywhere other than your hand this turn") versus the time-then-qualifier
    // order ("spells you've cast this turn from anywhere other than your hand"),
    // which strips "this turn" before the cast-origin suffix ever reaches here.
    let remainder = suffix[consumed..].trim();
    let remainder = opt(tag::<_, _, OracleError<'_>>("this turn"))
        .parse(remainder)
        .map_or(remainder, |(rest, _)| rest);
    if !remainder.trim().is_empty() {
        return None;
    }

    let base_filter = parse_spell_history_base_filter(base_text.trim())?;
    Some(add_spell_history_filter_qualifiers(base_filter, props))
}

fn parse_spell_history_post_this_turn_origin(input: &str) -> (&str, Option<Vec<FilterProp>>) {
    parse_spell_history_origin_props(input).map_or((input, None), |(props, consumed)| {
        (&input[consumed..], Some(props))
    })
}

fn parse_spell_history_origin_props(input: &str) -> Option<(Vec<FilterProp>, usize)> {
    parse_cast_origin_anywhere_other_than_suffix(input).or_else(|| {
        let (props, _controller, consumed) = parse_zone_suffix(input)?;
        Some((props, consumed))
    })
}

fn parse_cast_origin_anywhere_other_than_suffix(input: &str) -> Option<(Vec<FilterProp>, usize)> {
    let trimmed = input.trim_start();
    let leading_ws = input.len() - trimmed.len();
    let (rest, _) = tag::<_, _, OracleError<'_>>("from anywhere other than ")
        .parse(trimmed)
        .ok()?;
    let (rest, _) = opt(alt((
        value((), tag::<_, _, OracleError<'_>>("your ")),
        value((), tag("their ")),
        value((), tag("his ")),
        value((), tag("her ")),
        value((), tag("its ")),
        value((), tag("a ")),
        value((), tag("the ")),
    )))
    .parse(rest)
    .ok()?;
    let (rest, zone) = parse_zone_word(rest).ok()?;
    let (rest, _) = peek_zone_boundary(rest).ok()?;

    let consumed = leading_ws + trimmed.len() - rest.len();
    Some((
        vec![FilterProp::InAnyZone {
            zones: cast_capable_zones_except(zone),
        }],
        consumed,
    ))
}

fn parse_spell_history_base_filter(type_text: &str) -> Option<TargetFilter> {
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("spell").parse(type_text) {
        if rest.trim().is_empty() {
            return Some(TargetFilter::Any);
        }
    }
    parse_spell_history_filter(type_text)
}

fn add_spell_history_filter_qualifiers(
    filter: TargetFilter,
    props: Vec<FilterProp>,
) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            for prop in props {
                if !typed.properties.contains(&prop) {
                    typed.properties.push(prop);
                }
            }
            TargetFilter::Typed(typed)
        }
        TargetFilter::Any => TargetFilter::Typed(TypedFilter::default().properties(props)),
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|inner| add_spell_history_filter_qualifiers(inner, props.clone()))
                .collect(),
        },
        other => other,
    }
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
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you haven't ").parse(input) {
        let (rest, _) = tag("cast ").parse(rest)?;
        let (rest, filter) = parse_one_spell_this_turn_filter(rest)?;
        return Ok((
            rest,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Controller,
                        filter,
                    },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            },
        ));
    }

    let (rest, _) = tag("you didn't ").parse(input)?;
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("cast ").parse(rest) {
        let (rest, filter) = parse_one_spell_this_turn_filter(rest)?;
        return Ok((
            rest,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Controller,
                        filter,
                    },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            },
        ));
    }

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
            make_quantity_comparison(
                QuantityRef::AttackedThisTurn {
                    scope: CountScope::Controller,
                    filter: None,
                },
                Comparator::EQ,
                0,
            ),
            tag("attack this turn"),
        ),
        // CR 606.1 + CR 603.4: "you didn't activate a loyalty ability of a
        // planeswalker this turn" — The Chain Veil's printed end-step penalty.
        value(
            make_quantity_comparison(
                QuantityRef::LoyaltyAbilitiesActivatedThisTurn {
                    player: PlayerScope::Controller,
                },
                Comparator::EQ,
                0,
            ),
            (
                tag("activate a loyalty ability"),
                opt(tag(" of a planeswalker")),
                tag(" this turn"),
            ),
        ),
    ))
    .parse(rest)
}

fn parse_source_didnt_this_turn(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("~ didn't "), tag("this creature didn't "))).parse(input)?;
    alt((
        value(
            make_source_history_absence(FilterProp::AttackedThisTurn),
            tag("attack this turn"),
        ),
        value(
            make_source_history_absence(FilterProp::EnteredThisTurn),
            tag("enter the battlefield this turn"),
        ),
    ))
    .parse(rest)
}

fn make_source_history_absence(prop: FilterProp) -> StaticCondition {
    StaticCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::And {
                    filters: vec![
                        TargetFilter::SelfRef,
                        TargetFilter::Typed(TypedFilter::default().properties(vec![prop])),
                    ],
                },
            },
        },
        comparator: Comparator::EQ,
        rhs: QuantityExpr::Fixed { value: 0 },
    }
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
    let had_enter_suffix = "enter the battlefield under your control this turn";

    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you had ").parse(input) {
        return parse_entered_this_turn_subject(rest, had_enter_suffix, 1);
    }

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

    // Branch 2: "a/an/another [type] entered..."
    parse_entered_this_turn_subject(input, entered_suffix, 1)
}

fn parse_entered_this_turn_subject<'a>(
    input: &'a str,
    suffix: &'static str,
    count: u32,
) -> OracleResult<'a, StaticCondition> {
    let (rest, type_text) = take_until(suffix).parse(input)?;
    let (rest, _) = tag(suffix).parse(rest)?;
    let type_text = type_text.trim();
    let _ = alt((parse_article, value((), tag("another ")))).parse(type_text)?;
    let (filter, _) = parse_type_phrase(type_text.trim());
    let filter = inject_controller_you(filter);
    Ok((
        rest,
        make_quantity_ge(QuantityRef::EnteredThisTurn { filter }, count),
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

fn parse_card_exiled_with_source_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("a card is "), tag("one or more cards are "))).parse(input)?;
    let (rest, _) = tag("exiled with ").parse(rest)?;
    let (rest, _) = alt((
        tag("~"),
        tag("it"),
        tag("this artifact"),
        tag("this creature"),
        tag("this land"),
        tag("this permanent"),
    ))
    .parse(rest)?;
    Ok((rest, make_quantity_ge(QuantityRef::CardsExiledBySource, 1)))
}

/// Parse "there is a/an X card and a/an Y card in your <zone>" as two
/// independent zone-count predicates sharing the same zone/scope suffix.
fn parse_there_exists_compound_zone_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("there's "), tag("there is "))).parse(input)?;
    let (rest, _) = parse_article(rest)?;
    let (rest, first_card_types) = parse_single_card_type_before_and(rest)?;
    let (rest, _) = tag(" and ").parse(rest)?;
    let (rest, _) = parse_article(rest)?;
    let (rest, second_card_types) = parse_single_card_type_before_zone(rest)?;
    let (rest, _) = tag(" in ").parse(rest)?;
    let (rest, (zone, scope)) = parse_scoped_zone_count_ref(rest)?;
    Ok((
        rest,
        StaticCondition::And {
            conditions: vec![
                make_quantity_ge(
                    QuantityRef::ZoneCardCount {
                        zone: zone.clone(),
                        card_types: first_card_types,
                        scope: scope.clone(),
                        filter: None,
                    },
                    1,
                ),
                make_quantity_ge(
                    QuantityRef::ZoneCardCount {
                        zone,
                        card_types: second_card_types,
                        scope,
                        filter: None,
                    },
                    1,
                ),
            ],
        },
    ))
}

fn parse_single_card_type_before_and(input: &str) -> OracleResult<'_, Vec<TypeFilter>> {
    let (rest, type_text) = take_until(" card and ").parse(input)?;
    let (rest, _) = tag(" card").parse(rest)?;
    Ok((rest, parse_zone_card_type_text(type_text)))
}

fn parse_single_card_type_before_zone(input: &str) -> OracleResult<'_, Vec<TypeFilter>> {
    let (rest, type_text) = take_until(" card in ").parse(input)?;
    let (rest, _) = tag(" card").parse(rest)?;
    Ok((rest, parse_zone_card_type_text(type_text)))
}

fn parse_zone_card_type_text(type_text: &str) -> Vec<TypeFilter> {
    let (filter, _) = parse_type_phrase(type_text.trim());
    let mut card_types = match filter {
        TargetFilter::Typed(TypedFilter { type_filters, .. }) => type_filters,
        _ => vec![],
    };
    card_types.retain(|type_filter| *type_filter != TypeFilter::Card);
    card_types
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
        QuantityRef::GraveyardSize {
            player: PlayerScope::Controller,
        }
    } else {
        QuantityRef::ZoneCardCount {
            zone,
            card_types: type_filters,
            filter: None,
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

/// Parse "that player controls more [type] than you" → QuantityComparison.
///
/// CR 603.2b + CR 603.4 + CR 102.1: Phase triggers such as Keeper of the Accord
/// ("At the beginning of each opponent's end step, if that player controls more
/// creatures than you, ...") compare the active player's battlefield to the
/// source controller's. `ControllerRef::ScopedPlayer` binds to the event player
/// at both detection and resolution (`resolve_quantity_for_trigger_check`).
fn parse_that_player_controls_more_comparison(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("that player controls more ").parse(input)?;
    let (rest, type_text) = take_until::<_, _, OracleError<'_>>(" than you").parse(rest)?;
    let (rest, _) = tag(" than you").parse(rest)?;

    let (filter, _) = parse_type_phrase(type_text.trim());
    let scoped_filter = match filter {
        TargetFilter::Typed(tf) => TargetFilter::Typed(tf.controller(ControllerRef::ScopedPlayer)),
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
                    filter: scoped_filter,
                },
            },
            comparator: Comparator::GT,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter: you_filter },
            },
        },
    ))
}

/// Parse "defending player controls more [type] than you" → QuantityComparison.
///
/// CR 508.1b + CR 603.4: Attack triggers can carry intervening-if clauses
/// comparing the defending player's permanents to the trigger controller's.
/// The object-count machinery already handles `ControllerRef::You`; this arm
/// adds the combat-context controller axis for the LHS.
/// CR 508.5 + CR 603.4: "that opponent has more life than another of your
/// opponents" — defending player's life exceeds the lowest life total among
/// the source controller's opponents (Breena, the Demagogue).
fn parse_defending_player_more_life_than_another_opponent(
    input: &str,
) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("that opponent "), tag("defending player "))).parse(input)?;
    let (rest, _) = tag("has more life than another of your opponents").parse(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::DefendingPlayer,
                },
            },
            comparator: Comparator::GT,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Min,
                    },
                },
            },
        },
    ))
}

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

/// Parse "no opponent has more life than that/defending player".
///
/// This is the negated form of the cross-player life comparison used on attack
/// triggers such as Guild Artisan. The referenced player is the defending
/// player from the attack event, so the condition composes as:
/// max(opponent life) <= defending-player life.
fn parse_no_opponent_comparison_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("no opponent ").parse(input)?;
    let (rest, _) = tag("has more life than ").parse(rest)?;
    let (rest, _) = alt((tag("that player"), tag("defending player"))).parse(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Max,
                    },
                },
            },
            comparator: Comparator::LE,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::DefendingPlayer,
                },
            },
        },
    ))
}

/// CR 506.2 + CR 508.6 + CR 603.4: Parse "that/that opponent player has another
/// opponent who isn't being attacked" (Suppressor Skyguard's attack-trigger
/// intervening-if). "That player" is the triggering/attacking player; the clause
/// is true when at least one of that player's opponents is NOT in their
/// attacked-this-combat set. Modeled as `PlayerCount(filter) >= 1` over the
/// `OpponentOfTriggeringPlayerNotAttacked` filter (resolved in `game/quantity.rs`).
///
/// The apostrophe in "isn't" is normalized over BOTH U+0027 (straight) and
/// U+2019 (curly) since `to_lowercase()` preserves the source printing's
/// apostrophe — Scryfall English oracle text uses the curly form.
fn parse_triggering_player_has_unattacked_opponent(
    input: &str,
) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = alt((tag("that player "), tag("that opponent "))).parse(input)?;
    let (rest, _) = tag("has another opponent who isn").parse(rest)?;
    let (rest, _) = alt((tag("'t being attacked"), tag("\u{2019}t being attacked"))).parse(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::OpponentOfTriggeringPlayerNotAttacked,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
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

    // CR 109.4 + CR 109.5: "an opponent controls at least N more [type] than you"
    // — existential over opponents (at least one opponent's count is >= yours + N;
    // "you" = the ability's controller). Isolated Watchtower ("at least two more
    // lands"). Must precede the generic `controls` + `parse_ge_threshold` arm
    // below, which would otherwise mis-read "at least two more lands" as
    // "at least two" + type phrase "more lands than you".
    if let Ok((rest2, condition)) = parse_opponent_controls_at_least_more_than_you(rest) {
        return Ok((rest2, condition));
    }

    // CR 109.3 + CR 603.4: "an opponent controls N or more [type]" /
    // "an opponent controls at least N [type]" → ObjectCount(filter w/
    // ControllerRef::Opponent) >= N. Shares `parse_ge_threshold` with the
    // `you control` arms so both idioms work uniformly. Defense of the Heart
    // ("if an opponent controls three or more creatures") is the canonical
    // card for this pattern.
    if let Ok((rest2, _)) = tag::<_, _, OracleError<'_>>("controls ").parse(rest) {
        if let Ok((rest3, n)) = parse_ge_threshold(rest2) {
            if tag::<_, _, OracleError<'_>>("more ")
                .parse(rest3.trim_start())
                .is_ok()
            {
                return Err(oracle_err(rest3));
            }
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

    // CR 109.4 + CR 109.5: "an opponent controls more [type] than you" —
    // existential over opponents (at least one opponent strictly exceeds your
    // count; "you" = the ability's controller), not an aggregate of all
    // opponent permanents. Weathered Wayfarer, Land Tax.
    if let Ok((rest2, condition)) = parse_opponent_controls_more_than_you(rest) {
        return Ok((rest2, condition));
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

    // "an opponent has at least N more life than you"
    if let Ok((rest2, _)) = tag::<_, _, OracleError<'_>>("has at least ").parse(rest) {
        let (rest2, n) = parse_number(rest2)?;
        let (rest2, _) = tag(" more life than you").parse(rest2)?;
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
                comparator: Comparator::GE,
                rhs: QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::LifeTotal {
                            player: PlayerScope::Controller,
                        },
                    }),
                    offset: n as i32,
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

    // CR 404 + CR 603.4: "an opponent has N or more cards in their graveyard"
    // → QuantityComparison(GraveyardSize[Opponent] >= N). Merfolk Windrobber's
    // activation restriction and See Double's "you may choose both instead"
    // both read this. The opponent graveyard is aggregated with `Max` so the
    // condition holds when ANY opponent meets the threshold (CR 102.2).
    //
    // CR 119 + CR 102.2: "an opponent has N or less life" / "an opponent has N
    // or more life" → `LifeTotal[Opponent { aggregate }] CMP N`. The aggregate
    // is coupled to the comparator (existential): LE/LT → `Min` so the
    // condition holds iff ANY opponent's life ≤ N; GE/GT → `Max` so it holds
    // iff ANY opponent's life ≥ N. Bloodghast's haste gate
    // ("as long as an opponent has 10 or less life") is the canonical card.
    if let Ok((rest2, _)) = tag::<_, _, OracleError<'_>>("has ").parse(rest) {
        if let Ok((rest3, n)) = parse_number(rest2) {
            if let Ok((rest4, _)) =
                tag::<_, _, OracleError<'_>>(" or more cards in their graveyard").parse(rest3)
            {
                return Ok((
                    rest4,
                    make_quantity_ge(
                        QuantityRef::GraveyardSize {
                            player: PlayerScope::Opponent {
                                aggregate: AggregateFunction::Max,
                            },
                        },
                        n,
                    ),
                ));
            }
            if let Ok((rest4, comparator)) = alt((
                value(
                    Comparator::LE,
                    tag::<_, _, OracleError<'_>>(" or less life"),
                ),
                value(
                    Comparator::GE,
                    tag::<_, _, OracleError<'_>>(" or more life"),
                ),
            ))
            .parse(rest3)
            {
                return Ok((
                    rest4,
                    make_quantity_comparison(
                        QuantityRef::LifeTotal {
                            player: PlayerScope::Opponent {
                                aggregate: existential_aggregate(comparator),
                            },
                        },
                        comparator,
                        n,
                    ),
                ));
            }
        }
    }

    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
}

fn parse_opponent_controls_at_least_more_than_you(
    input: &str,
) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("controls at least ").parse(input)?;
    let (rest, n) = parse_number(rest)?;
    let (rest, _) = tag(" more ").parse(rest)?;
    let (rest, type_text) = take_until::<_, _, OracleError<'_>>(" than you").parse(rest)?;
    let (rest, _) = tag(" than you").parse(rest)?;
    let (type_filter, you_filter) =
        player_count_comparison_filters(type_text).ok_or_else(|| oracle_err(type_text))?;

    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::ControlsCount {
                        relation: PlayerRelation::Opponent,
                        filter: type_filter,
                        comparator: Comparator::GE,
                        count: Box::new(QuantityExpr::Offset {
                            inner: Box::new(QuantityExpr::Ref {
                                qty: QuantityRef::ObjectCount { filter: you_filter },
                            }),
                            offset: n as i32,
                        }),
                    },
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        },
    ))
}

fn parse_opponent_controls_more_than_you(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("controls more ").parse(input)?;
    let (rest, type_text) = take_until::<_, _, OracleError<'_>>(" than you").parse(rest)?;
    let (rest, _) = tag(" than you").parse(rest)?;
    let (type_filter, you_filter) =
        player_count_comparison_filters(type_text).ok_or_else(|| oracle_err(type_text))?;

    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::ControlsCount {
                        relation: PlayerRelation::Opponent,
                        filter: type_filter,
                        comparator: Comparator::GT,
                        count: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount { filter: you_filter },
                        }),
                    },
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        },
    ))
}

fn player_count_comparison_filters(type_text: &str) -> Option<(TargetFilter, TargetFilter)> {
    let (type_filter, remainder) = parse_type_phrase(type_text.trim());
    if !remainder.trim().is_empty() || matches!(type_filter, TargetFilter::Any | TargetFilter::None)
    {
        return None;
    }

    let you_filter = inject_controller_you(type_filter.clone());
    Some((type_filter, you_filter))
}

/// CR 118.12a: Parse "[player] pays {cost}" → UnlessPay { cost }.
///
/// Handles "you pay {N}", "their controller pays {N}", "its controller pays {N}".
/// Used inside "unless" conditions for tax effects (Ghostly Prison, Propaganda, etc.).
fn parse_unless_pay_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    use crate::types::ability::UnlessPayScaling;

    // Consume the payer prefix (all variants lead to the same semantic: paying a cost).
    let (rest, _) = alt((
        tag("you pay "),
        tag("its controller pays "),
        tag("their controller pays "),
        tag("that player pays "),
        // CR 509.1c: block-tax payer on the defending player (Awesome Presence).
        tag("defending player pays "),
    ))
    .parse(input)?;
    let (rest, cost) = parse_mana_cost(rest)?;
    let (rest, scaling) = opt(alt((
        value(
            UnlessPayScaling::PerAffectedCreature,
            tag(" for each creature they control that's blocking it"),
        ),
        value(
            UnlessPayScaling::PerAffectedCreature,
            tag(" for each creature they control that is blocking it"),
        ),
    )))
    .parse(rest)?;
    Ok((
        rest,
        StaticCondition::UnlessPay {
            cost,
            scaling: scaling.unwrap_or_default(),
            // CR 506.3 + CR 508.1d: Generic "unless [player] pays" condition
            // outside the combat-tax dispatcher carries no defender scope —
            // dispatcher-specific paths (`parse_combat_tax_body`) populate it
            // when a "you" / "you or planeswalkers you control" tail is present.
            defended: None,
        },
    ))
}

/// Parse an "unless" condition, wrapping the inner condition in `Not`.
///
/// `active_static_definitions` treats a static's `condition` as "restriction
/// ACTIVE when TRUE", so "can't attack UNLESS X" must store `Not(X)`.
///
/// EXCEPTION — `StaticCondition::UnlessPay`: this condition is inherently
/// negative-polarity (`layers::evaluate_condition` returns `false` for it — the
/// restriction is active, the pay choice is taken at declaration). A condition
/// parsed from text that began with "unless" into `UnlessPay` is ALREADY
/// correctly polarized; wrapping it in `Not` would double-negate. `UnlessPay`
/// is the only inherently-negative condition `parse_inner_condition` can emit
/// today — if `parse_resolution_context_conditions` later gains another, this
/// `match` is the single place that must exclude it.
pub(crate) fn parse_unless_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, inner) = parse_inner_condition(input)?;
    let condition = match inner {
        // Already negative-polarity — leave raw, do not double-negate.
        unless_pay @ StaticCondition::UnlessPay { .. } => unless_pay,
        other => StaticCondition::Not {
            condition: Box::new(other),
        },
    };
    Ok((rest, condition))
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
    // CR 608.2c: A "this way" conditional may be quantified. "at least one" /
    // "one or more" both mean "≥ 1", which the existential `.any()` semantics
    // of `ZoneChangedThisWay` already encode — they value-discard to unit. The
    // bare article "a"/"an" is the singular existential form. All collapse to
    // the same existential condition.
    let (rest, _) = alt((
        value((), tag::<_, _, OracleError<'_>>("at least one ")),
        value((), tag("one or more ")),
        parse_article,
    ))
    .parse(input)?;

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

    // tense: singular "is"/"was" + plural "are"/"were". Verb number is
    // grammatically inert here — "one or more cards are milled" and "an X was
    // milled" produce the same existential condition. (Negations stay
    // singular-only: no card prints "aren't"/"weren't ... this way".)
    let (rest, negated) = alt((
        value(true, tag::<_, _, OracleError<'_>>("wasn't ")),
        value(true, tag("isn't ")),
        value(true, tag("was not ")),
        value(true, tag("is not ")),
        value(false, tag("was ")),
        value(false, tag("were ")),
        value(false, tag("is ")),
        value(false, tag("are ")),
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

/// CR 603.12 + CR 608.2c: Recognize a leading reflexive-conditional connector
/// and return the corresponding AbilityCondition with the connector consumed.
/// Single authority for this set; consumed by both
/// `oracle_effect::conditions::strip_if_you_do_conditional` and the
/// `oracle_effect::sequence` chunk-splitter sticky-detection so they never drift.
pub(crate) fn parse_reflexive_conditional_connector(
    input: &str,
) -> OracleResult<'_, AbilityCondition> {
    alt((
        value(AbilityCondition::WhenYouDo, tag("when you do, ")),
        value(
            AbilityCondition::effect_performed(),
            tag("if a player does, "),
        ),
        value(AbilityCondition::effect_performed(), tag("if they do, ")),
        value(
            AbilityCondition::effect_performed(),
            tag("if that player does, "),
        ),
        value(
            AbilityCondition::effect_performed(),
            tag("if the player does, "),
        ),
        value(
            AbilityCondition::Not {
                condition: Box::new(AbilityCondition::effect_performed()),
            },
            tag("if that player doesn't, "),
        ),
        value(
            AbilityCondition::Not {
                condition: Box::new(AbilityCondition::effect_performed()),
            },
            tag("if the player doesn't, "),
        ),
        value(
            AbilityCondition::Not {
                condition: Box::new(AbilityCondition::effect_performed()),
            },
            tag("if they don't, "),
        ),
        value(AbilityCondition::effect_performed(), tag("if you do, ")),
    ))
    .parse(input)
}

/// CR 603.12: Match "when you do" + optional trailing ", ". Used by the
/// triggered-modal splitter to detect a reflexive optional-cost header
/// ("When you do, choose ...") so it can divert that text into the reflexive
/// effect-chain path instead of attaching the modal directly to the trigger.
pub(crate) fn match_when_you_do(i: &str) -> OracleResult<'_, ()> {
    let (i, _) = tag("when you do").parse(i)?;
    let (i, _) = opt(tag(", ")).parse(i)?;
    Ok((i, ()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        CardTypeSetSource, CountScope, RoundingMode, TypeFilter, TypedFilter, ZoneRef,
    };
    use crate::types::card_type::Supertype;
    use crate::types::mana::{ManaColor, ManaCost};

    /// CR 506.2 + CR 508.6 + CR 603.4: Suppressor Skyguard's intervening-if
    /// "that player has another opponent who isn't being attacked" parses to a
    /// `PlayerCount(OpponentOfTriggeringPlayerNotAttacked) >= 1` comparison.
    /// Covers the straight (U+0027) apostrophe, the curly (U+2019) printing form,
    /// and the "that opponent" subject alias.
    #[test]
    fn parse_triggering_player_unattacked_opponent_variants() {
        let expected = StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::OpponentOfTriggeringPlayerNotAttacked,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        };
        for text in [
            "that player has another opponent who isn't being attacked",
            "that player has another opponent who isn\u{2019}t being attacked",
            "that opponent has another opponent who isn't being attacked",
        ] {
            let (rest, cond) = parse_inner_condition(text)
                .unwrap_or_else(|e| panic!("failed to parse {text:?}: {e:?}"));
            assert_eq!(rest, "", "unconsumed remainder for {text:?}");
            assert_eq!(cond, expected, "wrong condition for {text:?}");
        }
    }

    /// CR 108.3 + CR 109.4 + CR 603.4: "you control N or more permanents you
    /// don't own" — the bare negated-ownership suffix must be consumed by the
    /// type-phrase parser so the whole count condition is recognized (Agent of
    /// Treachery #3304). Before the fix "you don't own" was left unconsumed,
    /// leaving a non-empty remainder that aborted intervening-if hoisting.
    #[test]
    fn parse_control_count_ge_permanents_you_dont_own() {
        for text in [
            "you control three or more permanents you don't own",
            "you control three or more permanents you do not own",
        ] {
            let (rest, cond) = parse_control_count_ge(text)
                .unwrap_or_else(|e| panic!("failed to parse {text:?}: {e:?}"));
            assert_eq!(rest, "", "unconsumed remainder for {text:?}");
            let StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            } = cond
            else {
                panic!("expected ObjectCount >= 3 comparison for {text:?}, got {cond:?}");
            };
            let TargetFilter::Typed(tf) = filter else {
                panic!("expected Typed filter for {text:?}, got {filter:?}");
            };
            assert_eq!(
                tf.controller,
                Some(ControllerRef::You),
                "controller pinned to You via inject_controller_you for {text:?}"
            );
            assert!(
                tf.properties.contains(&FilterProp::Owned {
                    controller: ControllerRef::Opponent,
                }),
                "filter must carry Owned{{Opponent}} (\"you don't own it\") for {text:?}, \
                 got {:?}",
                tf.properties
            );
        }
    }

    #[test]
    fn parse_quantity_quantity_comparison_x_ge_library() {
        // CR 107.3 + CR 608.2c: Thassa's Oracle trailing intervening-if.
        // After forward-fill substitution this becomes Devotion >= ZoneCardCount{Library},
        // but pre-substitution the LHS is still Variable("X").
        let (rest, c) = parse_inner_condition(
            "x is greater than or equal to the number of cards in your library",
        )
        .unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Library,
                        card_types: Vec::new(),
                        scope: CountScope::Controller,
                        filter: None,
                    }
                },
            },
        );
    }

    /// CR 113.6c: "as long as ~ isn't on the battlefield" (Grist, the Hunger
    /// Tide's command-zone-as-creature static) — the negated copula must wrap
    /// the affirmative `SourceInZone { Battlefield }` reading in `Not`, marking
    /// that the ability functions everywhere EXCEPT the battlefield.
    #[test]
    fn parse_source_isnt_on_battlefield_wraps_in_not() {
        let (rest, c) = parse_condition("as long as ~ isn't on the battlefield").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::Not {
                condition: Box::new(StaticCondition::SourceInZone {
                    zone: crate::types::zones::Zone::Battlefield,
                }),
            },
        );
    }

    /// CR 113.6b: the affirmative copula still produces the bare
    /// `SourceInZone { Battlefield }` (no `Not` wrapper) — guards against the
    /// polarity alternation regressing the existing affirmative path.
    #[test]
    fn parse_source_is_on_battlefield_stays_affirmative() {
        let (rest, c) = parse_condition("as long as ~ is on the battlefield").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::SourceInZone {
                zone: crate::types::zones::Zone::Battlefield,
            },
        );
    }

    /// CR 113.6c: the "is not" spelling variant must also wrap in `Not` and must
    /// not be greedily split into " is " + "not on the battlefield".
    #[test]
    fn parse_source_is_not_on_battlefield_wraps_in_not() {
        let (rest, c) = parse_condition("as long as ~ is not on the battlefield").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::Not {
                condition: Box::new(StaticCondition::SourceInZone {
                    zone: crate::types::zones::Zone::Battlefield,
                }),
            },
        );
    }

    // --- parse_reflexive_conditional_connector (CR 603.12 / 608.2c) ---

    #[test]
    fn reflexive_connector_all_nine_variants() {
        let effect = AbilityCondition::effect_performed();
        let not_effect = AbilityCondition::Not {
            condition: Box::new(AbilityCondition::effect_performed()),
        };
        let cases: &[(&str, AbilityCondition)] = &[
            ("when you do, rest", AbilityCondition::WhenYouDo),
            ("if a player does, rest", effect.clone()),
            ("if they do, rest", effect.clone()),
            ("if that player does, rest", effect.clone()),
            ("if the player does, rest", effect.clone()),
            ("if that player doesn't, rest", not_effect.clone()),
            ("if the player doesn't, rest", not_effect.clone()),
            ("if they don't, rest", not_effect.clone()),
            ("if you do, rest", effect.clone()),
        ];
        for (input, expected) in cases {
            let (rest, cond) = parse_reflexive_conditional_connector(input)
                .unwrap_or_else(|_| panic!("connector must parse: {input:?}"));
            assert_eq!(&cond, expected, "condition mismatch for {input:?}");
            assert_eq!(rest, "rest", "remainder mismatch for {input:?}");
        }
    }

    #[test]
    fn reflexive_connector_rejects_non_reflexive_conditional() {
        assert!(parse_reflexive_conditional_connector("if you control a creature, ").is_err());
    }

    #[test]
    fn parse_quantity_quantity_comparison_x_lt_library() {
        // CR 107.3 + CR 608.2c: shape parity for the comparator dual. The new
        // arm only fires when LHS is the spell-bound variable X — broader LHS
        // forms (Devotion, HandSize) intentionally fall through to the legacy
        // static-condition fallbacks (see derived.rs DevotionGE scan).
        let (rest, c) =
            parse_inner_condition("x is less than the number of cards in your library").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                comparator: Comparator::LT,
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Library,
                        card_types: Vec::new(),
                        scope: CountScope::Controller,
                        filter: None,
                    }
                },
            },
        );
    }

    #[test]
    fn test_parse_condition_your_turn() {
        let (rest, c) = parse_condition("if it's your turn, do").unwrap();
        assert_eq!(rest, ", do");
        assert_eq!(c, StaticCondition::DuringYourTurn);
    }

    #[test]
    fn parse_inner_condition_put_fewer_than_n_onto_battlefield_this_way() {
        // CR 608.2c: Expand the Sphere's resolution-context comparison — gates
        // a follow-up effect on how many objects the preceding effect placed
        // onto the battlefield this resolution.
        let (rest, c) =
            parse_inner_condition("you put fewer than two lands onto the battlefield this way")
                .unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                },
                comparator: Comparator::LT,
                rhs: QuantityExpr::Fixed { value: 2 },
            },
        );
    }

    #[test]
    fn parse_inner_condition_this_enchantment_on_battlefield() {
        // SUB-FIX B: "this enchantment is on the battlefield" is a
        // self-referential zone check equivalent to "~ is on the battlefield".
        for subject in [
            "~",
            "this card",
            "this enchantment",
            "this permanent",
            "this creature",
            "this artifact",
        ] {
            let input = format!("{subject} is on the battlefield");
            let (rest, c) = parse_inner_condition(&input).unwrap();
            assert_eq!(rest, "", "subject={subject}");
            assert_eq!(
                c,
                StaticCondition::SourceInZone {
                    zone: crate::types::zones::Zone::Battlefield,
                },
                "subject={subject}",
            );
        }
    }

    #[test]
    fn test_parse_condition_as_long_as_tapped() {
        let (rest, c) = parse_condition("as long as ~ is tapped").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::SourceIsTapped));
    }

    // CR 702.171b: "as long as ~ is saddled" → SourceIsSaddled.
    #[test]
    fn test_parse_condition_as_long_as_saddled() {
        let (rest, c) = parse_condition("as long as ~ is saddled").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::SourceIsSaddled));
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

    /// CR 603.2b + CR 603.4 + CR 102.1: "if that player has no cards in hand" — the
    /// HandSize ref binds to the scoped player (active player for Phase triggers
    /// like Ghirapur Orrery), not the source's controller.
    #[test]
    fn test_parse_condition_that_player_no_cards() {
        let (rest, c) = parse_condition("if that player has no cards in hand").unwrap();
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
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::ScopedPlayer,
                        },
                    }
                );
                assert_eq!(comparator, Comparator::EQ);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 0 });
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_condition_that_player_n_or_more_cards() {
        let (rest, c) = parse_condition("if that player has three or more cards in hand").unwrap();
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
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::ScopedPlayer,
                        },
                    }
                );
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 3 });
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_condition_that_player_n_or_fewer_cards() {
        let (rest, c) = parse_condition("if that player has one or fewer cards in hand").unwrap();
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
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::ScopedPlayer,
                        },
                    }
                );
                assert_eq!(comparator, Comparator::LE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 1 });
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    /// CR 119 + CR 603.4 + CR 603.7c: "if that player has N or less life"
    /// intervening-if predicate on a combat-damage trigger. Canonical card:
    /// Ezio Auditore da Firenze. "That player" resolves to the damaged
    /// player (the event-context player), not the source's controller.
    #[test]
    fn test_parse_condition_that_player_n_or_less_life() {
        let (rest, c) = parse_condition("if that player has 10 or less life").unwrap();
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
                        qty: QuantityRef::LifeTotal {
                            player: PlayerScope::ScopedPlayer,
                        },
                    }
                );
                assert_eq!(comparator, Comparator::LE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 10 });
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    /// CR 119 + CR 603.4: sibling for the GE arm of the life-predicate
    /// combinator. Not yet printed on a known card with the "that player"
    /// subject, but kept to cover the full grammatical family alongside the
    /// hand-size N-or-more test.
    #[test]
    fn test_parse_condition_that_player_n_or_more_life() {
        let (rest, c) = parse_condition("if that player has 20 or more life").unwrap();
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
                        qty: QuantityRef::LifeTotal {
                            player: PlayerScope::ScopedPlayer,
                        },
                    }
                );
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 20 });
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    /// CR 402: "fewer than N cards in hand" — strict less-than hand-size gate.
    /// Used by Kozilek, the Great Distortion ("fewer than seven") and Iymrith,
    /// Desert Doom ("fewer than three") as the draw-difference condition.
    #[test]
    fn test_parse_condition_you_have_fewer_than_n_cards() {
        let (rest, c) = parse_condition("if you have fewer than seven cards in hand").unwrap();
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
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::Controller,
                        },
                    }
                );
                assert_eq!(comparator, Comparator::LT, "fewer than → strict LT");
                assert_eq!(rhs, QuantityExpr::Fixed { value: 7 });
            }
            other => panic!("expected HandSize LT 7, got {other:?}"),
        }
    }

    /// CR 402: "that player has more cards in hand than you" — cross-player GT
    /// comparison. Used by Slithermuse and Sandstone Oracle.
    #[test]
    fn test_parse_condition_that_player_more_cards_than_you() {
        let (rest, c) = parse_condition("if that player has more cards in hand than you").unwrap();
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
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::ScopedPlayer,
                        },
                    }
                );
                assert_eq!(comparator, Comparator::GT);
                assert_eq!(
                    rhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::Controller,
                        },
                    }
                );
            }
            other => {
                panic!("expected HandSize(ScopedPlayer) GT HandSize(Controller), got {other:?}")
            }
        }
    }

    /// CR 402: "target opponent has more cards in hand than you" — Target-scoped
    /// cross-player GT comparison. Used by Balance of Power.
    #[test]
    fn test_parse_condition_target_opponent_more_cards_than_you() {
        let (rest, c) =
            parse_condition("if target opponent has more cards in hand than you").unwrap();
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
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::Target,
                        },
                    }
                );
                assert_eq!(comparator, Comparator::GT);
                assert_eq!(
                    rhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::Controller,
                        },
                    }
                );
            }
            other => panic!("expected HandSize(Target) GT HandSize(Controller), got {other:?}"),
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

    #[test]
    fn parse_played_land_or_cast_spell_from_outside_hand_this_turn() {
        let (rest, condition) = parse_inner_condition(
            "you've played a land or cast a spell this turn from anywhere other than your hand",
        )
        .unwrap();
        assert_eq!(rest, "");

        let StaticCondition::Or { conditions } = condition else {
            panic!("expected Or condition, got {condition:?}");
        };
        assert_eq!(conditions.len(), 2);
        let StaticCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::LandsPlayedThisTurn {
                            player: PlayerScope::Controller,
                            from_zones: Some(land_zones),
                        },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        } = &conditions[0]
        else {
            panic!(
                "expected LandsPlayedThisTurn condition, got {:?}",
                conditions[0]
            );
        };
        assert!(!land_zones.contains(&Zone::Hand));
        assert!(land_zones.contains(&Zone::Exile));

        let StaticCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::SpellsCastThisTurn {
                            scope: CountScope::Controller,
                            filter: Some(TargetFilter::Typed(TypedFilter { properties, .. })),
                        },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        } = &conditions[1]
        else {
            panic!(
                "expected SpellsCastThisTurn condition, got {:?}",
                conditions[1]
            );
        };

        let zones = properties.iter().find_map(|prop| match prop {
            FilterProp::InAnyZone { zones } => Some(zones),
            _ => None,
        });
        let zones = zones.expect("expected InAnyZone origin qualifier");
        assert!(!zones.contains(&Zone::Hand));
        assert!(zones.contains(&Zone::Exile));
        assert!(zones.contains(&Zone::Graveyard));
    }

    #[test]
    fn parse_cast_spell_this_turn_from_zone_after_turn_phrase() {
        let (rest, condition) =
            parse_inner_condition("you've cast a creature spell this turn from your graveyard")
                .unwrap();
        assert_eq!(rest, "");

        let StaticCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::SpellsCastThisTurn {
                            filter: Some(TargetFilter::Typed(TypedFilter { properties, .. })),
                            ..
                        },
                },
            ..
        } = condition
        else {
            panic!("expected SpellsCastThisTurn condition, got {condition:?}");
        };
        assert!(properties.iter().any(|prop| prop
            == &FilterProp::InZone {
                zone: Zone::Graveyard
            }));
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

    /// The "Villain" creature subtype (Marvel set) must be recognized so that
    /// "you control a Villain" conditions parse — e.g. the conditional self
    /// cost-reduction on Visions of Villainy / Venom's Hunger.
    #[test]
    fn test_you_control_a_villain_subtype() {
        let (rest, c) = parse_inner_condition("you control a villain").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::IsPresent { filter: Some(_) }));
    }

    /// Recent Universe Beyond / Standard creature subtypes (Hero, Spy,
    /// Scientist, Cyborg, Sorcerer) must be recognized so their oracle-text
    /// references ("you control a <type>", typed tokens, etc.) parse.
    #[test]
    fn test_you_control_recent_subtypes() {
        for w in ["hero", "spy", "scientist", "cyborg", "sorcerer"] {
            let input = format!("you control a {w}");
            let (rest, c) = parse_inner_condition(&input)
                .unwrap_or_else(|_| panic!("'{w}' subtype should be recognized"));
            assert_eq!(rest, "", "leftover after parsing '{w}'");
            assert!(matches!(c, StaticCondition::IsPresent { filter: Some(_) }));
        }
    }

    /// Universe Beyond creature subtypes (Marvel: Gamma/Symbiote/Kree/Inhuman/
    /// Skrull; Transformers: Autobot; DC: Brainiac) must be recognized.
    #[test]
    fn test_you_control_universe_beyond_subtypes() {
        for w in [
            "gamma", "symbiote", "kree", "inhuman", "skrull", "autobot", "brainiac",
        ] {
            let input = format!("you control a {w}");
            let (rest, c) = parse_inner_condition(&input)
                .unwrap_or_else(|_| panic!("'{w}' subtype should be recognized"));
            assert_eq!(rest, "", "leftover after parsing '{w}'");
            assert!(matches!(c, StaticCondition::IsPresent { filter: Some(_) }));
        }
    }

    /// "Glimmer" (Duskmourn enchantment-creature subtype) and "Mammoth" must be
    /// recognized so their oracle-text references parse.
    #[test]
    fn test_you_control_glimmer_mammoth_subtypes() {
        for w in ["glimmer", "mammoth"] {
            let input = format!("you control a {w}");
            let (rest, c) = parse_inner_condition(&input)
                .unwrap_or_else(|_| panic!("'{w}' subtype should be recognized"));
            assert_eq!(rest, "", "leftover after parsing '{w}'");
            assert!(matches!(c, StaticCondition::IsPresent { filter: Some(_) }));
        }
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
    fn test_you_control_named_pair() {
        // CR 201.2 + CR 603.4: Scepter of Empires class — "you control [type]
        // named [Name1] and [Name2]" requires both named cards under your
        // control, lowered to And { IsPresent(Named X1), IsPresent(Named X2) }.
        let (rest, c) = parse_inner_condition(
            "you control artifacts named crown of empires and throne of empires",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::And { conditions } => {
                assert_eq!(conditions.len(), 2);
                let names: Vec<&str> = conditions
                    .iter()
                    .map(|cond| match cond {
                        StaticCondition::IsPresent {
                            filter: Some(TargetFilter::Typed(tf)),
                        } => tf.properties.iter().find_map(|p| match p {
                            FilterProp::Named { name } => Some(name.as_str()),
                            _ => None,
                        }),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()
                    .expect("both conjuncts must be IsPresent of typed Named filters");
                assert_eq!(names, vec!["crown of empires", "throne of empires"]);
                // Both conjuncts must constrain the type to Artifact and the
                // controller to You. Both must also include InZone(Battlefield).
                for cond in &conditions {
                    let StaticCondition::IsPresent {
                        filter: Some(TargetFilter::Typed(tf)),
                    } = cond
                    else {
                        panic!("expected typed IsPresent");
                    };
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                    assert!(tf.type_filters.contains(&TypeFilter::Artifact));
                    assert!(tf.properties.iter().any(
                        |p| matches!(p, FilterProp::InZone { zone } if *zone == Zone::Battlefield)
                    ));
                }
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

    /// CR 700.8c: "you have a full party" — party size is 4 (the cap).
    /// Exercises the shared `parse_inner_condition` entry point so every
    /// downstream consumer (static gates, trigger intervening-ifs via
    /// `static_condition_to_trigger_condition`, clause-level conditions)
    /// inherits the parse.
    #[test]
    fn test_full_party_condition() {
        let (rest, c) = parse_inner_condition("you have a full party").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::PartySize {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            }
        );

        // Composes with the "if " prefix path used by trigger and static
        // condition extraction.
        let (rest, c) = parse_condition("if you have a full party, ").unwrap();
        assert_eq!(rest, ", ");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::PartySize {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            }
        );
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
                        qty: QuantityRef::ObjectCountDistinct { filter, qualities },
                    } => {
                        assert_eq!(qualities, vec![SharedQuality::Name]);
                        match filter {
                            TargetFilter::Typed(t) => {
                                assert_eq!(t.controller, Some(ControllerRef::You));
                            }
                            _ => panic!("expected Typed filter, got {:?}", filter),
                        }
                    }
                    _ => panic!("expected ObjectCountDistinct, got {:?}", lhs),
                }
            }
            _ => panic!("expected QuantityComparison, got {:?}", c),
        }
    }

    #[test]
    fn test_you_control_n_or_more_with_different_powers() {
        let (rest, c) =
            parse_inner_condition("you control three or more creatures with different powers")
                .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCountDistinct { filter, qualities },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            } => {
                assert_eq!(qualities, vec![SharedQuality::Power]);
                assert!(matches!(filter, TargetFilter::Typed(_)));
            }
            other => panic!("expected ObjectCountDistinct Power GE 3, got {other:?}"),
        }
    }

    #[test]
    fn test_you_control_count_ge_toughness_greater_than_power() {
        let (rest, c) = parse_inner_condition(
            "you control three or more creatures that each have toughness greater than their power",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            } => match filter {
                TargetFilter::Typed(typed) => {
                    assert!(
                        typed
                            .properties
                            .iter()
                            .any(|prop| matches!(prop, FilterProp::ToughnessGTPower)),
                        "expected ToughnessGTPower property, got {:?}",
                        typed.properties
                    );
                }
                other => panic!("expected typed filter, got {other:?}"),
            },
            other => panic!("expected ObjectCount GE 3, got {other:?}"),
        }
    }

    #[test]
    fn test_you_control_count_ge_subtype_and_or_subtype() {
        let (rest, c) =
            parse_inner_condition("you control three or more wolves and/or werewolves").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            } => match filter {
                TargetFilter::Or { filters } => {
                    assert_eq!(filters.len(), 2);
                    assert!(filters.iter().all(|filter| matches!(
                        filter,
                        TargetFilter::Typed(TypedFilter {
                            controller: Some(ControllerRef::You),
                            ..
                        })
                    )));
                }
                other => panic!("expected subtype disjunction, got {other:?}"),
            },
            other => panic!("expected ObjectCount GE 3, got {other:?}"),
        }
    }

    #[test]
    fn test_you_control_exactly_one_creature() {
        let (rest, c) = parse_inner_condition("you control exactly one creature").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { .. },
                    },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {}
            other => panic!("expected ObjectCount EQ 1, got {other:?}"),
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
    fn test_control_count_ge_fixed_land_subtype() {
        let (rest, c) = parse_inner_condition("you control five or more towns").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ObjectCount {
                                filter: TargetFilter::Typed(typed),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 5 },
            } => {
                assert!(
                    typed
                        .type_filters
                        .contains(&TypeFilter::Subtype("Town".to_string())),
                    "expected Town subtype filter, got {:?}",
                    typed.type_filters
                );
                assert_eq!(typed.controller, Some(ControllerRef::You));
                assert!(typed.properties.contains(&FilterProp::InZone {
                    zone: Zone::Battlefield
                }));
            }
            other => panic!("expected ObjectCount Town GE 5, got {other:?}"),
        }
    }

    #[test]
    fn test_control_count_ge_creature_subtype() {
        let (rest, c) = parse_inner_condition("you control four or more wizards").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ObjectCount {
                                filter: TargetFilter::Typed(typed),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            } => {
                assert!(
                    typed
                        .type_filters
                        .contains(&TypeFilter::Subtype("Wizard".to_string())),
                    "expected Wizard subtype filter, got {:?}",
                    typed.type_filters
                );
                assert_eq!(typed.controller, Some(ControllerRef::You));
            }
            other => panic!("expected ObjectCount Wizard GE 4, got {other:?}"),
        }
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
                        qty:
                            QuantityRef::GraveyardSize {
                                player: PlayerScope::Controller,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 5 },
            } => {}
            other => panic!("expected GraveyardSize GE 5, got {other:?}"),
        }
    }

    #[test]
    fn test_typed_graveyard_creature_count_ge() {
        let (rest, c) =
            parse_inner_condition("you have four or more creature cards in your graveyard")
                .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ZoneCardCount {
                                zone: ZoneRef::Graveyard,
                                card_types,
                                scope: CountScope::Controller,
                                ..
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            } => {
                assert_eq!(card_types, vec![TypeFilter::Creature]);
            }
            other => panic!("expected ZoneCardCount Creature GE 4, got {other:?}"),
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

    /// CR 408 + CR 113.6b: A standalone "command zone" zone condition,
    /// covering Eminence ability-word lines whose static functions from the
    /// command zone.
    #[test]
    fn test_source_in_command_zone() {
        let (rest, c) = parse_inner_condition("~ is in the command zone").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::SourceInZone {
                zone: crate::types::zones::Zone::Command
            }
        ));
    }

    /// CR 113.6b + CR 408: "as long as ~ is in the command zone or on the
    /// battlefield" — the canonical Eminence shape. Must produce a typed
    /// disjunction of `SourceInZone { Command }` and `SourceInZone {
    /// Battlefield }` so `populate_active_zones_from_condition` seeds both
    /// zones into the static definition's `active_zones`.
    #[test]
    fn test_source_in_command_zone_or_battlefield() {
        let (rest, c) =
            parse_inner_condition("~ is in the command zone or on the battlefield").unwrap();
        assert_eq!(rest, "");
        let zones = match c {
            StaticCondition::Or { conditions } => conditions
                .into_iter()
                .filter_map(|c| match c {
                    StaticCondition::SourceInZone { zone } => Some(zone),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            other => panic!("expected Or {{ conditions }}, got {other:?}"),
        };
        assert_eq!(
            zones,
            vec![
                crate::types::zones::Zone::Command,
                crate::types::zones::Zone::Battlefield,
            ]
        );
    }

    /// Symmetric variant — "as long as ~ is on the battlefield or in the
    /// command zone" — must produce the same Or-disjunction (order matches
    /// the printed zone order).
    #[test]
    fn test_source_on_battlefield_or_in_command_zone() {
        let (rest, c) =
            parse_inner_condition("~ is on the battlefield or in the command zone").unwrap();
        assert_eq!(rest, "");
        let zones = match c {
            StaticCondition::Or { conditions } => conditions
                .into_iter()
                .filter_map(|c| match c {
                    StaticCondition::SourceInZone { zone } => Some(zone),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            other => panic!("expected Or {{ conditions }}, got {other:?}"),
        };
        assert_eq!(
            zones,
            vec![
                crate::types::zones::Zone::Battlefield,
                crate::types::zones::Zone::Command,
            ]
        );
    }

    /// CR 113.6b: Source-zone lists can contain three or more zones using
    /// comma and Oxford-comma separators, not just a two-zone "or" pair.
    /// This locks the reusable zone-list separator used by all
    /// source-referential zone conditions.
    #[test]
    fn test_source_in_three_zone_oxford_list() {
        let (rest, c) =
            parse_inner_condition("~ is in your graveyard, in your hand, or in exile").unwrap();
        assert_eq!(rest, "");
        let zones = match c {
            StaticCondition::Or { conditions } => conditions
                .into_iter()
                .filter_map(|c| match c {
                    StaticCondition::SourceInZone { zone } => Some(zone),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            other => panic!("expected Or {{ conditions }}, got {other:?}"),
        };
        assert_eq!(
            zones,
            vec![
                crate::types::zones::Zone::Graveyard,
                crate::types::zones::Zone::Hand,
                crate::types::zones::Zone::Exile,
            ]
        );
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

    // CR 120.3 + CR 702.11b: "it hasn't dealt damage yet" (Palladia-Mors,
    // Karakyk Guardian conditional hexproof) → Not(SourceHasDealtDamage),
    // fully consumed.
    #[test]
    fn test_it_hasnt_dealt_damage_yet() {
        let (rest, c) = parse_inner_condition("it hasn't dealt damage yet").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::Not {
                condition: Box::new(StaticCondition::SourceHasDealtDamage),
            }
        );
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

    #[test]
    fn test_a_card_is_exiled_with_source() {
        let (rest, c) = parse_inner_condition("a card is exiled with ~").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::CardsExiledBySource,
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {}
            other => panic!("expected CardsExiledBySource GE 1, got {other:?}"),
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

    /// CR 611.3a: An ATTACHED-subject combat phrase must NOT collapse to a
    /// `Source*` combat condition (an Equipment/Aura is never an attacker). The
    /// combat-state predicate now excludes the attached prefixes, so
    /// `parse_inner_condition` fails on these; the dedicated
    /// `parse_attached_subject_combat_state` combinator binds the state to the
    /// host recipient instead (see the inverted-grant path).
    #[test]
    fn test_equipped_creature_is_attacking_not_source_condition() {
        assert!(parse_inner_condition("equipped creature is attacking").is_err());
        let (rest, (filter, prop)) =
            parse_attached_subject_combat_state("equipped creature is attacking").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            filter,
            TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Creature).properties(vec![FilterProp::EquippedBy])
            )
        );
        assert_eq!(prop, FilterProp::Attacking { defender: None });
    }

    #[test]
    fn test_enchanted_creature_is_attacking_not_source_condition() {
        assert!(parse_inner_condition("enchanted creature is attacking").is_err());
        let (rest, (filter, prop)) =
            parse_attached_subject_combat_state("enchanted creature is attacking").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            filter,
            TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Creature).properties(vec![FilterProp::EnchantedBy])
            )
        );
        assert_eq!(prop, FilterProp::Attacking { defender: None });
    }

    #[test]
    fn test_source_enchanted_by_plural_aura_count() {
        let (rest, c) = parse_inner_condition("~ is enchanted by 3 or more Auras").unwrap();
        assert_eq!(rest, "");
        let StaticCondition::QuantityComparison {
            comparator, rhs, ..
        } = c
        else {
            panic!("expected QuantityComparison, got {c:?}");
        };
        assert_eq!(comparator, Comparator::GE);
        assert_eq!(rhs, QuantityExpr::Fixed { value: 3 });
    }

    #[test]
    fn test_source_enchanted_by_exactly_one_aura() {
        let (rest, c) = parse_inner_condition("~ is enchanted by exactly one Aura").unwrap();
        assert_eq!(rest, "");
        let StaticCondition::QuantityComparison {
            comparator, rhs, ..
        } = c
        else {
            panic!("expected QuantityComparison, got {c:?}");
        };
        assert_eq!(comparator, Comparator::EQ);
        assert_eq!(rhs, QuantityExpr::Fixed { value: 1 });
    }

    #[test]
    fn test_source_enchanted_by_exactly_two_auras() {
        let (rest, c) = parse_inner_condition("~ is enchanted by exactly two Auras").unwrap();
        assert_eq!(rest, "");
        let StaticCondition::QuantityComparison {
            comparator, rhs, ..
        } = c
        else {
            panic!("expected QuantityComparison, got {c:?}");
        };
        assert_eq!(comparator, Comparator::EQ);
        assert_eq!(rhs, QuantityExpr::Fixed { value: 2 });
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

        // CR 303.4 + CR 702.103: Springheart Nantuko's bestow landfall gate
        // — the optional " you control" suffix must be consumed and treated
        // the same as the bare form (the host of a bestow Aura is always under
        // its controller, so the controller axis adds no AST information; the
        // evaluator already binds the host's controller to the ability's
        // controller).
        let (rest, c) =
            parse_inner_condition("this permanent is attached to a creature you control").unwrap();
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

    #[test]
    fn test_entered_this_turn_another_subtype() {
        let (rest, c) = parse_inner_condition(
            "another knight entered the battlefield under your control this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::EnteredThisTurn {
                                filter: TargetFilter::Typed(filter),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {
                assert_eq!(filter.controller, Some(ControllerRef::You));
                assert!(filter
                    .type_filters
                    .contains(&TypeFilter::Subtype("Knight".to_string())));
                assert!(filter.properties.contains(&FilterProp::Another));
            }
            other => panic!("expected another Knight EnteredThisTurn GE 1, got {other:?}"),
        }
    }

    #[test]
    fn test_you_had_another_enter_this_turn() {
        let (rest, c) = parse_inner_condition(
            "you had another creature enter the battlefield under your control this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::EnteredThisTurn {
                                filter: TargetFilter::Typed(filter),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {
                assert_eq!(filter.controller, Some(ControllerRef::You));
                assert!(filter.type_filters.contains(&TypeFilter::Creature));
                assert!(filter.properties.contains(&FilterProp::Another));
            }
            other => panic!("expected another creature EnteredThisTurn GE 1, got {other:?}"),
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
                        qty:
                            QuantityRef::GraveyardSize {
                                player: PlayerScope::Controller,
                            },
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

    #[test]
    fn test_you_have_exactly_cards_in_hand() {
        for text in [
            "you have exactly thirteen cards in hand",
            "you have exactly thirteen cards in your hand",
        ] {
            let (rest, c) = parse_inner_condition(text).unwrap();
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
                    comparator: Comparator::EQ,
                    rhs: QuantityExpr::Fixed { value: 13 },
                } => {}
                other => panic!("expected HandSize EQ 13 for {text:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_you_have_exactly_life() {
        let (rest, c) = parse_inner_condition("you have exactly 13 life").unwrap();
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
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 13 },
            } => {}
            other => panic!("expected LifeTotal EQ 13, got {other:?}"),
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

    #[test]
    fn there_are_zone_threshold_stops_before_counter_effect_clause() {
        let (rest, c) = parse_inner_condition(
            "there are four or more card types among cards in your graveyard, put three +1/+1 counters on ~",
        )
        .unwrap();
        assert_eq!(rest, ", put three +1/+1 counters on ~");
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
                                filter: None,
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
                                filter: None,
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
                                    filter: None,
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
    fn test_there_exists_compound_card_types_in_graveyard() {
        let (rest, condition) =
            parse_inner_condition("there is an instant card and a sorcery card in your graveyard")
                .unwrap();
        assert_eq!(rest, "");
        let StaticCondition::And { conditions } = condition else {
            panic!("expected compound graveyard condition, got {condition:?}");
        };
        assert_eq!(conditions.len(), 2);
        assert_zone_card_count_condition(&conditions[0], TypeFilter::Instant);
        assert_zone_card_count_condition(&conditions[1], TypeFilter::Sorcery);
    }

    fn assert_zone_card_count_condition(condition: &StaticCondition, expected: TypeFilter) {
        let StaticCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::ZoneCardCount {
                            zone,
                            card_types,
                            scope,
                            filter: None,
                        },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        } = condition
        else {
            panic!("expected zone card count >= 1, got {condition:?}");
        };
        assert_eq!(*zone, ZoneRef::Graveyard);
        assert_eq!(*scope, CountScope::Controller);
        assert_eq!(card_types, &vec![expected]);
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

    fn typed_presence(condition: &StaticCondition) -> &TypedFilter {
        match condition {
            StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(tf)),
            } => tf,
            other => panic!("expected typed IsPresent, got {other:?}"),
        }
    }

    fn typed_presence_under_not(condition: &StaticCondition) -> &TypedFilter {
        match condition {
            StaticCondition::Not { condition } => typed_presence(condition),
            StaticCondition::And { conditions } if conditions.len() == 2 => {
                typed_presence_under_not(&conditions[1])
            }
            other => panic!("expected Not(IsPresent), got {other:?}"),
        }
    }

    fn assert_negated_attached_subject_exists(condition: &StaticCondition) {
        let StaticCondition::And { conditions } = condition else {
            panic!("expected And condition");
        };
        assert_eq!(conditions.len(), 2);
        let subject = typed_presence(&conditions[0]);
        assert!(
            subject.properties.contains(&FilterProp::EnchantedBy),
            "expected source-relative attached subject in {subject:?}"
        );
    }

    fn assert_has_color(tf: &TypedFilter, color: ManaColor) {
        assert!(
            tf.properties.iter().any(
                |prop| matches!(prop, FilterProp::HasColor { color: actual } if *actual == color)
            ),
            "expected {color:?} in {tf:?}"
        );
    }

    fn assert_attached_typed(
        tf: &TypedFilter,
        attachment_prop: FilterProp,
        type_filter: TypeFilter,
    ) {
        assert!(
            tf.properties.contains(&attachment_prop),
            "expected {attachment_prop:?} in {tf:?}"
        );
        assert!(
            tf.type_filters.contains(&type_filter),
            "expected {type_filter:?} in {tf:?}"
        );
    }

    #[test]
    fn test_attached_object_is_type_condition() {
        let (rest, c) = parse_inner_condition("enchanted permanent is a creature").unwrap();
        assert_eq!(rest, "");
        let tf = typed_presence(&c);
        assert_attached_typed(tf, FilterProp::EnchantedBy, TypeFilter::Permanent);
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
    }

    #[test]
    fn test_attached_object_is_color_condition() {
        let (rest, c) = parse_inner_condition("enchanted creature is red").unwrap();
        assert_eq!(rest, "");
        let tf = typed_presence(&c);
        assert_attached_typed(tf, FilterProp::EnchantedBy, TypeFilter::Creature);
        assert_has_color(tf, ManaColor::Red);
    }

    #[test]
    fn test_attached_object_is_not_type_condition() {
        let (rest, c) = parse_inner_condition("enchanted artifact isn't a creature").unwrap();
        assert_eq!(rest, "");
        assert_negated_attached_subject_exists(&c);
        let tf = typed_presence_under_not(&c);
        assert_attached_typed(tf, FilterProp::EnchantedBy, TypeFilter::Artifact);
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
    }

    #[test]
    fn test_attached_land_is_basic_mountain_condition() {
        let (rest, c) = parse_inner_condition("enchanted land is a basic Mountain").unwrap();
        assert_eq!(rest, "");
        let tf = typed_presence(&c);
        assert_attached_typed(tf, FilterProp::EnchantedBy, TypeFilter::Land);
        assert!(tf
            .type_filters
            .contains(&TypeFilter::Subtype("Mountain".to_string())));
        assert!(tf.properties.iter().any(
            |prop| matches!(prop, FilterProp::HasSupertype { value } if *value == Supertype::Basic)
        ));
    }

    #[test]
    fn test_attached_creature_is_not_legendary_condition() {
        let (rest, c) = parse_inner_condition("enchanted creature isn't legendary").unwrap();
        assert_eq!(rest, "");
        assert_negated_attached_subject_exists(&c);
        let tf = typed_presence_under_not(&c);
        assert_attached_typed(tf, FilterProp::EnchantedBy, TypeFilter::Creature);
        assert!(tf.properties.iter().any(
            |prop| matches!(prop, FilterProp::HasSupertype { value } if *value == Supertype::Legendary)
        ));
    }

    #[test]
    fn test_attached_object_color_disjunction_condition() {
        let (rest, c) = parse_inner_condition("enchanted permanent is red or green").unwrap();
        assert_eq!(rest, "");
        let StaticCondition::Or { conditions } = c else {
            panic!("expected Or condition");
        };
        assert_eq!(conditions.len(), 2);
        let first = typed_presence(&conditions[0]);
        assert_attached_typed(first, FilterProp::EnchantedBy, TypeFilter::Permanent);
        assert_has_color(first, ManaColor::Red);
        let second = typed_presence(&conditions[1]);
        assert_attached_typed(second, FilterProp::EnchantedBy, TypeFilter::Permanent);
        assert_has_color(second, ManaColor::Green);
    }

    #[test]
    fn test_equipped_creature_type_disjunction_condition() {
        let (rest, c) = parse_inner_condition("equipped creature is a Human or an Angel").unwrap();
        assert_eq!(rest, "");
        let StaticCondition::Or { conditions } = c else {
            panic!("expected Or condition");
        };
        let human = typed_presence(&conditions[0]);
        assert_attached_typed(human, FilterProp::EquippedBy, TypeFilter::Creature);
        assert!(human
            .type_filters
            .contains(&TypeFilter::Subtype("Human".to_string())));
        let angel = typed_presence(&conditions[1]);
        assert_attached_typed(angel, FilterProp::EquippedBy, TypeFilter::Creature);
        assert!(angel
            .type_filters
            .contains(&TypeFilter::Subtype("Angel".to_string())));
    }

    // -- Anaphoric "it" recipient conditions (CR 611.3a) --

    fn recipient_filter(condition: &StaticCondition) -> &TargetFilter {
        match condition {
            StaticCondition::RecipientMatchesFilter { filter } => filter,
            other => panic!("expected RecipientMatchesFilter, got {other:?}"),
        }
    }

    fn assert_no_attachment_or_presence(condition: &StaticCondition) {
        // The recipient is by definition the modified object — it must never be
        // expressed via an attachment prop, an existence guard, or a source filter.
        let json = format!("{condition:?}");
        for forbidden in [
            "EnchantedBy",
            "EquippedBy",
            "IsPresent",
            "SourceMatchesFilter",
        ] {
            // allow-noncombinator: test assertion scanning Debug output, not parser dispatch.
            assert!(
                !json.contains(forbidden),
                "recipient condition leaked {forbidden}: {json}"
            );
        }
    }

    #[test]
    fn test_recipient_is_subtype_apostrophe_s() {
        let (rest, c) = parse_inner_condition("it's a Zombie").unwrap();
        assert_eq!(rest, "");
        let tf = recipient_filter(&c);
        let TargetFilter::Typed(tf) = tf else {
            panic!("expected Typed");
        };
        // A bare subtype phrase ("a Zombie") yields Subtype only — no implicit
        // Creature core type. At runtime `matches_target_filter` checks the
        // recipient's subtype, which a Zombie object carries.
        assert!(tf
            .type_filters
            .contains(&TypeFilter::Subtype("Zombie".to_string())));
        assert_no_attachment_or_presence(&c);
    }

    #[test]
    fn test_recipient_is_subtype_is_form() {
        let (rest, c) = parse_inner_condition("it is a Zombie").unwrap();
        assert_eq!(rest, "");
        let TargetFilter::Typed(tf) = recipient_filter(&c) else {
            panic!("expected Typed");
        };
        assert!(tf
            .type_filters
            .contains(&TypeFilter::Subtype("Zombie".to_string())));
        assert_no_attachment_or_presence(&c);
    }

    #[test]
    fn test_recipient_is_color() {
        let (rest, c) = parse_inner_condition("it's white").unwrap();
        assert_eq!(rest, "");
        let TargetFilter::Typed(tf) = recipient_filter(&c) else {
            panic!("expected Typed");
        };
        assert_has_color(tf, ManaColor::White);
        assert!(tf.type_filters.is_empty(), "bare color has no type: {tf:?}");
        assert_no_attachment_or_presence(&c);
    }

    #[test]
    fn test_recipient_isnt_subtype() {
        let (rest, c) = parse_inner_condition("it isn't a Zombie").unwrap();
        assert_eq!(rest, "");
        let StaticCondition::Not { condition } = &c else {
            panic!("expected Not, got {c:?}");
        };
        let TargetFilter::Typed(tf) = recipient_filter(condition) else {
            panic!("expected Typed");
        };
        assert!(tf
            .type_filters
            .contains(&TypeFilter::Subtype("Zombie".to_string())));
        // No IsPresent existence guard wrapping (recipient is the modified object).
        assert_no_attachment_or_presence(&c);
    }

    #[test]
    fn test_recipient_is_creature() {
        let (rest, c) = parse_inner_condition("it's a creature").unwrap();
        assert_eq!(rest, "");
        let TargetFilter::Typed(tf) = recipient_filter(&c) else {
            panic!("expected Typed");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert_no_attachment_or_presence(&c);
    }

    #[test]
    fn test_recipient_is_wall_keeps_subtype() {
        let (rest, c) = parse_inner_condition("it's a Wall").unwrap();
        assert_eq!(rest, "");
        let TargetFilter::Typed(tf) = recipient_filter(&c) else {
            panic!("expected Typed");
        };
        // "a Wall" is a bare subtype phrase → Subtype only (no implicit Creature).
        assert!(tf
            .type_filters
            .contains(&TypeFilter::Subtype("Wall".to_string())));
        assert_no_attachment_or_presence(&c);
    }

    #[test]
    fn test_recipient_is_subtype_disjunction() {
        let (rest, c) = parse_inner_condition("it's a Zombie or a Skeleton").unwrap();
        assert_eq!(rest, "");
        let StaticCondition::Or { conditions } = &c else {
            panic!("expected Or, got {c:?}");
        };
        assert_eq!(conditions.len(), 2);
        let TargetFilter::Typed(zombie) = recipient_filter(&conditions[0]) else {
            panic!("expected Typed");
        };
        assert!(zombie
            .type_filters
            .contains(&TypeFilter::Subtype("Zombie".to_string())));
        let TargetFilter::Typed(skeleton) = recipient_filter(&conditions[1]) else {
            panic!("expected Typed");
        };
        assert!(skeleton
            .type_filters
            .contains(&TypeFilter::Subtype("Skeleton".to_string())));
        assert_no_attachment_or_presence(&c);
    }

    #[test]
    fn test_recipient_as_long_as_prefix_stripped() {
        // CR 611.3a: "as long as it's a Zombie" yields the same body after the
        // duration prefix is consumed by parse_condition.
        let (rest, c) = parse_condition("as long as it's a Zombie").unwrap();
        assert_eq!(rest, "");
        let TargetFilter::Typed(tf) = recipient_filter(&c) else {
            panic!("expected Typed");
        };
        assert!(tf
            .type_filters
            .contains(&TypeFilter::Subtype("Zombie".to_string())));
    }

    #[test]
    fn test_recipient_guard_backtracks_to_attacking_alone() {
        // GUARD: "it's attacking alone" leaves "alone" after parse_type_phrase
        // matches "attacking " — the terminal-boundary guard rejects the pronoun
        // match so the combat combinator wins.
        let (rest, c) = parse_inner_condition("it's attacking alone").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::SourceAttackingAlone);
    }

    #[test]
    fn test_recipient_its_your_turn_not_captured() {
        // "it's your turn" is owned by the turn combinator, tried before this one.
        let (rest, c) = parse_inner_condition("it's your turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::DuringYourTurn);
    }

    #[test]
    fn test_recipient_its_night_not_captured() {
        let (rest, c) = parse_inner_condition("it's night").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::DayNightIs {
                state: DayNight::Night,
            }
        );
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
    fn test_you_have_the_initiative() {
        let (rest, c) = parse_inner_condition("you have the initiative").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::IsInitiative);
    }

    #[test]
    fn test_there_is_no_monarch() {
        let (rest, c) = parse_inner_condition("there is no monarch").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::NoMonarch);
    }

    #[test]
    fn test_theres_no_monarch() {
        let (rest, c) = parse_inner_condition("there's no monarch").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::NoMonarch);
    }

    #[test]
    fn test_city_blessing() {
        let (rest, c) = parse_inner_condition("you have the city's blessing").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::HasCityBlessing);
    }

    #[test]
    fn test_was_starting_player() {
        // CR 103.1: affirmative form.
        let (rest, c) = parse_inner_condition("you were the starting player").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::WasStartingPlayer {
                controller: ControllerRef::You,
            }
        );
    }

    #[test]
    fn test_wasnt_starting_player() {
        // CR 103.1: negated form (Radiant Smite, Cindercone Smite, Sylvan Smite).
        let (rest, c) = parse_inner_condition("you weren't the starting player").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::Not {
                condition: Box::new(StaticCondition::WasStartingPlayer {
                    controller: ControllerRef::You,
                }),
            }
        );
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
                                        exclude: None,
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
                            exclude: None,
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

    /// CR 119: "you have at least N life more than your starting life total"
    /// (Angel of Destiny class) — reuses the `LifeAboveStarting` building block
    /// (current life − starting life total), so the canonical shape is
    /// `LifeAboveStarting GE Fixed(N)`. Both "at least N" and "N or more"
    /// wordings resolve identically. This is the same shape the static
    /// "as long as …" gate produces (see oracle_static `shared.rs`).
    #[test]
    fn test_you_have_life_more_than_starting_life_total() {
        for text in [
            "you have at least 15 life more than your starting life total",
            "you have 15 or more life more than your starting life total",
        ] {
            let (rest, c) = parse_inner_condition(text).unwrap();
            assert_eq!(rest, "", "must fully consume {text:?}");
            assert_eq!(
                c,
                StaticCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::LifeAboveStarting,
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 15 },
                },
                "expected LifeAboveStarting GE Fixed(15) for {text:?}",
            );
        }
    }

    /// Regression guard: the new life-offset branch must NOT steal the plain
    /// "you have N or more life" condition (no "more than" suffix) — it falls
    /// through to the bare LifeTotal-GE comparison.
    #[test]
    fn test_you_have_or_more_life_still_parses_without_offset() {
        let (rest, c) = parse_inner_condition("you have 5 or more life").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 5 },
            },
        );
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
                        qty:
                            QuantityRef::PlayerCount {
                                filter:
                                    PlayerFilter::ControlsCount {
                                        relation: PlayerRelation::Opponent,
                                        comparator: Comparator::GT,
                                        ..
                                    },
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {}
            other => panic!("expected existential opponent ControlsCount GE 1, got {other:?}"),
        }
    }

    /// Issue #859: Weathered Wayfarer — "Activate only if an opponent controls
    /// more lands than you."
    #[test]
    fn test_opponent_controls_more_lands_than_you() {
        let (rest, c) = parse_inner_condition("an opponent controls more lands than you").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::PlayerCount {
                                filter:
                                    PlayerFilter::ControlsCount {
                                        relation: PlayerRelation::Opponent,
                                        filter: TargetFilter::Typed(tf),
                                        comparator: Comparator::GT,
                                        ..
                                    },
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Land]);
            }
            other => panic!("expected existential opponent land count GE 1, got {other:?}"),
        }
    }

    /// Issue #2908 / Isolated Watchtower — "an opponent controls at least N more
    /// [type] than you" uses GE with an Offset threshold, not bare GT.
    #[test]
    fn test_opponent_controls_at_least_n_more_lands_than_you() {
        let (rest, c) =
            parse_inner_condition("an opponent controls at least two more lands than you").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::PlayerCount {
                                filter:
                                    PlayerFilter::ControlsCount {
                                        relation: PlayerRelation::Opponent,
                                        filter: TargetFilter::Typed(tf),
                                        comparator: Comparator::GE,
                                        count,
                                    },
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Land]);
                match count.as_ref() {
                    QuantityExpr::Offset { inner, offset: 2 } => match inner.as_ref() {
                        QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount { filter },
                        } => {
                            assert!(matches!(
                                filter,
                                TargetFilter::Typed(TypedFilter {
                                    controller: Some(ControllerRef::You),
                                    ..
                                })
                            ));
                        }
                        other => panic!("expected ObjectCount inner, got {other:?}"),
                    },
                    other => panic!("expected Offset(+2) threshold, got {other:?}"),
                }
            }
            other => panic!("expected existential opponent land count GE (you+2), got {other:?}"),
        }
    }

    #[test]
    fn test_opponent_controls_more_rejects_unknown_type_phrase() {
        assert!(parse_inner_condition("an opponent controls more widgets than you").is_err());
        assert!(
            parse_inner_condition("an opponent controls at least two more widgets than you")
                .is_err()
        );
    }

    /// CR 603.2b + CR 603.4: Keeper of the Accord — "that player" is the active
    /// player whose phase is beginning, not a generic opponent aggregate.
    #[test]
    fn test_that_player_controls_more_creatures_than_you() {
        let (rest, c) =
            parse_inner_condition("that player controls more creatures than you").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ObjectCount {
                                filter: TargetFilter::Typed(lhs),
                            },
                    },
                comparator: Comparator::GT,
                rhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ObjectCount {
                                filter: TargetFilter::Typed(rhs),
                            },
                    },
            } => {
                assert_eq!(lhs.controller, Some(ControllerRef::ScopedPlayer));
                assert_eq!(rhs.controller, Some(ControllerRef::You));
            }
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
    fn test_opponent_has_n_cards_in_graveyard() {
        // CR 404 + CR 603.4: Merfolk Windrobber / See Double intervening-if.
        let (rest, c) =
            parse_inner_condition("an opponent has eight or more cards in their graveyard")
                .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::GraveyardSize {
                                player:
                                    PlayerScope::Opponent {
                                        aggregate: AggregateFunction::Max,
                                    },
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 8 },
            } => {}
            other => panic!("expected opponent GraveyardSize GE 8, got {other:?}"),
        }
    }

    /// CR 119 + CR 102.2: #659 Bloodghast — "an opponent has 10 or less life"
    /// must lower to `LifeTotal[Opponent { Min }] LE 10`. `Min` is the
    /// existential aggregate for LE: ANY opponent at ≤10 satisfies the
    /// condition. Covers Bloodghast's haste gate plus the class of
    /// opponent-life-threshold static abilities.
    #[test]
    fn test_opponent_has_n_or_less_life() {
        let (rest, c) = parse_inner_condition("an opponent has 10 or less life").unwrap();
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
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 10 },
            } => {}
            other => panic!("expected opponent LifeTotal LE 10 with Min aggregate, got {other:?}"),
        }
    }

    /// Symmetric mirror of the LE variant: "an opponent has N or more life"
    /// must aggregate with `Max` so the condition holds when ANY opponent's
    /// life ≥ N. Same combinator branch as the LE case.
    #[test]
    fn test_opponent_has_n_or_more_life() {
        let (rest, c) = parse_inner_condition("an opponent has 20 or more life").unwrap();
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
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 20 },
            } => {}
            other => panic!("expected opponent LifeTotal GE 20 with Max aggregate, got {other:?}"),
        }
    }

    #[test]
    fn test_that_opponent_has_more_life_than_another_opponent() {
        let (rest, c) =
            parse_inner_condition("that opponent has more life than another of your opponents")
                .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::LifeTotal {
                                player: PlayerScope::DefendingPlayer,
                            },
                    },
                comparator: Comparator::GT,
                rhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::LifeTotal {
                                player:
                                    PlayerScope::Opponent {
                                        aggregate: AggregateFunction::Min,
                                    },
                            },
                    },
            } => {}
            other => panic!("expected defending life GT min opponent life, got {other:?}"),
        }
    }

    #[test]
    fn test_no_opponent_has_more_life_than_that_player() {
        let (rest, c) =
            parse_inner_condition("no opponent has more life than that player").unwrap();
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
                comparator: Comparator::LE,
                rhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::LifeTotal {
                                player: PlayerScope::DefendingPlayer,
                            },
                    },
            } => {}
            other => {
                panic!("expected OpponentLifeTotal LE DefendingPlayerLifeTotal, got {other:?}")
            }
        }
    }

    #[test]
    fn test_opponent_has_at_least_n_more_life() {
        let (rest, c) =
            parse_inner_condition("an opponent has at least 5 more life than you").unwrap();
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
                comparator: Comparator::GE,
                rhs: QuantityExpr::Offset { inner, offset: 5 },
            } => match inner.as_ref() {
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::LifeTotal {
                            player: PlayerScope::Controller,
                        },
                } => {}
                other => panic!("expected controller life total offset base, got {other:?}"),
            },
            other => panic!("expected OpponentLifeTotal GE LifeTotal+5, got {other:?}"),
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
            StaticCondition::UnlessPay { cost, scaling, .. } => {
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
        // "unless X" normally wraps inner in Not — but `UnlessPay` is already
        // inherently negative-polarity, so it must pass through RAW (wrapping
        // would double-negate). See `parse_unless_condition`.
        assert!(
            matches!(c, StaticCondition::UnlessPay { .. }),
            "expected raw UnlessPay (not Not-wrapped), got {c:?}"
        );
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

    /// CR 208.1: The `~ has power N or greater` form is the canonical
    /// templating used by intervening-if continuations such as
    /// "Then if ~ has power 7 or greater, …" (Cloud, Ex-SOLDIER). Without
    /// this combinator the clause is dropped and the gated sub-ability fires
    /// unconditionally.
    #[test]
    fn test_self_ref_has_power_ge() {
        let (rest, c) = parse_inner_condition("~ has power 7 or greater").unwrap();
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
                rhs: QuantityExpr::Fixed { value: 7 },
            } => {}
            other => panic!("expected SelfPower GE 7, got {other:?}"),
        }
    }

    /// Level Up: granted attack trigger uses the pronoun "it" in the draw gate.
    #[test]
    fn test_it_has_power_ge() {
        let (rest, c) = parse_inner_condition("it has power 10 or greater").unwrap();
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
                rhs: QuantityExpr::Fixed { value: 10 },
            } => {}
            other => panic!("expected SelfPower GE 10, got {other:?}"),
        }
    }

    #[test]
    fn test_this_creature_has_toughness_le() {
        let (rest, c) = parse_inner_condition("this creature has toughness 2 or less").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::Toughness {
                                scope: crate::types::ability::ObjectScope::Source,
                            },
                    },
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 2 },
            } => {}
            other => panic!("expected SelfToughness LE 2, got {other:?}"),
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
    fn filtered_spell_count_this_turn_counts_controller_spells() {
        let (rest, c) = parse_inner_condition(
            "you've cast three or more instant and/or sorcery spells this turn",
        )
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
                rhs: QuantityExpr::Fixed { value: 3 },
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
            other => panic!("expected filtered SpellsCastThisTurn GE 3, got {other:?}"),
        }
    }

    #[test]
    fn youve_cast_historic_spell_this_turn_counts_controller_spells() {
        let (rest, c) = parse_inner_condition("you've cast a historic spell this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::SpellsCastThisTurn {
                                scope: CountScope::Controller,
                                filter: Some(TargetFilter::Typed(TypedFilter { properties, .. })),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => assert!(properties.contains(&FilterProp::Historic)),
            other => panic!("expected historic SpellsCastThisTurn GE 1, got {other:?}"),
        }
    }

    #[test]
    fn youve_cast_spell_with_mana_value_this_turn_counts_controller_spells() {
        let (rest, c) =
            parse_inner_condition("you've cast a spell with mana value 4 or greater this turn")
                .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::SpellsCastThisTurn {
                                scope: CountScope::Controller,
                                filter: Some(TargetFilter::Typed(TypedFilter { properties, .. })),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => assert!(properties.contains(&FilterProp::Cmc {
                comparator: Comparator::GE,
                value: QuantityExpr::Fixed { value: 4 },
            })),
            other => panic!("expected mana-value filtered SpellsCastThisTurn, got {other:?}"),
        }
    }

    #[test]
    fn youve_cast_both_creature_and_noncreature_spells_this_turn_is_compound() {
        let (rest, c) = parse_inner_condition(
            "you've cast both a creature spell and a noncreature spell this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::And { conditions } => {
                assert_eq!(conditions.len(), 2);
                assert!(conditions.iter().any(|condition| matches!(
                    condition,
                    StaticCondition::QuantityComparison {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::SpellsCastThisTurn {
                                scope: CountScope::Controller,
                                filter: Some(TargetFilter::Typed(TypedFilter { type_filters, .. })),
                            },
                        },
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: 1 },
                    } if type_filters == &vec![TypeFilter::Creature]
                )));
                assert!(conditions.iter().any(|condition| matches!(
                    condition,
                    StaticCondition::QuantityComparison {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::SpellsCastThisTurn {
                                scope: CountScope::Controller,
                                filter: Some(TargetFilter::Typed(TypedFilter { type_filters, .. })),
                            },
                        },
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: 1 },
                    } if type_filters == &vec![TypeFilter::Non(Box::new(TypeFilter::Creature))]
                )));
            }
            other => panic!("expected compound SpellsCastThisTurn condition, got {other:?}"),
        }
    }

    #[test]
    fn you_cast_both_creature_and_noncreature_spells_this_turn_is_compound() {
        let (rest, c) = parse_inner_condition(
            "you cast both a creature spell and a noncreature spell this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::And { conditions } if conditions.len() == 2));
    }

    #[test]
    fn you_havent_cast_spell_this_turn_counts_zero_controller_spells() {
        let (rest, c) = parse_inner_condition("you haven't cast a spell this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Controller,
                        filter: None,
                    },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            }
        );
    }

    #[test]
    fn you_havent_cast_spell_from_hand_this_turn_keeps_origin_filter() {
        let (rest, c) =
            parse_inner_condition("you haven't cast a spell from your hand this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::SpellsCastThisTurn {
                                scope: CountScope::Controller,
                                filter: Some(TargetFilter::Typed(TypedFilter { properties, .. })),
                            },
                    },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            } => assert!(properties.contains(&FilterProp::InZone { zone: Zone::Hand })),
            other => panic!("expected origin-filtered zero spell count, got {other:?}"),
        }
    }

    #[test]
    fn sacrificed_artifact_this_turn_counts_controller_sacrifices() {
        let (rest, c) =
            parse_condition("as long as you've sacrificed an artifact this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::SacrificedThisTurn {
                                player: PlayerScope::Controller,
                                filter: TargetFilter::Typed(TypedFilter { type_filters, .. }),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => assert_eq!(type_filters, vec![TypeFilter::Artifact]),
            other => panic!("expected artifact SacrificedThisTurn GE 1, got {other:?}"),
        }
    }

    #[test]
    fn sacrificed_permanent_this_turn_counts_controller_sacrifices() {
        let (rest, c) = parse_inner_condition("you sacrificed a permanent this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::SacrificedThisTurn {
                                player: PlayerScope::Controller,
                                filter: TargetFilter::Typed(TypedFilter { type_filters, .. }),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => assert_eq!(type_filters, vec![TypeFilter::Permanent]),
            other => panic!("expected permanent SacrificedThisTurn GE 1, got {other:?}"),
        }
    }

    #[test]
    fn sacrificed_clue_threshold_this_turn_counts_controller_sacrifices() {
        let (rest, c) =
            parse_inner_condition("you sacrificed three or more clues this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::SacrificedThisTurn {
                                player: PlayerScope::Controller,
                                filter: TargetFilter::Typed(TypedFilter { type_filters, .. }),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            } => assert!(type_filters.contains(&TypeFilter::Subtype("Clue".to_string()))),
            other => panic!("expected Clue SacrificedThisTurn GE 3, got {other:?}"),
        }
    }

    #[test]
    fn youve_discarded_a_card_this_turn_counts_controller_discards() {
        let (rest, c) = parse_inner_condition("you've discarded a card this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::CardsDiscardedThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        );
    }

    #[test]
    fn surveilled_this_turn_counts_controller_player_actions() {
        let (rest, c) = parse_inner_condition("you've surveilled this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::PlayerActionsThisTurn {
                        player: PlayerScope::Controller,
                        action: PlayerActionKind::Surveil,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        );
    }

    #[test]
    fn scried_this_turn_counts_controller_player_actions() {
        let (rest, c) = parse_inner_condition("you scried this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::PlayerActionsThisTurn {
                        player: PlayerScope::Controller,
                        action: PlayerActionKind::Scry,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        );
    }

    #[test]
    fn opponent_discarded_a_card_this_turn_counts_opponent_discards() {
        let (rest, c) = parse_inner_condition("an opponent discarded a card this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::CardsDiscardedThisTurn {
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

    /// Issue #551 — The Raven Man: "if a player discarded a card this turn".
    /// "A player" means any player (including you), so the discards are summed
    /// across all players; the intervening-if is true whenever anyone discarded.
    #[test]
    fn a_player_discarded_a_card_this_turn_counts_all_players() {
        for text in [
            "a player discarded a card this turn",
            "any player discarded a card this turn",
        ] {
            let (rest, c) = parse_inner_condition(text).unwrap();
            assert_eq!(rest, "", "leftover for {text:?}");
            assert_eq!(
                c,
                StaticCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::CardsDiscardedThisTurn {
                            player: PlayerScope::AllPlayers {
                                aggregate: AggregateFunction::Sum,
                                exclude: None,
                            },
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                },
                "condition mismatch for {text:?}"
            );
        }
    }

    #[test]
    fn you_created_a_token_this_turn_counts_controller_tokens() {
        let (rest, c) = parse_inner_condition("you created a token this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::TokensCreatedThisTurn {
                        player: PlayerScope::Controller,
                        filter: TargetFilter::Any,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        );
    }

    #[test]
    fn youve_drawn_two_or_more_cards_this_turn_counts_controller_draws() {
        let (rest, c) = parse_inner_condition("you've drawn two or more cards this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::CardsDrawnThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            }
        );
    }

    #[test]
    fn opponent_has_drawn_four_or_more_cards_this_turn_counts_opponents() {
        let (rest, c) =
            parse_inner_condition("an opponent has drawn four or more cards this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::CardsDrawnThisTurn {
                        player: PlayerScope::Opponent {
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
    fn opponent_cast_two_or_more_spells_this_turn_counts_opponents() {
        let (rest, c) =
            parse_inner_condition("an opponent cast two or more spells this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Opponents,
                        filter: None,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            }
        );
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
                        qty: QuantityRef::AttackedThisTurn {
                            scope: CountScope::Controller,
                            filter: None,
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
    fn test_no_creatures_attacked_this_turn() {
        let (rest, c) = parse_inner_condition("no creatures attacked this turn").unwrap();
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
                        qty: QuantityRef::AttackedThisTurn {
                            scope: CountScope::All,
                            filter: Some(TargetFilter::Typed(ref tf)),
                        },
                    } if tf.type_filters.contains(&TypeFilter::Creature)
                ));
                assert_eq!(comparator, Comparator::EQ);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 0 });
            }
            _ => panic!("expected QuantityComparison, got {c:?}"),
        }
    }

    #[test]
    fn source_didnt_attack_this_turn_counts_self_with_history_filter() {
        let (rest, c) = parse_inner_condition("~ didn't attack this turn").unwrap();
        assert_eq!(rest, "");
        assert_source_history_absence(c, FilterProp::AttackedThisTurn);
    }

    #[test]
    fn source_didnt_enter_this_turn_counts_self_with_history_filter() {
        let (rest, c) =
            parse_inner_condition("this creature didn't enter the battlefield this turn").unwrap();
        assert_eq!(rest, "");
        assert_source_history_absence(c, FilterProp::EnteredThisTurn);
    }

    fn assert_source_history_absence(c: StaticCondition, prop: FilterProp) {
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ObjectCount {
                                filter: TargetFilter::And { filters },
                            },
                    },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            } => {
                assert!(filters
                    .iter()
                    .any(|filter| matches!(filter, TargetFilter::SelfRef)));
                assert!(filters.iter().any(|filter| matches!(
                    filter,
                    TargetFilter::Typed(TypedFilter { properties, .. }) if properties.contains(&prop)
                )));
            }
            other => panic!("expected source history absence condition, got {other:?}"),
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
    fn test_card_put_into_your_graveyard_from_anywhere_this_turn() {
        let (rest, c) = parse_inner_condition(
            "a creature card was put into your graveyard from anywhere this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ZoneChangeCountThisTurn {
                                from: None,
                                to: Some(Zone::Graveyard),
                                filter: TargetFilter::Typed(filter),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {
                assert!(filter.type_filters.contains(&TypeFilter::Creature));
                assert!(filter.properties.iter().any(|property| matches!(
                    property,
                    FilterProp::Owned {
                        controller: ControllerRef::You
                    }
                )));
                assert!(filter
                    .properties
                    .iter()
                    .any(|property| matches!(property, FilterProp::NonToken)));
            }
            other => {
                panic!("expected owned creature-card graveyard zone-change count, got {other:?}")
            }
        }
    }

    #[test]
    fn test_artifact_or_creature_put_into_graveyard_from_battlefield_this_turn() {
        let (rest, c) = parse_inner_condition(
            "an artifact or creature was put into a graveyard from the battlefield this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ZoneChangeCountThisTurn {
                                from: Some(Zone::Battlefield),
                                to: Some(Zone::Graveyard),
                                filter: TargetFilter::Or { filters },
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {
                assert_eq!(filters.len(), 2);
            }
            other => {
                panic!(
                    "expected artifact-or-creature battlefield-to-graveyard count, got {other:?}"
                )
            }
        }
    }

    #[test]
    fn test_card_left_your_graveyard_this_turn() {
        let (rest, c) = parse_inner_condition("a card left your graveyard this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ZoneChangeCountThisTurn {
                                from: Some(Zone::Graveyard),
                                to: None,
                                filter: TargetFilter::Typed(filter),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {
                assert!(filter.properties.iter().any(|property| matches!(
                    property,
                    FilterProp::Owned {
                        controller: ControllerRef::You
                    }
                )));
                assert!(filter
                    .properties
                    .iter()
                    .any(|property| matches!(property, FilterProp::NonToken)));
            }
            other => panic!("expected owned-card graveyard leave count, got {other:?}"),
        }
    }

    #[test]
    fn test_permanent_put_into_your_hand_from_battlefield_this_turn() {
        let (rest, c) = parse_inner_condition(
            "a permanent was put into your hand from the battlefield this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ZoneChangeCountThisTurn {
                                from: Some(Zone::Battlefield),
                                to: Some(Zone::Hand),
                                filter: TargetFilter::Typed(filter),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {
                assert!(filter.type_filters.contains(&TypeFilter::Permanent));
                assert!(filter.properties.iter().any(|property| matches!(
                    property,
                    FilterProp::Owned {
                        controller: ControllerRef::You
                    }
                )));
            }
            other => panic!("expected owned permanent battlefield-to-hand count, got {other:?}"),
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
    fn test_filtered_creature_died_under_your_control() {
        let (rest, c) =
            parse_inner_condition("a non-skeleton creature died under your control this turn")
                .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ZoneChangeCountThisTurn {
                                from: Some(Zone::Battlefield),
                                to: Some(Zone::Graveyard),
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
                assert!(filter.type_filters.iter().any(|filter| matches!(
                    filter,
                    TypeFilter::Non(inner) if matches!(inner.as_ref(), TypeFilter::Subtype(subtype) if subtype == "Skeleton")
                )));
                assert_eq!(filter.controller, Some(ControllerRef::You));
            }
            other => panic!("expected controlled non-Skeleton dies count, got {other:?}"),
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
    fn test_you_control_your_commander_is_own() {
        let (rest, c) = parse_inner_condition("you control your commander").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::ControlsCommander {
                ownership: CommanderOwnership::Own,
            }
        );
    }

    #[test]
    fn test_you_control_a_commander_is_any() {
        let (rest, c) = parse_inner_condition("you control a commander").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::ControlsCommander {
                ownership: CommanderOwnership::Any,
            }
        );
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

    #[test]
    fn test_source_you_controlled_dealt_damage_threshold_this_turn() {
        let (rest, c) =
            parse_inner_condition("a source you controlled dealt 5 or more damage this turn")
                .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::DamageDealtThisTurn {
                                source,
                                target,
                                aggregate: AggregateFunction::Max,
                                group_by: Some(DamageGroupKey::SourceId),
                                damage_kind: DamageKindFilter::Any,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 5 },
            } => {
                let TargetFilter::Typed(typed) = *source else {
                    panic!("expected typed source filter");
                };
                assert_eq!(typed.controller, Some(ControllerRef::You));
                assert_eq!(*target, TargetFilter::Any);
            }
            other => panic!("expected source-damage threshold quantity, got {other:?}"),
        }
    }

    #[test]
    fn test_player_was_dealt_damage_threshold_this_turn() {
        let (rest, c) = parse_inner_condition("you were dealt 4 or more damage this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::DamageDealtThisTurn {
                                source,
                                target,
                                aggregate: AggregateFunction::Sum,
                                group_by: None,
                                damage_kind: DamageKindFilter::Any,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            } => {
                assert_eq!(*source, TargetFilter::Any);
                let TargetFilter::Typed(typed) = *target else {
                    panic!("expected typed target filter");
                };
                assert_eq!(typed.controller, Some(ControllerRef::You));
            }
            other => panic!("expected player damage threshold quantity, got {other:?}"),
        }
    }

    #[test]
    fn test_opponent_was_dealt_damage_threshold_this_turn() {
        let (rest, c) =
            parse_inner_condition("an opponent was dealt 3 or more damage this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::DamageDealtThisTurn {
                                source,
                                target,
                                aggregate: AggregateFunction::Sum,
                                group_by: None,
                                damage_kind: DamageKindFilter::Any,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            } => {
                assert_eq!(*source, TargetFilter::Any);
                let TargetFilter::Typed(typed) = *target else {
                    panic!("expected typed target filter");
                };
                assert_eq!(typed.controller, Some(ControllerRef::Opponent));
            }
            other => panic!("expected opponent damage threshold quantity, got {other:?}"),
        }
    }

    #[test]
    fn test_source_dealt_damage_to_opponent_this_turn() {
        let (rest, c) =
            parse_inner_condition("this creature dealt damage to an opponent this turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::DamageDealtThisTurn {
                                source,
                                target,
                                aggregate: AggregateFunction::Sum,
                                group_by: None,
                                damage_kind: DamageKindFilter::Any,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {
                assert_eq!(*source, TargetFilter::SelfRef);
                let TargetFilter::Typed(target) = *target else {
                    panic!("expected typed opponent target");
                };
                assert_eq!(target.controller, Some(ControllerRef::Opponent));
            }
            other => panic!("expected self damage-to-opponent condition, got {other:?}"),
        }
    }

    #[test]
    fn test_source_was_dealt_damage_this_turn() {
        let (rest, c) = parse_inner_condition("this creature was dealt damage this turn").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::DamageDealtThisTurn {
                        source,
                        target,
                        aggregate: AggregateFunction::Sum,
                        group_by: None,
                        damage_kind: DamageKindFilter::Any,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } if source == Box::new(TargetFilter::Any)
                && target == Box::new(TargetFilter::SelfRef)
        ));
    }

    /// Issue #1347 — CR 603.4 + CR 120.2a + CR 608.2i: "a player was dealt
    /// combat damage by a Zombie this turn" (Lost Monarch of Ifnir's
    /// intervening-if) parses to a combat-only `DamageDealtThisTurn` keyed on a
    /// Zombie source and any-player recipient.
    #[test]
    fn test_player_dealt_combat_damage_by_creature_type_this_turn() {
        let (rest, c) =
            parse_inner_condition("a player was dealt combat damage by a Zombie this turn")
                .unwrap();
        assert_eq!(rest, "");
        let StaticCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::DamageDealtThisTurn {
                            source,
                            target,
                            aggregate: AggregateFunction::Sum,
                            group_by: None,
                            damage_kind: DamageKindFilter::CombatOnly,
                        },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        } = c
        else {
            panic!("expected combat-only DamageDealtThisTurn GE 1, got {c:?}");
        };
        // CR 120.1: any-player recipient (Lost Monarch reads "a player").
        assert_eq!(*target, TargetFilter::Player);
        // CR 608.2i: the source qualifier carries the Zombie subtype.
        let TargetFilter::Typed(tf) = source.as_ref() else {
            panic!("expected typed source filter, got {source:?}");
        };
        assert!(
            tf.type_filters
                .contains(&TypeFilter::Subtype("Zombie".to_string())),
            "source must be keyed on the Zombie subtype, got {:?}",
            tf.type_filters
        );
    }

    /// Issue #1347 — class coverage: the same predicate with an "an opponent"
    /// recipient and a bare-creature source ("by a creature") still parses,
    /// proving the combinator is parameterized over subject and source rather
    /// than special-cased to "a player … Zombie".
    #[test]
    fn test_opponent_dealt_combat_damage_by_creature_this_turn() {
        let (rest, c) =
            parse_inner_condition("an opponent was dealt combat damage by a creature this turn")
                .unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::DamageDealtThisTurn {
                        damage_kind: DamageKindFilter::CombatOnly,
                        ..
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }
        ));
    }

    /// CR 601.2h + CR 603.4 + CR 702.191a: Increment intervening-if parses as
    /// `And { SourceMatchesFilter(creature), Or { mana spent > self P/T } }`.
    #[test]
    fn test_parse_condition_increment_mana_spent_vs_self_pt() {
        let (rest, c) = parse_condition(
            "if the amount of mana you spent is greater than this creature's power or toughness",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::And { conditions } => {
                assert_eq!(conditions.len(), 2, "expected two conjuncts");
                assert!(matches!(
                    &conditions[0],
                    StaticCondition::SourceMatchesFilter {
                        filter: TargetFilter::Typed(tf),
                    } if tf.type_filters.contains(&TypeFilter::Creature)
                ));
                let StaticCondition::Or { conditions } = &conditions[1] else {
                    panic!("expected P/T disjunction, got {:?}", conditions[1]);
                };
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
            other => panic!("expected And, got {other:?}"),
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
                                // Subject is "it" → CR 400.7d → SelfObject.
                                scope: CastManaObjectScope::SelfObject,
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
                                // Subject is "it" → CR 400.7d → SelfObject.
                                scope: CastManaObjectScope::SelfObject,
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

    /// CR 400.7d: subject anaphora drives `CastManaObjectScope` — "this spell"
    /// on a resolving spell (Devour Intellect class) → `SelfObject`; "that
    /// spell" on a triggered ability → `TriggeringSpell`. Proves the building
    /// block across all three condition constructors.
    #[test]
    fn test_mana_spent_condition_subject_drives_scope() {
        let scope_of = |text: &str| -> CastManaObjectScope {
            let (rest, c) = parse_inner_condition(text).unwrap();
            assert_eq!(rest, "", "input {text:?} should fully parse");
            match c {
                StaticCondition::QuantityComparison {
                    lhs:
                        QuantityExpr::Ref {
                            qty: QuantityRef::ManaSpentToCast { scope, .. },
                        },
                    ..
                } => scope,
                other => panic!("expected ManaSpentToCast QuantityComparison, got {other:?}"),
            }
        };

        // Site A — source-qualified positive check.
        assert_eq!(
            scope_of("mana from a treasure was spent to cast this spell"),
            CastManaObjectScope::SelfObject,
        );
        assert_eq!(
            scope_of("mana from a treasure was spent to cast that spell"),
            CastManaObjectScope::TriggeringSpell,
        );
        // Site B — source-qualified threshold.
        assert_eq!(
            scope_of("five or more mana from an artifact was spent to cast this spell"),
            CastManaObjectScope::SelfObject,
        );
        assert_eq!(
            scope_of("five or more mana from an artifact was spent to cast that spell"),
            CastManaObjectScope::TriggeringSpell,
        );
        // Site C — bare total threshold.
        assert_eq!(
            scope_of("five or more mana was spent to cast this spell"),
            CastManaObjectScope::SelfObject,
        );
        assert_eq!(
            scope_of("five or more mana was spent to cast that spell"),
            CastManaObjectScope::TriggeringSpell,
        );
    }

    #[test]
    fn test_parse_condition_mana_spent_vs_self_power_only() {
        let (rest, c) = parse_condition(
            "if the amount of mana you spent is greater than this creature's power",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::And { conditions } => {
                assert_eq!(conditions.len(), 2);
                assert!(matches!(
                    &conditions[0],
                    StaticCondition::SourceMatchesFilter {
                        filter: TargetFilter::Typed(tf),
                    } if tf.type_filters.contains(&TypeFilter::Creature)
                ));
                let StaticCondition::QuantityComparison {
                    lhs,
                    comparator,
                    rhs,
                } = &conditions[1]
                else {
                    panic!("expected QuantityComparison, got {:?}", conditions[1]);
                };
                assert_eq!(
                    lhs,
                    &QuantityExpr::Ref {
                        qty: QuantityRef::ManaSpentToCast {
                            scope: crate::types::ability::CastManaObjectScope::TriggeringSpell,
                            metric: crate::types::ability::CastManaSpentMetric::Total
                        }
                    }
                );
                assert_eq!(*comparator, Comparator::GT);
                assert_eq!(
                    rhs,
                    &QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: crate::types::ability::ObjectScope::Source
                        }
                    }
                );
            }
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_condition_mana_spent_vs_this_permanent_pt_has_no_creature_gate() {
        let (rest, c) = parse_condition(
            "if the amount of mana you spent is greater than this permanent's power",
        )
        .unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::QuantityComparison {
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: crate::types::ability::ObjectScope::Source
                    }
                },
                ..
            }
        ));
    }

    #[test]
    fn test_parse_condition_mana_spent_vs_normalized_self_pt_has_creature_gate() {
        let (rest, c) =
            parse_condition("if the amount of mana you spent is greater than ~'s power").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::And {
                conditions,
            } if matches!(
                conditions.as_slice(),
                [
                    StaticCondition::SourceMatchesFilter {
                        filter: TargetFilter::Typed(tf),
                    },
                    StaticCondition::QuantityComparison {
                        rhs: QuantityExpr::Ref {
                            qty: QuantityRef::Power {
                                scope: crate::types::ability::ObjectScope::Source
                            },
                        },
                        ..
                    },
                ] if tf.type_filters.contains(&TypeFilter::Creature)
            )
        ));
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

    /// Bound-pronoun subject `"it "` — the duration grammar
    /// (`parse_recipient_has_counters`, used by `Duration::ForAsLongAs` in
    /// duration.rs for clauses like "has flying for as long as it has a flood
    /// counter on it") binds "it" to the recipient/affected object.
    #[test]
    fn has_counters_pronoun_subject_it_any() {
        let (rest, c) = parse_recipient_has_counters("it has a counter on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::RecipientHasCounters {
                counters: CounterMatch::Any,
                minimum: 1,
                maximum: None,
            }
        );
    }

    /// Regression for the coverage-honesty flip (#3084): the bare pronoun "it"
    /// in an intervening-"if" trigger condition (Ayara's Oathsworn — "whenever ~
    /// attacks, if it has three or more +1/+1 counters on it, …") is
    /// source-referential. It must stay `HasCounters` (evaluated against the
    /// triggering source), not `RecipientHasCounters`, which has no recipient at
    /// trigger-evaluation time and is silently swallowed by the coverage gate.
    #[test]
    fn parse_inner_condition_it_has_counters_is_source_referential() {
        let (rest, c) = parse_inner_condition("it has three or more +1/+1 counters on it").unwrap();
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

    // --- Typed-counter (CounterMatch::OfType) variants -----------------------

    /// Unleash / Outlast body: "it has a +1/+1 counter on it" (article → min 1).
    /// A static-gate "as long as" condition (via `parse_inner_condition`) is
    /// source-referential: "it" = this creature, evaluated against the source.
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
        let (rest, c) = parse_recipient_has_counters("it has a flood counter on it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::RecipientHasCounters {
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
                            exclude: None,
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
                            exclude: None,
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
                            exclude: None,
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

    /// Issue #477 — Renegade Reaper: quantifier prefix "at least one" with a
    /// subtype, "card" noun, and singular verb. The quantifier collapses to the
    /// existential `ZoneChangedThisWay` (≥ 1).
    #[test]
    fn test_zone_changed_this_way_at_least_one_subtype() {
        let (rest, (filter, negated)) = parse_zone_changed_this_way_clause(
            "at least one angel card is milled this way, you gain 4 life",
        )
        .unwrap();
        assert_eq!(rest, ", you gain 4 life");
        assert!(!negated);
        match filter {
            TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                assert!(
                    type_filters.iter().any(
                        |f| matches!(f, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Angel"))
                    ),
                    "expected Subtype Angel, got {type_filters:?}"
                );
            }
            other => panic!("expected Typed Angel, got {other:?}"),
        }
    }

    /// Issue #477 — The Wise Mothman: quantifier "one or more" + bare `cards`
    /// type + `nonland` negated-type prefix + **plural verb** "are".
    #[test]
    fn test_zone_changed_this_way_one_or_more_nonland_cards_plural() {
        let (rest, (filter, negated)) = parse_zone_changed_this_way_clause(
            "one or more nonland cards are exiled this way, you draw a card",
        )
        .unwrap();
        assert_eq!(rest, ", you draw a card");
        assert!(!negated);
        match filter {
            TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                assert!(
                    type_filters.iter().any(|f| matches!(f, TypeFilter::Card)),
                    "expected TypeFilter::Card, got {type_filters:?}"
                );
            }
            other => panic!("expected Typed Card, got {other:?}"),
        }
    }

    /// Issue #477 — Augusta: quantifier "one or more" + bare `cards` type +
    /// **plural verb** "are milled".
    #[test]
    fn test_zone_changed_this_way_one_or_more_cards_plural_milled() {
        let (rest, (filter, negated)) = parse_zone_changed_this_way_clause(
            "one or more cards are milled this way, you gain 1 life",
        )
        .unwrap();
        assert_eq!(rest, ", you gain 1 life");
        assert!(!negated);
        match filter {
            TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                assert!(
                    type_filters.iter().any(|f| matches!(f, TypeFilter::Card)),
                    "expected TypeFilter::Card, got {type_filters:?}"
                );
            }
            other => panic!("expected Typed Card, got {other:?}"),
        }
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

    /// CR 109.5: `add_owned_you_with_props` is the unified replacement for the
    /// prior `add_owned_you` / `add_owned_you_non_token` pair. With an empty
    /// extras slice it must produce only the `Owned { You }` tag (the bare
    /// "owned by you" shape); with `&[FilterProp::NonToken]` it must additionally
    /// carry the `NonToken` tag. Both `Typed` inputs and `Any` (lifted to a
    /// fresh `Typed` filter) must follow the same uniqueness rule.
    #[test]
    fn add_owned_you_with_props_matches_legacy_helper_shapes() {
        // Empty extras + Any input → fresh Typed filter with Owned only.
        let owned_only = add_owned_you_with_props(TargetFilter::Any, &[]);
        assert_eq!(
            owned_only,
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Owned {
                controller: ControllerRef::You,
            }])),
        );

        // NonToken extras + Any input → Owned + NonToken in that order.
        let owned_non_token = add_owned_you_with_props(TargetFilter::Any, &[FilterProp::NonToken]);
        assert_eq!(
            owned_non_token,
            TargetFilter::Typed(TypedFilter::default().properties(vec![
                FilterProp::Owned {
                    controller: ControllerRef::You,
                },
                FilterProp::NonToken,
            ])),
        );

        // Typed input that already carries an `Owned { Opponent }` tag must NOT
        // gain a second `Owned` entry — variant-tag uniqueness, not value
        // equality. This mirrors the legacy `matches!(p, FilterProp::Owned { .. })`
        // presence check.
        let pre_owned =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Owned {
                controller: ControllerRef::Opponent,
            }]));
        let after = add_owned_you_with_props(pre_owned.clone(), &[FilterProp::NonToken]);
        match after {
            TargetFilter::Typed(typed) => {
                let owned_count = typed
                    .properties
                    .iter()
                    .filter(|p| matches!(p, FilterProp::Owned { .. }))
                    .count();
                assert_eq!(owned_count, 1, "must not duplicate Owned tag");
                assert!(typed.properties.contains(&FilterProp::NonToken));
            }
            other => panic!("expected Typed, got {other:?}"),
        }

        // Non-typed/non-Any inputs (e.g., Player) must pass through unchanged
        // — owner-tagging is meaningless on those shapes.
        let unchanged = add_owned_you_with_props(TargetFilter::Player, &[FilterProp::NonToken]);
        assert_eq!(unchanged, TargetFilter::Player);
    }

    /// CR 208.1 + CR 603.4 + CR 109.3: Selvala-class superlative-comparison
    /// gate — "its power is greater than each other creature's power" must
    /// emit a `QuantityComparison` whose RHS is an aggregate (Max, Power)
    /// over creatures excluding the triggering object.
    #[test]
    fn parse_inner_condition_superlative_each_other_power_greater_than() {
        let (rest, c) =
            parse_inner_condition("its power is greater than each other creature's power.")
                .unwrap();
        assert_eq!(rest, ".");
        match c {
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => {
                assert_eq!(comparator, Comparator::GT);
                assert_eq!(
                    lhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: ObjectScope::EventSource,
                        },
                    }
                );
                match rhs {
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::Aggregate {
                                function,
                                property,
                                filter,
                            },
                    } => {
                        assert_eq!(function, AggregateFunction::Max);
                        assert_eq!(property, ObjectProperty::Power);
                        match filter {
                            TargetFilter::Typed(tf) => {
                                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                                assert!(
                                    tf.properties.contains(&FilterProp::OtherThanTriggerObject),
                                    "expected OtherThanTriggerObject, got {:?}",
                                    tf.properties
                                );
                            }
                            other => panic!("expected Typed creature, got {other:?}"),
                        }
                    }
                    other => panic!("expected Aggregate Max Power, got {other:?}"),
                }
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    /// "less than" variant: aggregate function should switch to Min.
    #[test]
    fn parse_inner_condition_superlative_each_other_power_less_than() {
        let (_rest, c) =
            parse_inner_condition("its power is less than each other creature's power.").unwrap();
        match c {
            StaticCondition::QuantityComparison {
                comparator,
                rhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::Aggregate { function, .. },
                    },
                ..
            } => {
                assert_eq!(comparator, Comparator::LT);
                assert_eq!(function, AggregateFunction::Min);
            }
            other => panic!("expected QuantityComparison with Aggregate, got {other:?}"),
        }
    }

    /// "greater than or equal to" variant: comparator should be GE, aggregate Max.
    #[test]
    fn parse_inner_condition_superlative_each_other_ge() {
        let (_rest, c) = parse_inner_condition(
            "its toughness is greater than or equal to each other creature's toughness.",
        )
        .unwrap();
        match c {
            StaticCondition::QuantityComparison {
                comparator,
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::Toughness {
                                scope: ObjectScope::EventSource,
                            },
                    },
                rhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::Aggregate {
                                function, property, ..
                            },
                    },
            } => {
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(function, AggregateFunction::Max);
                assert_eq!(property, ObjectProperty::Toughness);
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    /// "it has the greatest power among" surface form — equivalent to the
    /// inequality form but as a "has the X" predicate. Strict GT.
    #[test]
    fn parse_inner_condition_superlative_has_greatest_power() {
        let (_rest, c) =
            parse_inner_condition("it has the greatest power among creatures on the battlefield.")
                .unwrap();
        match c {
            StaticCondition::QuantityComparison {
                comparator,
                rhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::Aggregate { function, .. },
                    },
                ..
            } => {
                assert_eq!(comparator, Comparator::GT);
                assert_eq!(function, AggregateFunction::Max);
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    /// CR 702.185c: "a spell was warped this turn" parses to the
    /// `SpellCastWithVariantThisTurn { Warp }` condition.
    #[test]
    fn parse_inner_condition_spell_warped_this_turn() {
        let (rest, c) = parse_inner_condition("a spell was warped this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            c,
            StaticCondition::SpellCastWithVariantThisTurn {
                variant: crate::types::game_state::CastingVariant::Warp,
            }
        );
    }

    /// CR 608.2c + CR 702.185c: Plasma Bolt's Void clause — a two-sided
    /// disjunction "<zone-history> or a spell was warped this turn" parses to
    /// `StaticCondition::Or` over the existing left-half condition and the
    /// warp-half condition.
    #[test]
    fn parse_inner_condition_nonland_left_or_spell_warped() {
        let (rest, c) = parse_inner_condition(
            "a nonland permanent left the battlefield this turn or a spell was warped this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::Or { conditions } => {
                assert_eq!(conditions.len(), 2);
                assert_eq!(
                    conditions[1],
                    StaticCondition::SpellCastWithVariantThisTurn {
                        variant: crate::types::game_state::CastingVariant::Warp,
                    }
                );
            }
            other => panic!("expected Or, got {other:?}"),
        }
    }

    /// "it has the greatest power or is tied for greatest power among" — the
    /// "or is tied for" tail relaxes strict GT to GE.
    #[test]
    fn parse_inner_condition_superlative_has_greatest_or_tied_for_greatest() {
        let (_rest, c) = parse_inner_condition(
            "it has the greatest power or is tied for greatest power among creatures on the battlefield.",
        )
        .unwrap();
        match c {
            StaticCondition::QuantityComparison { comparator, .. } => {
                assert_eq!(comparator, Comparator::GE);
            }
            other => panic!("expected QuantityComparison, got {other:?}"),
        }
    }

    /// CR 702: "a creature you control has <keyword>" — subject-first
    /// presence check. Building block behind Odric, Lunarch Marshal's
    /// in-effect "if" gate.
    #[test]
    fn parse_inner_condition_creature_you_control_has_first_strike() {
        let (rest, c) = parse_inner_condition("a creature you control has first strike").unwrap();
        assert!(rest.is_empty());
        match c {
            StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(tf)),
            } => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf
                    .properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::WithKeyword { value } if *value == Keyword::FirstStrike)));
            }
            other => panic!("expected IsPresent(Typed), got {other:?}"),
        }
    }

    /// The combinator generalizes over the whole evergreen vocabulary —
    /// "flying" works exactly as "first strike" does.
    #[test]
    fn parse_inner_condition_creature_you_control_has_flying() {
        let (_rest, c) = parse_inner_condition("a creature you control has flying").unwrap();
        match c {
            StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(tf)),
            } => {
                assert!(tf.properties.iter().any(
                    |p| matches!(p, FilterProp::WithKeyword { value } if *value == Keyword::Flying)
                ));
            }
            other => panic!("expected IsPresent(Typed), got {other:?}"),
        }
    }

    /// No controller suffix — the bare "a creature has trample" form still
    /// parses (controller stays unset).
    #[test]
    fn parse_inner_condition_creature_has_keyword_no_controller_suffix() {
        let (_rest, c) = parse_inner_condition("a creature has trample").unwrap();
        match c {
            StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(tf)),
            } => {
                assert!(tf.properties.iter().any(
                    |p| matches!(p, FilterProp::WithKeyword { value } if *value == Keyword::Trample)
                ));
            }
            other => panic!("expected IsPresent(Typed), got {other:?}"),
        }
    }

    /// A trailing word that is not an evergreen keyword must fail the
    /// combinator rather than mis-parsing.
    #[test]
    fn parse_creature_has_keyword_rejects_non_keyword() {
        assert!(parse_creature_has_keyword("a creature you control has counters").is_err());
    }

    /// Issue #2919: "as long as it was cast" must lower to WasCast, not Unrecognized.
    #[test]
    fn parse_inner_condition_it_was_cast() {
        let (rest, c) = parse_inner_condition("it was cast").unwrap();
        assert!(rest.is_empty());
        assert_eq!(c, StaticCondition::WasCast { zone: None });
    }
}
