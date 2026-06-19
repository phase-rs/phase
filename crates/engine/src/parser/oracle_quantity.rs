//! Quantity expression parsing from Oracle text.
//!
//! This module consolidates semantic quantity interpretation — mapping Oracle text
//! phrases like "the number of creatures you control" or "your life total" into
//! typed `QuantityRef` / `QuantityExpr` values. This is distinct from `oracle_util`,
//! which provides raw text extraction primitives (number parsing, mana symbol
//! counting, phrase matching).
//!
//! **Frozen for new grammar.** New quantity-phrase recognition belongs in
//! `oracle_nom/quantity.rs` (the combinator grammar this module delegates
//! to), not here — this module's remaining surface is the legacy semantic
//! entry points (`parse_cda_quantity`, `parse_quantity_ref`,
//! `parse_event_context_quantity`, `parse_for_each_clause`) and their
//! context wiring. Adding a new phrase table or `tag()` alternative here
//! re-creates the parallel-grammar split that the oracle-parser skill's
//! "Where New Grammar Goes" section exists to prevent.

use std::str::FromStr;

use crate::parser::oracle_nom::error::{OracleError, OracleResult};
use nom::branch::alt;
use nom::bytes::complete::{tag, take_till1, take_until};
use nom::combinator::{all_consuming, eof, opt, peek, value};
use nom::multi::separated_list1;
use nom::sequence::{pair, preceded, terminated};
use nom::Parser;

use super::oracle_ir::context::ParseContext;
use super::oracle_nom::bridge::nom_on_lower;
use super::oracle_nom::condition::{inject_controller_you, parse_spell_history_filter};
use super::oracle_nom::duration::parse_cast_snapshot_suffix;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_nom::quantity as nom_quantity;
use super::oracle_nom::target as nom_target;
use crate::parser::oracle_effect::counter::normalize_counter_type;
use crate::parser::oracle_effect::parse_controls_permanent_object;
use crate::parser::oracle_target::{parse_target, parse_type_phrase, parse_type_phrase_with_ctx};
use crate::parser::oracle_util::merge_or_filters;
use crate::types::ability::{
    AggregateFunction, AttackScope, AttackSubject, Comparator, ControllerRef, CountScope,
    DevotionColors, FilterProp, ObjectProperty, ObjectScope, PlayerFilter, PlayerRelation,
    PlayerScope, QuantityExpr, QuantityRef, RoundingMode, TargetFilter, ThisWayCause, TypeFilter,
    TypedFilter, ZoneRef,
};
#[cfg(test)]
use crate::types::counter::CounterType;
use crate::types::events::PlayerActionKind;
use crate::types::keywords::KeywordKind;
use crate::types::mana::ManaColor;
use crate::types::zones::Zone;

/// Map a quantity phrase to a dynamic QuantityRef.
///
/// Delegates to `oracle_nom::quantity::parse_quantity_ref` for simple exact-match
/// patterns (life total, hand size, graveyard size, self P/T, life lost/gained,
/// starting life total), then falls through to complex patterns (counters,
/// aggregates, object counts, devotion, etc.) that nom doesn't yet cover.
pub(crate) fn parse_quantity_ref(text: &str) -> Option<QuantityRef> {
    let mut ctx = ParseContext::default();
    parse_quantity_ref_with_context(text, &mut ctx)
}

/// CR 119.1 + CR 102.1: "the {highest|lowest} life total among {all players|
/// players|your opponents}" → LifeTotal{ AllPlayers|Opponent { aggregate } }.
/// Two independent nom axes (aggregate × population) — not full-string tags.
/// Life is CR 119 → routes to LifeTotal/PlayerScope, never the CR 208/202
/// object-property Aggregate (hence placed before that block).
fn parse_cross_player_life_extremum(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("the ").parse(input)?;
    let (rest, aggregate) = alt((
        value(AggregateFunction::Max, tag("highest")),
        value(AggregateFunction::Min, tag("lowest")),
    ))
    .parse(rest)?;
    let (rest, _) = tag(" life total among ").parse(rest)?;
    let (rest, player) = alt((
        value(
            PlayerScope::AllPlayers {
                aggregate,
                exclude: None,
            },
            alt((tag("all players"), tag("players"))),
        ),
        value(PlayerScope::Opponent { aggregate }, tag("your opponents")),
    ))
    .parse(rest)?;
    Ok((rest, QuantityRef::LifeTotal { player }))
}

pub(crate) fn parse_quantity_ref_with_context(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<QuantityRef> {
    let trimmed = text.trim().trim_end_matches('.');

    // Try nom combinator first for simple exact-match patterns.
    if let Ok((rest, qty)) = nom_quantity::parse_quantity_ref.parse(trimmed) {
        if rest.is_empty() {
            return Some(canonicalize_quantity_ref(qty));
        }
    }

    // Complex patterns requiring type phrase parsing or counter normalization.

    // CR 608.2c + CR 122.1: "the number of [kind] counter[s] removed this way"
    // is a dynamic amount from the preceding RemoveCounter effect, not an
    // object count over a battlefield type phrase.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("the number of ").parse(trimmed) {
        if try_parse_counters_removed_this_way(rest) {
            return Some(QuantityRef::PreviousEffectAmount);
        }
    }

    if let Some(qty) = parse_milled_this_way_count(trimmed) {
        return Some(qty);
    }

    if all_consuming(pair(
        tag::<_, _, OracleError<'_>>("the number of"),
        alt((tag(" counters on ~"), tag(" counters on it"))),
    ))
    .parse(trimmed)
    .is_ok()
    {
        return Some(QuantityRef::CountersOn {
            scope: ObjectScope::Source,
            counter_type: None,
        });
    }

    if all_consuming(pair(
        tag::<_, _, OracleError<'_>>("the number of"),
        alt((
            tag(" counters on that creature"),
            tag(" counters on that permanent"),
        )),
    ))
    .parse(trimmed)
    .is_ok()
    {
        return Some(QuantityRef::CountersOn {
            scope: ObjectScope::Target,
            counter_type: None,
        });
    }

    // "[counter type] counter(s) on ~" / "[counter type] counter(s) on it"
    // Handles both plural ("counters on ~") and singular ("counter on ~") forms.
    if let Some(rest) = trimmed
        .strip_suffix(" counters on ~")
        .or_else(|| trimmed.strip_suffix(" counters on it"))
        .or_else(|| trimmed.strip_suffix(" counter on ~"))
        .or_else(|| trimmed.strip_suffix(" counter on it"))
    {
        let raw_type = tag::<_, _, OracleError<'_>>("the number of ")
            .parse(rest)
            .map_or(rest, |(r, _)| r)
            .trim();
        if !raw_type.is_empty() {
            let counter_type = normalize_counter_type(raw_type);
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
        let raw_type = tag::<_, _, OracleError<'_>>("the number of ")
            .parse(rest)
            .map_or(rest, |(r, _)| r)
            .trim();
        if !raw_type.is_empty() {
            let counter_type = normalize_counter_type(raw_type);
            return Some(QuantityRef::CountersOn {
                scope: ObjectScope::Target,
                counter_type: Some(counter_type),
            });
        }
    }

    // "the number of [counter type] counters on [filter]" — total counters across
    // all matching objects, distinct from object count.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("the number of ").parse(trimmed) {
        for suffix in [
            " counters on ",
            " counter on ",
            " counters among ",
            " counter among ",
        ] {
            let Ok((after_suffix, counter_text)) =
                take_until::<_, _, OracleError<'_>>(suffix).parse(rest)
            else {
                continue;
            };
            let Ok((after_filter, _)) = tag::<_, _, OracleError<'_>>(suffix).parse(after_suffix)
            else {
                continue;
            };
            let counter_text = counter_text.trim();
            if counter_text.is_empty() {
                continue;
            }
            let counter_type = normalize_counter_type(counter_text);
            let (filter, remainder) = parse_type_phrase_with_ctx(after_filter, ctx);
            if remainder.trim().is_empty()
                && !matches!(filter, TargetFilter::Any)
                && !is_empty_typed_filter(&filter)
            {
                return Some(QuantityRef::CountersOnObjects {
                    counter_type: Some(counter_type),
                    filter,
                });
            }
        }
    }

    // CR 119.1 + CR 102.1: cross-player life extremum ("the highest/lowest life
    // total among …"). Life is CR 119 → must route to LifeTotal/PlayerScope, not
    // the CR 208/202 object-property Aggregate below — wired first so the
    // aggregate block can't claim it.
    if let Ok((rest, qty)) = parse_cross_player_life_extremum(trimmed) {
        if rest.is_empty() {
            return Some(qty);
        }
    }

    // Aggregate patterns: "the greatest X among" / "the total power of"
    if let Ok((rest, (func, prop))) = alt((
        value(
            (AggregateFunction::Max, ObjectProperty::Power),
            tag::<_, _, OracleError<'_>>("the greatest power among "),
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
        // CR 208.1: total toughness sum for "the total toughness of <filter>"
        // phrasing. Building-block companion to the trigger-condition predicate
        // "<filter> have total toughness N or greater" added in
        // `oracle_nom::condition::parse_filter_have_total_property`.
        value(
            (AggregateFunction::Sum, ObjectProperty::Toughness),
            tag("the total toughness of "),
        ),
        // CR 202.3: total mana value sum, parallel building block to the
        // power and toughness aggregates above.
        value(
            (AggregateFunction::Sum, ObjectProperty::ManaValue),
            tag("the total mana value of "),
        ),
    ))
    .parse(trimmed)
    {
        // CR 608.2c + CR 609.3 + CR 107.3e: "the total <property> of those exiled
        // cards" is an aggregate over the most recent chain tracked set, not over live
        // battlefield objects — the anaphor "those exiled cards" refers to the set
        // the preceding effect published (e.g. Ensnared by the Mara's `ExileTop`).
        // Matched before `parse_type_phrase_with_ctx` so the exile anaphor isn't
        // mis-read as a type phrase. Reuses the established exile-anaphor pair from
        // `oracle_effect::mod` (`those exiled cards` / `the exiled cards`).
        if let Ok((anaphor_rest, _)) = alt((
            tag::<_, _, OracleError<'_>>("those exiled cards"),
            tag("the exiled cards"),
        ))
        .parse(rest)
        {
            if anaphor_rest.trim().is_empty() {
                return Some(QuantityRef::TrackedSetAggregate {
                    function: func,
                    property: prop,
                });
            }
        }
        let (filter, remainder) = parse_type_phrase_with_ctx(rest, ctx);
        // CR 608.2h: present-tense aggregate. Accept a bare empty remainder
        // (existing no-snapshot behavior) or a trailing cast/activation-time
        // snapshot suffix ("as you cast this spell") — the suffix is a pure
        // timing marker that the resolver honors, so it must not block the
        // filter check.
        let snapshot_ok = remainder.trim().is_empty()
            || parse_cast_snapshot_suffix(remainder)
                .map(|(r, _)| r.trim().is_empty())
                .unwrap_or(false);
        if snapshot_ok && !matches!(filter, TargetFilter::Any) && !is_empty_typed_filter(&filter) {
            return Some(QuantityRef::Aggregate {
                function: func,
                property: prop,
                filter,
            });
        }

        // CR 400.7 + CR 700.4: "the total power of <filter> that died [under your
        // control] this turn" aggregates over this turn's battlefield→graveyard
        // zone-change records, not live battlefield objects — the objects have
        // left play and carry their death-time P/T snapshot. Reuse the filter
        // parse_type_phrase_with_ctx already produced (above) and run the shared
        // death-suffix combinator on its remainder. Placed before the past-tense
        // "you controlled" arm so it isn't shadowed, and after the present-tense
        // arm so plain "the total power of creatures you control" stays a live
        // `Aggregate`.
        if !matches!(filter, TargetFilter::Any) && !is_empty_typed_filter(&filter) {
            if let Ok((after, controller)) =
                nom_quantity::parse_died_this_turn_suffix(remainder.trim_start())
            {
                if after.trim().is_empty() {
                    // Only ControllerRef::You is producible by the suffix combinator.
                    let filter = if controller.is_some() {
                        inject_controller_you(filter)
                    } else {
                        filter
                    };
                    return Some(QuantityRef::ZoneChangeAggregateThisTurn {
                        from: Some(Zone::Battlefield),
                        to: Some(Zone::Graveyard),
                        filter,
                        function: func,
                        property: prop,
                    });
                }
            }
        }

        // CR 608.2i: past-tense "you controlled" look-back. tag("you control") in
        // parse_zone_controller has no word boundary and would prefix-match
        // "you controlled", corrupting the remainder to "led …". Isolate the bare
        // head via take_until(" you controlled ") BEFORE parse_type_phrase, then
        // re-inject ControllerRef::You. Reuses the inject_controller_you building
        // block; same strip-controller-before-type-phrase ordering as
        // parse_controller_controlled_as_cast_condition
        // (oracle_effect/conditions.rs:1444).
        if let Ok((after_head_tag, head_text)) =
            take_until::<_, _, OracleError<'_>>(" you controlled ").parse(rest)
        {
            let (head_filter, head_rem) = parse_type_phrase(head_text);
            if head_rem.trim().is_empty()
                && !matches!(head_filter, TargetFilter::Any)
                && !is_empty_typed_filter(&head_filter)
            {
                if let Ok((after_ctrl, _)) =
                    tag::<_, _, OracleError<'_>>(" you controlled ").parse(after_head_tag)
                {
                    if let Ok((rest2, _)) = parse_cast_snapshot_suffix(after_ctrl) {
                        if rest2.trim().is_empty() {
                            return Some(QuantityRef::Aggregate {
                                function: func,
                                property: prop,
                                filter: inject_controller_you(head_filter),
                            });
                        }
                    }
                }
            }
        }
    }

    // "the number of {type} you control" → ObjectCount { filter }
    // "the number of opponents you have" → PlayerCount { Opponent }
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("the number of ").parse(trimmed) {
        if rest == "opponents you have" || rest == "opponent you have" {
            return Some(QuantityRef::PlayerCount {
                filter: PlayerFilter::Opponent,
            });
        }
        // CR 104.3: "players who have lost the game" (Rampant Frogantua quantity form).
        if let Ok((remainder, ())) = value(
            (),
            (
                alt((
                    tag::<_, _, OracleError<'_>>("players who have "),
                    tag("player who has "),
                )),
                tag("lost the game"),
            ),
        )
        .parse(rest)
        {
            if remainder.trim().is_empty() {
                return Some(QuantityRef::PlayerCount {
                    filter: PlayerFilter::HasLostTheGame,
                });
            }
        }
        // CR 120.1 + CR 510.1: "opponents that were dealt combat damage
        // [this turn]". The trailing " this turn" suffix is optional because
        // upstream callers may strip durations before this parser sees the
        // phrase. PlayerCount{OpponentDealtCombatDamage} is inherently scoped
        // to this turn through `state.damage_dealt_this_turn`.
        if let Ok((_, source)) = parse_opponent_dealt_combat_damage_clause(rest) {
            return Some(QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentDealtCombatDamage {
                    source: source.map(Box::new),
                },
            });
        }
        // CR 508.6: "opponents you attacked [this turn]" (Militant Angel).
        if parse_opponents_attacked_clause(rest).is_ok() {
            return Some(QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentAttacked {
                    subject: AttackSubject::You,
                    scope: AttackScope::ThisTurn,
                },
            });
        }
        // CR 109.4 + CR 109.5: "opponents who control <filter>" / "opponents who
        // don't control <filter>" / "players who control more <type> than you" →
        // PlayerCount over the population satisfying the shared control predicate.
        // Consume only the population word here (capturing its relation), then
        // hand the "who controls …" remainder to the shared
        // `parse_controls_permanent_object` core (DRY with the "each opponent who
        // controls …" subject path). Tried before the generic ObjectCount
        // fall-through so the player population — not battlefield permanents — is
        // counted. The population word also fixes the relation: "opponents"/
        // "opponent" → Opponent; "players"/"player" → All (so "the number of
        // players who control more lands than you", Oreskos Explorer, is covered,
        // not just the opponent cards). (Singular forms are accepted for the
        // grammatically-degenerate one-player phrasing.)
        if let Ok((predicate_input, relation)) = alt((
            value(
                PlayerRelation::Opponent,
                tag::<_, _, OracleError<'_>>("opponents "),
            ),
            value(PlayerRelation::Opponent, tag("opponent ")),
            value(PlayerRelation::All, tag("players ")),
            value(PlayerRelation::All, tag("player ")),
        ))
        .parse(rest)
        {
            if let Some((comparator, count, filter, remainder)) =
                parse_controls_permanent_object(predicate_input, ctx)
            {
                if remainder.trim().is_empty() {
                    return Some(QuantityRef::PlayerCount {
                        filter: PlayerFilter::ControlsCount {
                            relation,
                            filter,
                            comparator,
                            count: Box::new(count),
                        },
                    });
                }
            }
        }
        // CR 402.1 / 119.1 / 122.1f / 404.1: "opponents who have N or more
        // <kind> counters" (Glissa's Retriever) / "your opponents with N or
        // more cards in hand" (Wolfcaller's Howl) → PlayerCount over the
        // population whose per-candidate scalar attribute compares to N. Tried
        // before the generic ObjectCount fall-through so the player population —
        // not battlefield permanents — is counted.
        if let Ok((remainder, filter)) = parse_player_attribute_predicate(rest) {
            if remainder.trim().is_empty() {
                return Some(QuantityRef::PlayerCount { filter });
            }
        }
        // CR 608.2c + CR 109.5: "the number of [population] who [verb]ed … this
        // way" — the count of players who performed the preceding optional
        // action (Wernog's "the number of opponents who investigated this way").
        // Shares the verb-dispatched combinator with the for-each path above, so
        // search and investigate (and any future verb) stay one building block.
        // Tried before the generic ObjectCount fall-through so the player
        // population — not battlefield permanents — is counted.
        if let Ok((remainder, (relation, action))) = parse_action_this_way(rest) {
            if remainder.trim().is_empty() {
                return Some(QuantityRef::PlayerCount {
                    filter: PlayerFilter::PerformedActionThisWay { relation, action },
                });
            }
        }
        if let Ok((remainder, (relation, action))) = parse_optional_offer_accepted_clause(rest) {
            if remainder.trim().is_empty() {
                return Some(QuantityRef::PlayerCount {
                    filter: PlayerFilter::PerformedActionThisWay { relation, action },
                });
            }
        }
        // CR 608.2c + CR 400.7: "the number of [filter] destroyed/sacrificed
        // this way" — count from the tracked set populated by the preceding
        // destroy/sacrifice in the sub_ability chain. Must run BEFORE
        // `parse_type_phrase`, which would consume "creatures you controlled"
        // and leave an unresolved "that were destroyed this way" tail.
        // Class: Kaya's Wrath (issue #2943), Ceaseless Conflict, and any
        // "equal to the number of … destroyed this way" lifegain phrasing.
        if let Some(qty) =
            parse_destroyed_or_sacrificed_this_way_quantity(&rest.to_ascii_lowercase())
        {
            return Some(qty);
        }
        let (filter, remainder) = parse_type_phrase_with_ctx(rest, ctx);
        // CR 109.1: `parse_type_phrase_with_ctx` always returns `TargetFilter::Typed`,
        // including the empty-shaped form (no `type_filters`, no `controller`, no
        // `properties`) when the input has no recognized type word (e.g.
        // "opponents that were dealt combat damage this turn"). The empty shape
        // matches every battlefield object, so emitting an `ObjectCount` against
        // it would silently drain every permanent. Treat the empty shape as
        // "no type-phrase match" and fall through to the next pattern (or
        // surface `Unimplemented`) instead.
        if remainder.trim().is_empty()
            && !matches!(filter, TargetFilter::Any)
            && !is_empty_typed_filter(&filter)
        {
            return Some(QuantityRef::ObjectCount { filter });
        }
    }
    // "your devotion to that color" / "your devotion to {color}" /
    // "your devotion to {color} and {color}"
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("your devotion to ").parse(trimmed) {
        if tag::<_, _, OracleError<'_>>("that color")
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

fn parse_milled_this_way_count(text: &str) -> Option<QuantityRef> {
    all_consuming((
        tag::<_, _, OracleError<'_>>("the number of "),
        opt(tag("nonland ")),
        alt((tag("cards"), tag("card"))),
        tag(" milled this way"),
    ))
    .parse(text)
    .is_ok()
    .then_some(QuantityRef::EventContextAmount)
}

/// CR 109.1: `parse_type_phrase` always returns `TargetFilter::Typed`, even
/// when no type word was matched — in that case all three of `type_filters`,
/// `controller`, and `properties` are empty. An empty-shaped `Typed` matches
/// *every* battlefield object, so callers that interpret a non-`Any` filter
/// as "type phrase recognized" must reject this shape explicitly. The
/// building-block guard lives here so every quantity parser that wraps
/// `parse_type_phrase` shares one consistent rejection rule.
pub(crate) fn is_empty_typed_filter(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::Typed(typed)
            if typed.type_filters.is_empty()
                && typed.controller.is_none()
                && typed.properties.is_empty()
    )
}

pub(crate) fn canonicalize_quantity_ref(qty: QuantityRef) -> QuantityRef {
    match qty {
        QuantityRef::ZoneCardCount {
            zone: ZoneRef::Hand,
            card_types,
            filter: None,
            scope: CountScope::Controller,
        } if card_types.is_empty() => QuantityRef::HandSize {
            player: PlayerScope::Controller,
        },
        QuantityRef::ZoneCardCount {
            zone: ZoneRef::Graveyard,
            card_types,
            filter: None,
            scope: CountScope::Controller,
        } if card_types.is_empty() => QuantityRef::GraveyardSize {
            player: PlayerScope::Controller,
        },
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
    let mut ctx = ParseContext::default();
    parse_cda_quantity_with_context(text, &mut ctx)
}

pub(crate) fn parse_cda_quantity_with_context(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<QuantityExpr> {
    let text = text.trim().trim_end_matches('.');

    // CR 107.1a: "half/third/tenth <inner>, rounded up/down" fractional
    // quantities delivered via a "where X is …" binding or a CDA route through
    // here (Chainer's Torment, Endless Ranks of the Dead, Ghoulcaller's Harvest,
    // Imskir Iron-Eater). Delegate to the shared `parse_fraction_rounded`
    // combinator so every inner the general quantity grammar recognizes
    // (life totals, "the number of <type> you control", "<type> cards in your
    // graveyard", possessive refs, …) composes — without this arm the phrase
    // falls through to `Variable { name: "<whole phrase>" }`, which resolves to 0
    // at runtime (a silent no-op). Tried first so the leading "half " is consumed
    // before the single-ref / binary-arithmetic arms below.
    if let Ok((rest, expr)) = nom_quantity::parse_fraction_rounded(text) {
        if rest.is_empty() {
            return Some(expr);
        }
    }

    // CR 107.1a: Fraction over a CDA-recursive inner — "half/third/tenth <inner>,
    // rounded up/down" where <inner> is any quantity THIS function recognizes but
    // the general nom grammar above does not (notably the cross-player aggregate
    // "the highest life total among your opponents" — Malignus). Reuse the shared
    // divisor / rounding combinators, then recurse on the inner so the fraction
    // composes over the full CDA quantity grammar (mirrors the "twice [inner]"
    // recursion below).
    if let Ok((after_divisor, divisor)) = nom_quantity::parse_fraction_divisor(text) {
        // Optional "of " ("half of ~"), consumed via the nom combinator per the
        // parser mandate.
        let (after_divisor, _) = opt(tag::<_, _, OracleError<'_>>("of "))
            .parse(after_divisor)
            .ok()?;

        // `parse_cda_quantity_with_context` returns an `Option` without a nom
        // remainder, so split the explicit rounding suffix first and recurse on
        // only the inner CDA grammar. With no suffix, keep the shared
        // parse_rounding_suffix default of Down.
        let rounded_inner = pair(
            take_until::<_, _, OracleError<'_>>(", round"),
            nom_quantity::parse_explicit_rounding_suffix,
        )
        .parse(after_divisor);
        let (inner_text, rounding) = match rounded_inner {
            Ok(("", (inner, rounding))) => (inner, rounding),
            _ => (after_divisor, RoundingMode::Down),
        };
        if let Some(inner) = parse_cda_quantity_with_context(inner_text.trim(), ctx) {
            return Some(QuantityExpr::DivideRounded {
                inner: Box::new(inner),
                divisor,
                rounding,
            });
        }
    }

    // "twice [inner]" or "three times [inner]" → Multiply { factor, inner }
    if let Ok((rest, factor)) = alt((
        value(2i32, tag::<_, _, OracleError<'_>>("twice ")),
        value(3, tag("three times ")),
    ))
    .parse(text)
    {
        if let Some(inner) = parse_cda_quantity_with_context(rest, ctx) {
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
            value(1i32, tag::<_, _, OracleError<'_>>(" plus ")),
            value(-1i32, tag(" minus ")),
        )),
    )
        .parse(text)
    {
        if let Some(inner) = parse_cda_quantity_with_context(rest, ctx) {
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

    // CR 208.1: "the difference between its power and toughness" — the
    // unsigned gap between an object's two current post-layer characteristics.
    // ("The difference between A and B" being unsigned is an Oracle templating
    // convention with no dedicated CR number; the resolver takes `.abs()`.)
    // Composed from `tag`s by axis (subject form ×
    // power/toughness ordering), emitting a general `QuantityExpr::Difference`
    // over existing `QuantityRef::Power`/`Toughness` leaves. Placed before the
    // generic `parse_quantity_ref` arm so the whole difference phrase is
    // recognized as a unit. Operand order is irrelevant — `Difference`
    // resolves to an absolute value — but both orderings are parsed so the
    // remainder is fully consumed.
    //
    // CR 115.10: the P/T refs are scoped to `ObjectScope::Recipient`. On a
    // trigger pump like Doran's ("Whenever a creature you control attacks or
    // blocks, it gets +X/+X … where X is the difference between its power and
    // toughness"), "its" anaphors back to the *affected* creature, not the
    // ability's own source — `Recipient` resolves to the first object target
    // (the pumped creature) and only falls back to the source when no target
    // is present (the CDA case), so a single scope is correct for every
    // parse path that lands a difference phrase.
    if let Ok((rest, (left_ref, right_ref))) = (
        tag::<_, _, OracleError<'_>>("the difference between "),
        alt((tag("its "), tag("~'s "), tag("this creature's "))),
        alt((
            value(
                (
                    QuantityRef::Power {
                        scope: ObjectScope::Recipient,
                    },
                    QuantityRef::Toughness {
                        scope: ObjectScope::Recipient,
                    },
                ),
                pair(tag("power and "), tag("toughness")),
            ),
            value(
                (
                    QuantityRef::Toughness {
                        scope: ObjectScope::Recipient,
                    },
                    QuantityRef::Power {
                        scope: ObjectScope::Recipient,
                    },
                ),
                pair(tag("toughness and "), tag("power")),
            ),
        )),
    )
        .parse(text)
        .map(|(rest, (_, _, refs))| (rest, refs))
    {
        if rest.is_empty() {
            return Some(QuantityExpr::Difference {
                left: Box::new(QuantityExpr::Ref { qty: left_ref }),
                right: Box::new(QuantityExpr::Ref { qty: right_ref }),
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

    if let Some(qty) = parse_milled_this_way_count(text) {
        return Some(QuantityExpr::Ref { qty });
    }

    if let Ok((rest, expr)) = parse_owned_cards_in_zones_with_property_filter(text) {
        if rest.is_empty() {
            return Some(expr);
        }
    }

    if let Ok((rest, expr)) = parse_owned_cards_in_zones_quantity(text) {
        if rest.is_empty() {
            return Some(expr);
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
                filter: None,
            },
        });
    }

    // "the number of noncreature spells they've cast this turn"
    // "the number of spells they've cast this turn"
    // "the number of spells you've cast this turn from anywhere other than your hand"
    // CR 400.1 + CR 601.2a: the shared helper locates the verb phrase
    // mid-clause (via take_until) so a trailing cast-origin qualifier survives;
    // "this turn" may already be stripped by strip_trailing_duration, so the bare
    // " they've cast" / " that player has cast" forms are also recognized.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("the number of ").parse(text) {
        if let Some((scope, filter)) = parse_spell_history_clause(rest, CountScope::Controller) {
            return Some(QuantityExpr::Ref {
                qty: QuantityRef::SpellsCastThisTurn { scope, filter },
            });
        }
    }

    // CR 107.x: Binary arithmetic over two dynamic quantities, e.g. "the number
    // of Caves you control plus the number of Cave cards in your graveyard"
    // (Calamitous Cave-In) or "the number of cards in their hand minus 4"
    // (Bant Charm class). Composes the existing Sum/Multiply/Offset variants
    // over recursively-parsed operands so the whole arithmetic class types
    // instead of falling through to an unresolved `Variable` (which resolves to
    // 0 at runtime — a silent no-op). Mirrors the leading-number "N plus inner"
    // arm above for the reversed operand order. Placed after the specific arms
    // and before the single-ref delegate; it only fires when the LEFT operand is
    // itself a dynamic quantity, so single refs and unparseable tails fall
    // through untouched. The runtime clamps a negative total to 0 (CR 107.1b),
    // matching the existing "N minus inner" handling.
    for (separator, negate) in [(" plus ", false), (" minus ", true)] {
        let Ok((_, (left_text, right_text))) = nom_primitives::split_once_on(text, separator)
        else {
            continue;
        };
        let Some(left) = parse_cda_quantity_with_context(left_text, ctx) else {
            continue;
        };
        // Right operand is either another dynamic quantity (→ Sum) or a bare
        // integer offset (→ Offset).
        if let Some(right) = parse_cda_quantity_with_context(right_text, ctx) {
            let right = if negate {
                QuantityExpr::Multiply {
                    factor: -1,
                    inner: Box::new(right),
                }
            } else {
                right
            };
            return Some(QuantityExpr::Sum {
                exprs: vec![left, right],
            });
        }
        if let Ok((rest, n)) = nom_primitives::parse_number(right_text.trim()) {
            if rest.trim().is_empty() {
                let offset = if negate { -(n as i32) } else { n as i32 };
                return Some(QuantityExpr::Offset {
                    inner: Box::new(left),
                    offset,
                });
            }
        }
    }

    // Delegate to existing parse_quantity_ref for patterns like
    // "the number of {type} you control", "your devotion to X"
    if let Some(qty) = parse_quantity_ref_with_context(text, ctx) {
        return Some(QuantityExpr::Ref { qty });
    }

    None
}

// CR 604.3: "the total number of cards you own in exile and in your graveyard
// that are Oozes or are named Slime Against Humanity" — type/name filters trail
// the zone list rather than preceding "cards".
fn parse_zone_card_that_are_filter_list(input: &str) -> OracleResult<'_, TargetFilter> {
    fn parse_named_card_filter(input: &str) -> OracleResult<'_, TargetFilter> {
        let (rest, _) = tag("named ").parse(input)?;
        let (rest, name) = alt((
            terminated(take_until(" or are "), peek(tag(" or are "))),
            terminated(take_until(" or "), peek(tag(" or "))),
            take_till1(|c| c == '.' || c == ','),
        ))
        .parse(rest)?;
        Ok((
            rest,
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Named {
                name: name.trim().to_string(),
            }])),
        ))
    }

    fn parse_type_card_filter(input: &str) -> OracleResult<'_, TargetFilter> {
        let (rest, filter) = nom_target::parse_type_filter_word(input)?;
        Ok((rest, TargetFilter::Typed(TypedFilter::new(filter))))
    }

    let (mut rest, first) = alt((parse_named_card_filter, parse_type_card_filter)).parse(input)?;
    let mut filters = vec![first];
    loop {
        let Ok((next_rest, _)) =
            alt((tag::<_, _, OracleError<'_>>(" or are "), tag(" or "))).parse(rest)
        else {
            break;
        };
        let (after, next) =
            alt((parse_named_card_filter, parse_type_card_filter)).parse(next_rest)?;
        filters.push(next);
        rest = after;
    }
    let filter = if filters.len() == 1 {
        filters.remove(0)
    } else {
        TargetFilter::Or { filters }
    };
    Ok((rest, filter))
}

fn parse_owned_cards_in_zones_with_property_filter(
    input: &str,
) -> nom::IResult<&str, QuantityExpr, OracleError<'_>> {
    let (rest, _) = alt((tag("the total number of "), tag("the number of "))).parse(input)?;
    let (rest, _) = alt((tag("cards"), tag("card"))).parse(rest)?;
    let (rest, _) = tag(" you own in ").parse(rest)?;
    let (rest, zones) = separated_list1(
        alt((tag(" and in "), tag(", and in "), tag(", in "))),
        preceded(opt(tag("your ")), nom_quantity::parse_zone_ref_singular),
    )
    .parse(rest)?;
    let (rest, _) = tag(" that are ").parse(rest)?;
    let (rest, filter) = parse_zone_card_that_are_filter_list(rest)?;
    let (rest, _) = opt(tag(".")).parse(rest)?;
    let (rest, _) = eof(rest)?;

    let mut exprs: Vec<QuantityExpr> = zones
        .into_iter()
        .map(|zone| QuantityExpr::Ref {
            qty: QuantityRef::ZoneCardCount {
                zone,
                card_types: Vec::new(),
                filter: Some(filter.clone()),
                scope: CountScope::Owner,
            },
        })
        .collect();

    let expr = if exprs.len() == 1 {
        exprs.remove(0)
    } else {
        QuantityExpr::Sum { exprs }
    };
    Ok((rest, expr))
}

// CR 604.3: Characteristic-defining abilities can define power/toughness using
// card-count quantities.
// CR 404.2: Cards in graveyards and exile are scoped by owner, not controller.
fn parse_owned_cards_in_zones_quantity(
    input: &str,
) -> nom::IResult<&str, QuantityExpr, OracleError<'_>> {
    let (rest, _) = alt((tag("the total number of "), tag("the number of "))).parse(input)?;
    let (rest, card_types) = nom_quantity::parse_type_filter_list(rest)?;
    let (rest, _) = nom_quantity::parse_card_word(rest)?;
    let (rest, _) = tag(" you own in ").parse(rest)?;
    let (rest, zones) = separated_list1(
        alt((tag(" and in "), tag(", and in "), tag(", in "))),
        preceded(opt(tag("your ")), nom_quantity::parse_zone_ref_singular),
    )
    .parse(rest)?;
    let (rest, _) = eof(rest)?;

    let mut exprs: Vec<QuantityExpr> = zones
        .into_iter()
        .map(|zone| QuantityExpr::Ref {
            qty: QuantityRef::ZoneCardCount {
                zone,
                card_types: card_types.clone(),
                scope: CountScope::Owner,
                filter: None,
            },
        })
        .collect();

    let expr = if exprs.len() == 1 {
        exprs.remove(0)
    } else {
        QuantityExpr::Sum { exprs }
    };
    Ok((rest, expr))
}

fn parse_previous_effect_amount_this_way(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    all_consuming(value(
        (),
        terminated(
            (
                opt(tag("the ")),
                alt((
                    parse_life_paid_or_lost_phrase,
                    parse_damage_dealt_phrase,
                    parse_dealt_damage_phrase,
                    parse_counters_removed_phrase,
                )),
            ),
            tag(" this way"),
        ),
    ))
    .parse(input)
}

fn parse_life_paid_or_lost_phrase(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    let (input, _) = opt(take_until("life ")).parse(input)?;
    let (input, _) = tag("life ").parse(input)?;
    let (input, _) = alt((tag("lost"), tag("paid"))).parse(input)?;
    Ok((input, ()))
}

fn parse_damage_dealt_phrase(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    let (input, _) = opt(take_until("damage dealt")).parse(input)?;
    let (input, _) = tag("damage dealt").parse(input)?;
    Ok((input, ()))
}

fn parse_dealt_damage_phrase(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    let (input, _) = opt(take_until("dealt damage")).parse(input)?;
    let (input, _) = tag("dealt damage").parse(input)?;
    Ok((input, ()))
}

fn parse_counters_removed_phrase(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    let (input, _) = opt(take_until("counter")).parse(input)?;
    let (input, _) = alt((tag("counters removed"), tag("counter removed"))).parse(input)?;
    Ok((input, ()))
}

/// CR 120.1 + CR 510.1 + CR 120.9 + CR 608.2i: Parse "[your] opponents who were
/// dealt combat damage [by <source>] [this turn]" into the optional source
/// filter. Returns `Ok((_, None))` for the unfiltered class (Tymna the Weaver,
/// Moonshae Pixie) and `Ok((_, Some(f)))` for a `by <source>` restriction
/// (Estinien Varlineau: "by ~ or a Dragon" → `Or[SelfRef, Typed{Dragon}]`).
/// `Err` means the clause did not match. The whole clause must be consumed
/// (explicit `eof`) so trailing unrecognized text doesn't silently drop.
fn parse_opponent_dealt_combat_damage_clause(
    input: &str,
) -> OracleResult<'_, Option<TargetFilter>> {
    let (input, _) = opt(tag("your ")).parse(input)?;
    let (input, _) = alt((tag("opponents"), tag("opponent"))).parse(input)?;
    let (input, _) = tag(" ").parse(input)?;
    let (input, _) = alt((tag("that"), tag("who"))).parse(input)?;
    let (input, _) = tag(" ").parse(input)?;
    let (input, _) = alt((tag("were"), tag("was"))).parse(input)?;
    let (input, _) = tag(" dealt combat damage").parse(input)?;
    // CR 120.9: optional "by <source>" restriction. The source phrase is
    // isolated from the optional trailing " this turn" with a combinator
    // (`take_until` / `eof`), then parsed via the `parse_target` + " or " +
    // `merge_or_filters` building block.
    let (input, source) = opt(preceded(tag(" by "), parse_damage_source_chain)).parse(input)?;
    let (input, _) = opt(tag(" this turn")).parse(input)?;
    let (input, _) = eof.parse(input)?;
    Ok((input, source))
}

/// CR 120.9 + CR 608.2i: Parse a damage-source phrase ("~", "a Dragon", "~ or a
/// Dragon") into a `TargetFilter`, composing chained "or"-separated subjects via
/// `parse_target` + `merge_or_filters`. The source phrase ends at the optional
/// trailing " this turn" or at end-of-input. Isolating the phrase with
/// `take_until`/`eof` (not `.split`/`.rfind`/`.contains`) lets `parse_target`
/// consume the whole subject without swallowing the duration suffix.
fn parse_damage_source_chain(input: &str) -> OracleResult<'_, TargetFilter> {
    // Isolate the source phrase: everything up to " this turn", or the whole
    // remainder if no duration suffix is present.
    let (rest, phrase) = alt((take_until(" this turn"), nom::combinator::rest)).parse(input)?;
    if phrase.is_empty() {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Tag,
        )));
    }
    let filter = parse_source_chain_phrase(phrase);
    Ok((rest, filter))
}

/// Recursively parse "X or Y or ..." over a fully-isolated source phrase using
/// `parse_target` (which maps "~" → `SelfRef` and "a Dragon" → `Typed{Dragon}`)
/// and `merge_or_filters` to fold the disjunction.
fn parse_source_chain_phrase(phrase: &str) -> TargetFilter {
    let (first, rest) = parse_target(phrase);
    let rest = rest.trim_start();
    if let Ok((after, _)) = tag::<_, _, OracleError<'_>>("or ").parse(rest) {
        let second = parse_source_chain_phrase(after);
        return merge_or_filters(first, second);
    }
    first
}

/// CR 508.6: "opponents you attacked [this turn]". Trailing " this turn"
/// optional (durations may be stripped upstream). No collision with "creature
/// you attacked WITH this turn" (`AttackedThisTurn`) — different subject word,
/// no " with".
fn parse_opponents_attacked_clause(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    all_consuming((
        alt((tag::<_, _, OracleError<'_>>("opponents"), tag("opponent"))),
        tag(" you attacked"),
        opt(tag(" this turn")),
    ))
    .parse(input)
    .map(|(rest, _)| (rest, ()))
}

/// CR 109.5: "opponent who [scalar predicate]" → `PlayerCount` over opponents
/// matching the per-candidate attribute threshold.
fn parse_for_each_opponent_player_attribute_clause(clause: &str) -> Option<QuantityRef> {
    let ((relation, attr, count), rest) = nom_on_lower(clause, clause, |input| {
        let (input, relation) = parse_player_population(input)?;
        let (input, (attr, count)) = alt((
            parse_cards_drawn_attr_clause,
            parse_battlefield_entries_attr_clause,
        ))
        .parse(input)?;
        Ok((input, (relation, attr, count)))
    })?;
    if !rest.is_empty() || relation != PlayerRelation::Opponent {
        return None;
    }
    Some(QuantityRef::PlayerCount {
        filter: PlayerFilter::PlayerAttribute {
            relation,
            attr: Box::new(attr),
            comparator: Comparator::GE,
            value: Box::new(QuantityExpr::Fixed { value: count }),
        },
    })
}

/// CR 402.1 / 119.1 / 122.1f / 404.1: Parse a player population whose scalar
/// attribute crosses a threshold, into `PlayerFilter::PlayerAttribute`. Reached
/// after `"the number of "` has been stripped.
///
/// Grammar (composed by prefix dispatch, not enumerated permutations):
///   `<population> <attr-clause>`
/// where `<population>` fixes the `PlayerRelation` and `<attr-clause>` is one of
/// the per-player-scalar shapes — currently:
///   - `who have <N> or more <kind> counters` → `PlayerCounter { kind }`
///     (Glissa's Retriever; `parse_player_counter_kind` covers poison / rad /
///     experience / ticket, so the whole counter class is handled, not one card).
///   - `with <N> or more cards in hand` → `HandSize` (Wolfcaller's Howl).
///
/// All shapes are `GE`-against-`Fixed(N)` ("N or more"). The embedded
/// `PlayerScope` / `CountScope` is inert at runtime (`candidate_player_scalar`
/// reads the candidate directly), so a neutral `ScopedPlayer` is emitted to
/// document "their" / "each player's own" semantics.
fn parse_player_attribute_predicate(input: &str) -> OracleResult<'_, PlayerFilter> {
    let (input, relation) = parse_player_population(input)?;
    let (input, (attr, count)) = alt((
        parse_player_counter_attr_clause,
        parse_hand_size_attr_clause,
        parse_cards_drawn_attr_clause,
        parse_battlefield_entries_attr_clause,
    ))
    .parse(input)?;
    Ok((
        input,
        PlayerFilter::PlayerAttribute {
            relation,
            attr: Box::new(attr),
            comparator: Comparator::GE,
            value: Box::new(QuantityExpr::Fixed { value: count }),
        },
    ))
}

/// CR 102.2 + CR 109.5: Population word fixing the `PlayerRelation`. The
/// optional `"your "` possessive and singular forms are accepted for the
/// grammatically-degenerate phrasings. "opponents"/"opponent" → Opponent;
/// "players"/"player" → All.
fn parse_player_population(input: &str) -> OracleResult<'_, PlayerRelation> {
    let (input, _) = opt(tag("your ")).parse(input)?;
    alt((
        value(PlayerRelation::Opponent, tag("opponents ")),
        value(PlayerRelation::Opponent, tag("opponent ")),
        value(PlayerRelation::All, tag("players ")),
        value(PlayerRelation::All, tag("player ")),
    ))
    .parse(input)
}

/// CR 122.1f + CR 122.1: "who have N or more <kind> counters" → the candidate's
/// named player-counter total. Delegates kind recognition to the shared
/// `parse_player_counter_kind` grammar so poison / rad / experience / ticket are
/// all covered.
fn parse_player_counter_attr_clause(input: &str) -> OracleResult<'_, (QuantityRef, i32)> {
    let (input, _) = tag("who have ").parse(input)?;
    let (input, n) = nom_primitives::parse_number(input)?;
    let (input, _) = tag(" or more ").parse(input)?;
    let (input, kind) = nom_quantity::parse_player_counter_kind(input)?;
    let (input, _) = tag(" counter").parse(input)?;
    let (input, _) = opt(tag("s")).parse(input)?;
    Ok((
        input,
        (
            QuantityRef::PlayerCounter {
                kind,
                scope: CountScope::ScopedPlayer,
            },
            n as i32,
        ),
    ))
}

/// CR 402.1: "with N or more cards in hand" → the candidate's hand size.
fn parse_hand_size_attr_clause(input: &str) -> OracleResult<'_, (QuantityRef, i32)> {
    let (input, _) = tag("with ").parse(input)?;
    let (input, n) = nom_primitives::parse_number(input)?;
    let (input, _) = tag(" or more cards in hand").parse(input)?;
    Ok((
        input,
        (
            QuantityRef::HandSize {
                player: PlayerScope::ScopedPlayer,
            },
            n as i32,
        ),
    ))
}

/// CR 121.1: "who drew N or more cards this turn" → the candidate's draw count.
fn parse_cards_drawn_attr_clause(input: &str) -> OracleResult<'_, (QuantityRef, i32)> {
    let (input, _) = tag("who drew ").parse(input)?;
    let (input, n) = nom_primitives::parse_number(input)?;
    let (input, _) = tag(" or more cards this turn").parse(input)?;
    Ok((
        input,
        (
            QuantityRef::CardsDrawnThisTurn {
                player: PlayerScope::ScopedPlayer,
            },
            n as i32,
        ),
    ))
}

/// CR 403.3: "who had N or more [type] enter the battlefield under their control
/// this turn" → battlefield-entry count for the candidate.
fn parse_battlefield_entries_attr_clause(input: &str) -> OracleResult<'_, (QuantityRef, i32)> {
    let (input, _) = tag("who had ").parse(input)?;
    let (input, n) = nom_primitives::parse_number(input)?;
    let (input, _) = tag(" or more ").parse(input)?;
    let (input, type_text) =
        take_until(" enter the battlefield under their control this turn").parse(input)?;
    let (input, _) = tag(" enter the battlefield under their control this turn").parse(input)?;
    let (filter, remainder) = parse_type_phrase(type_text.trim());
    if matches!(filter, TargetFilter::Any) || !remainder.trim().is_empty() {
        return Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((
        input,
        (
            QuantityRef::BattlefieldEntriesThisTurn {
                player: PlayerScope::ScopedPlayer,
                filter,
            },
            n as i32,
        ),
    ))
}

fn anaphoric_power_expr() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::Power {
            scope: ObjectScope::Anaphoric,
        },
    }
}

fn anaphoric_toughness_expr() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::Toughness {
            scope: ObjectScope::Anaphoric,
        },
    }
}

fn parse_anaphoric_power_or_toughness_property(
    input: &str,
) -> nom::IResult<&str, ObjectProperty, OracleError<'_>> {
    alt((
        value(ObjectProperty::Power, tag("its power")),
        value(ObjectProperty::Toughness, tag("its toughness")),
    ))
    .parse(input)
}

fn parse_anaphoric_power_toughness_sum(
    input: &str,
) -> nom::IResult<&str, QuantityExpr, OracleError<'_>> {
    let (rest, first) = parse_anaphoric_power_or_toughness_property(input)?;
    let (rest, _) = tag(" plus ").parse(rest)?;
    let (rest, second) = parse_anaphoric_power_or_toughness_property(rest)?;

    if matches!(
        (first, second),
        (ObjectProperty::Power, ObjectProperty::Toughness)
            | (ObjectProperty::Toughness, ObjectProperty::Power)
    ) {
        Ok((
            rest,
            QuantityExpr::Sum {
                exprs: vec![anaphoric_power_expr(), anaphoric_toughness_expr()],
            },
        ))
    } else {
        Err(nom::Err::Error(nom::error::Error::new(
            rest,
            nom::error::ErrorKind::Tag,
        )))
    }
}

/// Parse event-context quantity references from Oracle text fragments.
/// Returns None for unrecognized patterns (caller falls back to Variable).
pub(crate) fn parse_event_context_quantity(text: &str) -> Option<QuantityExpr> {
    let lower = text.to_lowercase();
    let lower = lower.trim();
    // CR 608.2c + CR 608.2h: "the X <verb>ed/<verb> this way" — numeric result from the
    // preceding effect (or trigger event) in the same resolution. Must check
    // before "that much" to avoid false match on "this way" vs. "this turn".
    // Verb-phrase combinators cover:
    //   - life-payment/loss: "life lost", "life paid"
    //   - combat-damage triggers: "damage dealt" (active voice),
    //     "dealt damage" (passive voice — e.g. Hordewing Skaab's
    //     "opponents dealt damage this way")
    //   - counter-removal chains: "counters removed", "counter removed"
    //     (Sensational Spider-Man's "stun counters removed this way";
    //     `state.last_effect_amount` is stamped by the preceding RemoveCounter).
    // PreviousEffectAmount reads `state.last_effect_amount`, which the
    // upstream effect (damage / counter removal / life loss) stamps.
    if parse_previous_effect_amount_this_way(lower).is_ok() {
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
        nom::combinator::opt(tag::<_, _, OracleError<'_>>("the ")),
        tag::<_, _, OracleError<'_>>("damage prevented this way"),
    ))
    .parse(lower)
    .is_ok()
    {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        });
    }

    if nom::combinator::all_consuming((
        tag::<_, _, OracleError<'_>>("the "),
        alt((tag("greatest "), tag("highest "))),
        tag("number of cards "),
        nom::combinator::opt(alt((tag("a player "), tag("any player ")))),
        tag("discarded this way"),
    ))
    .parse(lower)
    .is_ok()
    {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::PreviousEffectAmount,
        });
    }

    // CR 614.1a: "that much/many [noun] (plus|minus) N" — Offset over the
    // event-context amount. Composed from independent dimensions:
    //   - quantifier: "that much" | "that many"
    //   - noun (optional): " cards" | " life" | "" (bare quantifier)
    //   - sign: "plus" → +N | "minus" → -N
    //   - N: integer literal
    // Used by Heron of Hope / Angel of Vitality / Leyline of Hope / Pest
    // Rescuer ("you gain that much life plus 1 instead"); Honor Troll, Bilbo,
    // Knight of Dawn's Light, Cleric Class siblings; and the existing draw /
    // mill / scry "that many [cards] plus N" patterns.
    if let Ok((_, (_quantifier, _noun, sign, n))) = nom::combinator::all_consuming((
        alt((tag::<_, _, OracleError<'_>>("that much"), tag("that many"))),
        alt((
            tag::<_, _, OracleError<'_>>(" cards"),
            tag(" life"),
            tag(""),
        )),
        alt((
            value(1i32, tag::<_, _, OracleError<'_>>(" plus ")),
            value(-1i32, tag(" minus ")),
        )),
        nom_primitives::parse_number,
    ))
    .parse(lower)
    {
        return Some(QuantityExpr::Offset {
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            }),
            offset: sign * (n as i32),
        });
    }

    // CR 119.3 + CR 208.1: "its power plus its toughness" / "its toughness
    // plus its power" — sum of Anaphoric power and toughness refs. Both
    // operands use Anaphoric scope so the enclosing clause's subject-injection
    // and the runtime resolver apply identically to the individual "its power"
    // / "its toughness" single-value forms.
    if let Ok(("", expr)) = all_consuming(parse_anaphoric_power_toughness_sum).parse(lower) {
        return Some(expr);
    }

    match lower {
        // allow-noncombinator: dispatching on already-classified pre-trimmed phrase
        "that much" | "that many" | "that many cards" => {
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
        // CR 608.2k: bare anaphoric "its" — referent bound at parse time by the
        // enclosing clause's subject/target. Emits `Anaphoric` so context
        // remaps (subject-injection -> Source, "itself" -> Target) touch only
        // the pronoun, never an explicit possessive ("the sacrificed
        // creature's power" -> `CostPaidObject`, handled below).
        "its power" => {
            return Some(QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Anaphoric,
                },
            })
        }
        "its toughness" => {
            return Some(QuantityExpr::Ref {
                qty: QuantityRef::Toughness {
                    scope: ObjectScope::Anaphoric,
                },
            })
        }
        "its mana value" | "its converted mana cost" => {
            return Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Anaphoric,
                },
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

    // CR 603.7c: Decompose possessive noun phrases: "{referent}'s {property}".
    // The prefix classifier (`classify_possessive_referent`) picks the
    // ObjectScope per the prefix's grammatical role:
    //   - participle adjective + type ("the sacrificed creature's power",
    //     "the destroyed creature's power", "the revealed card's mana value")
    //     → `CostPaidObject` (CR 608.2k cost / trigger-condition referent).
    //   - bare demonstrative ("that card's mana value", "that creature's
    //     power", "that spell's mana value", "the creature's toughness") →
    //     `Demonstrative` (CR 608.2c earlier-instruction referent — Yuriko /
    //     Dark Confidant issue #511 class).
    // Neither the participle nor the demonstrative form is ever rewritten by
    // the subject-injection / "itself" remaps — unlike the bare pronoun "its"
    // arms above, which emit `Anaphoric` precisely so they can be remapped.
    if let Some((prefix, suffix)) = lower.split_once("'s ") {
        let suffix = suffix.trim();
        if let Some(scope) = classify_possessive_referent(prefix.trim()) {
            // CR 608.2k / 608.2c: the trailing property word maps to the
            // referenced object's characteristic. Nom `alt` over the property
            // keywords (longest-match first for "mana value" variants).
            let qty = alt((
                value(
                    QuantityRef::ObjectManaValue { scope },
                    alt((
                        tag::<_, _, OracleError<'_>>("mana value"),
                        tag("converted mana cost"),
                    )),
                ),
                value(QuantityRef::Power { scope }, tag("power")),
                value(QuantityRef::Toughness { scope }, tag("toughness")),
            ))
            .parse(suffix)
            .ok()
            .filter(|(rest, _): &(&str, QuantityRef)| rest.is_empty())
            .map(|(_, qty)| qty);
            if let Some(qty) = qty {
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
    let stripped = tag::<_, _, OracleError<'_>>("the ")
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
    let rest = tag::<_, _, OracleError<'_>>("the ")
        .parse(input)
        .map_or(input, |(r, _)| r);
    // Consume the core phrase. Accept both "mana you spent" and "mana spent".
    let rest = alt((
        value(
            (),
            tag::<_, _, OracleError<'_>>("amount of mana you spent to cast "),
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
                tag::<_, _, OracleError<'_>>("this spell"),
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

/// CR 603.7c: Classify the prefix of a `"<referent>'s <property>"` possessive
/// noun phrase and return the appropriate `ObjectScope` for the property's
/// owning object — or `None` if the prefix is not a recognized referent.
///
/// Two distinct classes share the same possessive grammar but differ in the CR
/// rule that licenses the reference and therefore in the runtime fallback order
/// (`game/quantity.rs`):
///
/// - **Participle adjective + type** ("the sacrificed creature", "the exiled
///   card", "the revealed creature") → [`ObjectScope::CostPaidObject`].
///   CR 608.2k authorizes references to "a specific untargeted object that has
///   been previously referred to by [the] ability's cost or trigger
///   condition." The participle names which earlier event introduced the
///   referent (sacrificed = cost, destroyed = trigger condition, etc.), so
///   slot 1 (`cost_paid_object`) is the canonical first slot, with the trigger
///   source and `effect_context_object` as later fallbacks. Greater Good
///   (issue #338) and the cost-referent class depend on this priority.
///
/// - **Bare demonstrative** ("that creature", "that card", "that spell", "the
///   creature") → [`ObjectScope::Demonstrative`]. CR 608.2c (the "follow
///   instructions in the order written / apply the rules of English to the
///   text" anaphora rule) makes the antecedent the *most recent earlier
///   effect instruction* in the same ability. The runtime `Demonstrative` arm
///   (shared with `Anaphoric`) inverts the slot order accordingly: slot 1 is
///   `effect_context_object` (the revealed / moved / effect-sacrificed
///   object), then the trigger source (CR 608.2k trigger-condition referent),
///   then `cost_paid_object`. This is the Yuriko, the Tiger's Shadow / Dark
///   Confidant class (issue #511): a reveal earlier in the same ability binds
///   "that card's" to the revealed card, not to the trigger source. The
///   dedicated variant (vs. the pronoun `Anaphoric`) is what keeps the
///   subject-injection rewrite from clobbering this fixed antecedent.
///
/// Picking the scope at parse time (rather than always emitting one or the
/// other) lets the runtime consult the right slot priority for each
/// grammatical form without per-card resolution rules.
fn classify_possessive_referent(prefix: &str) -> Option<ObjectScope> {
    // Consume the determiner ("that " or "the ") via nom `alt(tag(...))`.
    // Anything that doesn't begin with one of these determiners is not an
    // anaphoric possessive — return None.
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("that "),
        tag::<_, _, OracleError<'_>>("the "),
    ))
    .parse(prefix)
    .ok()?;

    // CR 608.2k: a participle-possessive adjective ("the destroyed creature",
    // "the revealed card") binds the referent to the cost-paid /
    // trigger-condition object. Each adjective names an earlier cost
    // (sacrifice/exile/discard) or trigger-condition (destroy, counter,
    // return, target, reveal, draw, copy) event in the same ability. The
    // adjective MUST be followed by a full object type phrase — otherwise
    // `"the targeted player"` would match the `targeted` participle even
    // though CR 608.2k object references do not apply to players.
    if all_consuming((
        parse_possessive_participle,
        tag(" "),
        parse_possessive_object_type,
    ))
    .parse(rest)
    .is_ok()
    {
        return Some(ObjectScope::CostPaidObject);
    }

    // CR 608.2c: bare demonstrative / definite possessive — "that <type>" /
    // "the <type>" with no participle adjective in between. The type word must
    // be the entire remainder (no trailing modifiers), which `all_consuming`
    // enforces. Emits `Demonstrative` (NOT the pronoun `Anaphoric`): the
    // antecedent is a full noun phrase fixed by the Oracle text, so the
    // subject-injection rewrite must never rebind it (Creature Bond, Erratic
    // Explosion). At runtime it resolves identically to `Anaphoric`.
    if nom::combinator::all_consuming(parse_possessive_object_type)
        .parse(rest)
        .is_ok()
    {
        return Some(ObjectScope::Demonstrative);
    }

    None
}

/// CR 608.2k: Recognize a participle adjective that names an earlier cost or
/// trigger-condition event in the same ability. The participle binds the
/// possessive referent to the cost-paid / event-condition object:
///
/// - cost participles: `sacrificed`, `exiled`, `discarded`, `milled`, `targeted`
/// - trigger-condition participles: `destroyed`, `countered`, `returned`,
///   `revealed`, `drawn`, `copied`, `discovered`
///
/// The combinator only consumes the participle word; callers MUST follow with
/// a whitespace boundary + an object type (via [`parse_possessive_object_type`])
/// to avoid matching prefixes like `"targeter"` or `"revealed cards face down"`.
fn parse_possessive_participle(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            // alt() has an arity limit; group cost vs. trigger-condition forms.
            alt((
                tag("sacrificed"),
                tag("exiled"),
                tag("discarded"),
                tag("milled"),
                tag("targeted"),
            )),
            alt((
                tag("destroyed"),
                tag("countered"),
                tag("returned"),
                tag("revealed"),
                tag("drawn"),
                tag("copied"),
                tag("discovered"),
            )),
        )),
    )
    .parse(input)
}

/// CR 603.7c / CR 205: Recognize the object-type phrase that follows the
/// determiner (and optional participle) in a possessive prefix.
///
/// Decomposes as `opt(supertype) + type_word`, reusing the shared
/// `oracle_nom::target::parse_supertype_prefix` building block (CR 205.4a
/// supertypes) for the optional adjective. This covers both bare single-word
/// forms (`"creature"`, `"artifact"`, `"permanent"`) and composed forms
/// (`"legendary creature"`, `"snow land"`, `"basic land"`) without
/// enumerating verbatim multi-word strings.
///
/// The bare type word is a *singular* card type or the `token` referent:
///
/// - Card types per CR 205.2/205.3: `creature`, `artifact`, `enchantment`,
///   `card`, `spell`, `permanent`, `planeswalker`, `land`, `battle`,
///   `instant`, `sorcery`.
/// - Token referent per CR 109.1 / CR 110.5: a non-card object that can still
///   anchor a possessive reference. Not a CR 205 type, so listed explicitly.
///
/// Plural forms (`creatures'`) are rejected — Oracle text possessives are
/// always singular (`the sacrificed creature's`). Plurals also cannot reach
/// this combinator through `parse_event_context_quantity` because the caller
/// splits on `"'s "` (apostrophe + s + space), and `creatures' power` has
/// `s' ` (no `'s ` substring) — but listing only singular forms here pins the
/// invariant at the parser layer, not the caller.
fn parse_possessive_object_type(input: &str) -> OracleResult<'_, ()> {
    // Optional supertype prefix consumes a trailing space.
    let (rest, _) = opt(nom_target::parse_supertype_prefix).parse(input)?;
    // Singular object-type words. Order matters where one is a prefix of
    // another: none of these share a prefix, so any order works.
    value(
        (),
        alt((
            tag("creature"),
            tag("artifact"),
            tag("enchantment"),
            tag("planeswalker"),
            tag("permanent"),
            tag("battle"),
            tag("instant"),
            tag("sorcery"),
            tag("land"),
            tag("spell"),
            tag("card"),
            tag("token"),
        )),
    )
    .parse(rest)
}

/// CR 400.7 + CR 608.2c: Match "<noun> exiled from <possessive> hand this way"
/// — used by Deadly Cover-Up's "draws a card for each card exiled from their
/// hand this way." Tries the `exiled from <possessive> hand` combinator at
/// every word boundary and returns `Some(())` on the first match.
fn try_parse_exiled_from_hand_this_way(lower: &str) -> Option<()> {
    crate::parser::oracle_nom::primitives::scan_at_word_boundaries(lower, |input| {
        let (rest, _) = tag::<_, _, OracleError<'_>>("exiled from ").parse(input)?;
        let (rest, _) = alt((
            value((), tag::<_, _, OracleError<'_>>("their hand")),
            value((), tag("your hand")),
            value((), tag("its owner's hand")),
            value((), tag("that player's hand")),
        ))
        .parse(rest)?;
        Ok((rest, ()))
    })
}

/// CR 608.2c + CR 122.1: Detect "counter[s] removed this way" — the for-each
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
/// Returns true when the filter carries information that can restrict the
/// tracked set beyond the default (all objects moved by the preceding effect).
/// Only `Any` is trivial; even a plain type/subtype filter can matter when the
/// parent effect moved a wider set.
fn filter_is_nontrivial_for_tracked_set(filter: &crate::types::ability::TargetFilter) -> bool {
    !matches!(filter, crate::types::ability::TargetFilter::Any)
}

/// CR 608.2c + CR 614.6: Try to parse a "for each <filter> [that was/were]
/// <verb> this way" clause, where `<verb>` is one of the producer keyword
/// actions. Returns the optional restricting filter (`None` = trivial → fall
/// through to plain `TrackedSetSize`) PAIRED with the producer action the verb
/// names (`caused_by`):
///
///   - destroyed this way → `Destroyed` (CR 701.8a).
///   - sacrificed this way → `Sacrificed` (CR 701.21a).
///   - milled this way → `Milled` (CR 701.17a).
///   - discarded this way → `Discarded` (CR 701.9a).
///   - exiled this way → `Exiled` (CR 701.13a).
///
/// Uses `terminated(take_until(suffix), tag(suffix))` to split at each
/// recognized suffix, then delegates the prefix to `parse_type_phrase`. The
/// `caused_by` action lets the resulting `FilteredTrackedSetSize` count only the
/// members the matching verb produced within a merged chain set — disjoint from
/// same-destination actions and stable under replacement redirection (#2932).
fn parse_destroyed_or_sacrificed_this_way_filter(
    lower: &str,
) -> Option<(Option<TargetFilter>, ThisWayCause)> {
    // Each (suffix, producer action) is tried in order; the first complete match
    // wins. Longer/more-specific suffixes precede shorter ones so "that was
    // destroyed this way" is preferred over "destroyed this way".
    let suffixes: &[(&str, ThisWayCause)] = &[
        (" that was destroyed this way", ThisWayCause::Destroyed),
        (" that were destroyed this way", ThisWayCause::Destroyed),
        (" destroyed this way", ThisWayCause::Destroyed),
        ("destroyed this way", ThisWayCause::Destroyed),
        (" that was sacrificed this way", ThisWayCause::Sacrificed),
        (" that were sacrificed this way", ThisWayCause::Sacrificed),
        (" sacrificed this way", ThisWayCause::Sacrificed),
        ("sacrificed this way", ThisWayCause::Sacrificed),
        (" that was milled this way", ThisWayCause::Milled),
        (" that were milled this way", ThisWayCause::Milled),
        (" milled this way", ThisWayCause::Milled),
        ("milled this way", ThisWayCause::Milled),
        (" that was discarded this way", ThisWayCause::Discarded),
        (" that were discarded this way", ThisWayCause::Discarded),
        (" discarded this way", ThisWayCause::Discarded),
        ("discarded this way", ThisWayCause::Discarded),
        (" that was exiled this way", ThisWayCause::Exiled),
        (" that were exiled this way", ThisWayCause::Exiled),
        (" exiled this way", ThisWayCause::Exiled),
        ("exiled this way", ThisWayCause::Exiled),
    ];
    for &(suffix, cause) in suffixes {
        // terminated(take_until(suffix), tag(suffix)) parses the noun-phrase
        // prefix, then consumes the suffix exactly, leaving an empty remainder.
        let result: OracleResult<'_, &str> =
            terminated(take_until(suffix), tag(suffix)).parse(lower);
        if let Ok(("", filter_phrase)) = result {
            // CR 700.1: "card" is the zone-agnostic head noun for the discarded
            // members ("nonland card discarded this way"). parse_type_phrase maps
            // it to TypeFilter::Card (matches every card type), so "nonland card"
            // yields [Card, Non(Land)] — counting nonland instants/sorceries too.
            // It must NOT be narrowed to TypeFilter::Permanent: a nonland instant
            // discarded by Seasoned Pyromancer is still counted (CR 701.9a).
            let (filter, remainder) =
                crate::parser::oracle_target::parse_type_phrase(filter_phrase.trim());
            if remainder.trim().is_empty() {
                return Some((Some(filter), cause));
            }
            // A suffix matched but the filter is trivial or the phrase
            // didn't fully consume — fall through to TrackedSetSize.
            return Some((None, cause));
        }
    }
    None
}

fn parse_filtered_destroyed_this_way(lower: &str) -> Option<QuantityRef> {
    match parse_destroyed_or_sacrificed_this_way_filter(lower)? {
        (Some(filter), cause) if filter_is_nontrivial_for_tracked_set(&filter) => {
            Some(QuantityRef::FilteredTrackedSetSize {
                filter: Box::new(filter),
                caused_by: Some(cause),
            })
        }
        _ => None,
    }
}

fn parse_destroyed_or_sacrificed_this_way_quantity(lower: &str) -> Option<QuantityRef> {
    match parse_destroyed_or_sacrificed_this_way_filter(lower)? {
        (Some(filter), cause) if filter_is_nontrivial_for_tracked_set(&filter) => {
            Some(QuantityRef::FilteredTrackedSetSize {
                filter: Box::new(filter),
                caused_by: Some(cause),
            })
        }
        // CR 608.2c: a bare "<verb> this way" with no restricting filter counts
        // the whole tracked set; `TrackedSetSize` reads it id-only (no action
        // discrimination), matching legacy behavior.
        _ => Some(QuantityRef::TrackedSetSize),
    }
}

fn try_parse_counters_removed_this_way(lower: &str) -> bool {
    crate::parser::oracle_nom::primitives::scan_at_word_boundaries(lower, |input| {
        let (rest, _) = alt((
            value((), tag::<_, _, OracleError<'_>>("counters")),
            value((), tag::<_, _, OracleError<'_>>("counter")),
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
    parse_for_each_clause_expr_with_parser(clause, parse_for_each_clause)
}

pub(crate) fn parse_for_each_clause_expr_with_context(
    clause: &str,
    ctx: &ParseContext,
) -> Option<QuantityExpr> {
    parse_for_each_clause_expr_with_parser(clause, |segment| {
        parse_for_each_clause_with_context(segment, ctx)
    })
}

fn parse_for_each_clause_expr_with_parser(
    clause: &str,
    parse_clause: impl Fn(&str) -> Option<QuantityRef> + Copy,
) -> Option<QuantityExpr> {
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

    if let Some((rest, expr)) =
        parse_for_each_beyond_first_clause_expr_with_parser(clause, parse_clause)
    {
        return rest.is_empty().then_some(expr);
    }

    fn segment(i: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
        alt((take_until(" and each "), rest)).parse(i)
    }
    let mut split = separated_list1(tag::<_, _, OracleError<'_>>(" and each "), segment);
    let segments: Vec<&str> = split
        .parse(clause)
        .map(|(_, v)| v)
        .unwrap_or_else(|_| vec![clause]);

    let refs: Option<Vec<QuantityRef>> = segments.iter().map(|s| parse_clause(s.trim())).collect();
    let mut exprs: Vec<QuantityExpr> = refs?
        .into_iter()
        .map(|qty| QuantityExpr::Ref { qty })
        .collect();
    if exprs.len() == 1 {
        return exprs.pop();
    }
    Some(QuantityExpr::Sum { exprs })
}

/// CR 702.23a + CR 107.1b: "for each [object] beyond the first" composes a
/// normal object-count quantity with an offset of -1, clamped at zero. This
/// preserves the shared `for each` grammar and keeps "beyond the first" as an
/// expression modifier rather than adding a leaf-level `QuantityRef` variant.
fn parse_for_each_beyond_first_clause_expr_with_parser(
    input: &str,
    parse_clause: impl Fn(&str) -> Option<QuantityRef>,
) -> Option<(&str, QuantityExpr)> {
    let (input, base_clause) = terminated::<_, _, OracleError<'_>, _, _>(
        take_until(" beyond the first"),
        tag(" beyond the first"),
    )
    .parse(input)
    .ok()?;
    let qty = parse_clause(base_clause)?;
    let count_minus_one = QuantityExpr::Offset {
        inner: Box::new(QuantityExpr::Ref { qty }),
        offset: -1,
    };
    Some((
        input,
        QuantityExpr::ClampMin {
            inner: Box::new(count_minus_one),
            minimum: 0,
        },
    ))
}

/// CR 109.4 + CR 400.1 + CR 608.2c: Parse an anaphoric hand-card union like
/// "Mountain and red card in it" after "target opponent reveals their hand".
/// The pronoun "it" refers to the targeted player's hand; the two filter atoms
/// are a disjunction, so a red Mountain is counted once.
fn parse_target_hand_type_or_color_clause(
    input: &str,
) -> nom::IResult<&str, QuantityExpr, OracleError<'_>> {
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

/// CR 608.2c + CR 109.5: Recognize "[population] who [verb]ed [this way]" as a
/// player-action quantity, returning the player relation and the action that
/// keys the runtime accumulator. The runtime accumulator is keyed by
/// `GameEvent::PlayerPerformedAction`, not by zone changes, so it still counts
/// a player who performed the action without an observable board result (e.g.
/// searched and failed to find).
///
/// Nesting: the population word ("opponent(s)"/"player(s)") fixes the relation,
/// then the shared `"who "` prefix dispatches on the verb arm. The search arm
/// carries an object-noun ("searched their library"); the investigate arm is
/// object-less ("investigated"). Composed entirely from `alt`/`value`/`tag` —
/// no permutation enumeration.
fn parse_action_this_way(
    input: &str,
) -> nom::IResult<&str, (PlayerRelation, PlayerActionKind), OracleError<'_>> {
    let (input, relation) = alt((
        value(PlayerRelation::Opponent, tag("opponents ")),
        value(PlayerRelation::Opponent, tag("opponent ")),
        value(PlayerRelation::All, tag("players ")),
        value(PlayerRelation::All, tag("player ")),
    ))
    .parse(input)?;
    let (input, _) = tag("who ").parse(input)?;
    let (input, action) = alt((parse_searched_arm, parse_investigated_arm)).parse(input)?;
    let (input, _) = tag(" this way").parse(input)?;
    Ok((input, (relation, action)))
}

/// "searches/searched a/their library" → `SearchedLibrary` (Tempting Offer cycle).
fn parse_searched_arm(input: &str) -> nom::IResult<&str, PlayerActionKind, OracleError<'_>> {
    let (input, _) = alt((tag("searches"), tag("searched"))).parse(input)?;
    let (input, _) = tag(" ").parse(input)?;
    let (input, _) = alt((tag("a "), tag("their "))).parse(input)?;
    let (input, _) = tag("library").parse(input)?;
    Ok((input, PlayerActionKind::SearchedLibrary))
}

/// "investigates/investigated" → `Investigate` (Wernog, Rider's Chaplain).
fn parse_investigated_arm(input: &str) -> nom::IResult<&str, PlayerActionKind, OracleError<'_>> {
    value(
        PlayerActionKind::Investigate,
        alt((tag("investigates"), tag("investigated"))),
    )
    .parse(input)
}

/// "opponent who does" / "players who do" → accepted the optional offer.
fn parse_optional_offer_accepted_clause(
    input: &str,
) -> nom::IResult<&str, (PlayerRelation, PlayerActionKind), OracleError<'_>> {
    let (input, relation) = alt((
        value(PlayerRelation::Opponent, tag("opponents ")),
        value(PlayerRelation::Opponent, tag("opponent ")),
        value(PlayerRelation::All, tag("players ")),
        value(PlayerRelation::All, tag("player ")),
    ))
    .parse(input)?;
    let (input, _) = tag("who ").parse(input)?;
    let (input, _) = alt((tag("does"), tag("do"), tag("did"))).parse(input)?;
    Ok((input, (relation, PlayerActionKind::AcceptedOptionalEffect)))
}

/// Parse the clause after "for each" into a QuantityRef.
/// CR 702.62b: A suspended card is a card in the exile zone with the suspend
/// keyword and a time counter on it. Counting clauses (`for each suspended card
/// you own`) compose those observable axes with the ownership qualifier.
///
/// Composes existing `FilterProp`s (`InZone`/`HasKeywordKind`/`Owned`/`Counters`)
/// into a typed card filter — never a one-off `Suspended` tag or a verbatim
/// string match.
fn parse_suspended_card_clause(clause: &str) -> Option<QuantityRef> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("suspended ")
        .parse(clause)
        .ok()?;
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("cards"),
        tag::<_, _, OracleError<'_>>("card"),
    ))
    .parse(rest)
    .ok()?;
    // CR 108.3: only the "you own" ownership form is exercised by suspended-card
    // count clauses (the cards are exiled face up under their owner).
    let (rest, _) = preceded(tag::<_, _, OracleError<'_>>(" "), tag("you own"))
        .parse(rest)
        .ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    use crate::types::counter::{CounterMatch, CounterType};
    Some(QuantityRef::ObjectCount {
        filter: TargetFilter::Typed(TypedFilter::card().properties(vec![
            // CR 400.1: in the exile zone.
            FilterProp::InZone { zone: Zone::Exile },
            // CR 702.62b: has suspend.
            FilterProp::HasKeywordKind {
                value: KeywordKind::Suspend,
            },
            // CR 108.3: owned by the ability's controller.
            FilterProp::Owned {
                controller: ControllerRef::You,
            },
            // CR 702.62b: bears at least one time counter.
            FilterProp::Counters {
                counters: CounterMatch::OfType(CounterType::Time),
                comparator: Comparator::GE,
                count: QuantityExpr::Fixed { value: 1 },
            },
        ])),
    })
}

/// CR 400.1 + CR 601.2a: Parse a spell-history count clause into its
/// controller scope and an optional characteristic/cast-origin filter.
///
/// Single authority shared by both the `for each <spell> you've cast this turn …`
/// arm and the `the number of <spell> you've cast this turn …` (CDA) arm. The
/// verb phrase (`you've cast this turn`, `they've cast`, …) can appear *mid-clause*
/// when a cast-origin qualifier follows (`spell you've cast this turn from
/// anywhere other than your hand`), so it is located with `take_until` rather
/// than `strip_suffix`. The noun before the verb and the `from …` tail after it
/// are reattached into the verb-phrase-free noun phrase
/// (`spell from anywhere other than your hand`) that `parse_spell_history_filter`
/// accepts, which yields the `FilterProp::InAnyZone` cast-origin filter.
///
/// Returns `None` only when no verb phrase matches (so callers fall through to
/// other arms). `default_controller` supplies the scope for the `you`/`you've`
/// forms; `they`/`that player` forms always resolve to `CountScope::Controller`
/// per the existing arms' semantics.
fn parse_spell_history_clause(
    clause: &str,
    default_controller: CountScope,
) -> Option<(CountScope, Option<TargetFilter>)> {
    // Verb-phrase alternates, longest-first so "… this turn" wins over the bare
    // form. Each is a typed (tag, scope) separator, not a verbatim whole-clause
    // match — the noun and `from …` tail around it are captured slices.
    let verb_phrases: [(&str, CountScope); 8] = [
        (" they've cast this turn", CountScope::Controller),
        (" that player has cast this turn", CountScope::Controller),
        (" you've cast this turn", default_controller.clone()),
        (" you cast this turn", default_controller.clone()),
        (" they've cast", CountScope::Controller),
        (" that player has cast", CountScope::Controller),
        (" you've cast", default_controller.clone()),
        (" you cast", default_controller.clone()),
    ];

    for (verb_phrase, scope) in verb_phrases {
        let Ok((after, noun)) = take_until::<_, _, OracleError<'_>>(verb_phrase).parse(clause)
        else {
            continue;
        };
        let Ok((tail, _)) = tag::<_, _, OracleError<'_>>(verb_phrase).parse(after) else {
            continue;
        };
        let noun = noun.trim();
        let tail = tail.trim();

        // Reconstruct the verb-phrase-free noun phrase: noun + trailing `from …`
        // qualifier. For "spell" + "from anywhere other than your hand" this is
        // exactly the string `parse_spell_history_filter` consumes.
        let noun_phrase = if tail.is_empty() {
            noun.to_string()
        } else {
            format!("{noun} {tail}")
        };

        // Bare spell/time forms with no qualifier keep the historical
        // `filter: None` behavior.
        let bare = matches!(noun, "spells" | "spell" | "time" | "") && tail.is_empty();
        if bare {
            return Some((scope, None));
        }

        // Cast-origin / type / color qualifiers route through the shared
        // spell-history filter grammar (which emits FilterProp::InAnyZone for
        // "from anywhere other than …").
        if let Some(filter) = parse_spell_history_filter(&noun_phrase) {
            return Some((scope, Some(filter)));
        }

        // A non-empty `tail` means trailing text followed the verb phrase
        // (`… you've cast this turn <tail>`). The only `tail` shape this helper
        // recognizes is the cast-origin qualifier consumed by
        // `parse_spell_history_filter` above. If that did not match, the tail is
        // unrelated trailing text (e.g. a compound `… and <something else>`
        // clause): return `None` so later arms can try, rather than swallowing
        // the tail and mis-quantifying as `filter: None`. This restores the
        // suffix-anchored original's behavior — which would not have matched a
        // mid-clause verb phrase at all — now that the verb phrase is located
        // with `take_until`.
        if !tail.is_empty() {
            return None;
        }

        // Fallback for type-only phrasings the spell-history grammar may reject
        // (e.g. "instant", "noncreature spell"): strip the trailing spell noun and
        // run the context-free type-phrase parser. Cast-origin clauses never
        // reach here (they parse above), so this only recovers legacy type forms.
        // Strip the trailing spell noun via nom (mirrors
        // `strip_spell_history_noun`) before the context-free type-phrase
        // parse: "noncreature spell" → "noncreature", "instant" unchanged.
        let qualifier = terminated(
            take_until::<_, _, OracleError<'_>>(" spell"),
            alt((tag(" spells"), tag(" spell"))),
        )
        .parse(noun)
        .map(|(_, before)| before.trim())
        .unwrap_or(noun);
        let (filter, remainder) = parse_type_phrase(qualifier);
        if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
            return Some((scope, Some(filter)));
        }

        // Suffix-anchored noun with no recognized qualifier (e.g. an unknown
        // spell noun that ended the clause): mirror the original arms' contract
        // of returning the spell-history scope with no filter.
        return Some((scope, None));
    }

    None
}

pub(crate) fn parse_for_each_clause(clause: &str) -> Option<QuantityRef> {
    parse_for_each_clause_with_they_controller(
        clause,
        ControllerRef::ScopedPlayer,
        &ParseContext::default(),
    )
}

pub(crate) fn parse_for_each_clause_with_context(
    clause: &str,
    ctx: &ParseContext,
) -> Option<QuantityRef> {
    let they_controller = ctx
        .third_person_player_controller_ref()
        .unwrap_or(ControllerRef::ScopedPlayer);
    parse_for_each_clause_with_they_controller(clause, they_controller, ctx)
}

fn parse_for_each_clause_with_they_controller(
    clause: &str,
    they_controller: ControllerRef,
    ctx: &ParseContext,
) -> Option<QuantityRef> {
    let clause = clause.trim().trim_end_matches('.');

    if let Some(qty) = parse_for_each_kicker_count(clause) {
        return Some(qty);
    }

    if let Ok((rest, qty)) = nom_quantity::parse_for_each_clause_ref_with_context(
        clause,
        &for_each_anaphor_context(ctx, &they_controller),
    ) {
        if rest.is_empty() {
            return Some(qty);
        }
    }

    // CR 406.6 + CR 607.1 + CR 614.1c: "[type phrase] card(s) exiled with it/~"
    // -- a count of the linked-exile set (cards exiled with this source, e.g.
    // via Delve) restricted to a type phrase. Murktide Regent's ETB counter
    // "for each instant and sorcery card exiled with it". `ExiledBySource`
    // reports `extract_in_zone() == Exile`, so the resulting `ObjectCount` scans
    // the exile zone and `matches_target_filter` intersects the type phrase with
    // the linked-exile set. Runs after the `nom_quantity` attempt so the bare
    // "card exiled with it" form keeps its existing `CardsExiledBySource` lower.
    for exiled_suffix in [" exiled with it", " exiled with ~"] {
        if let Ok((after, prefix)) = terminated(
            take_until::<_, _, OracleError<'_>>(exiled_suffix),
            tag::<_, _, OracleError<'_>>(exiled_suffix),
        )
        .parse(clause)
        {
            if after.trim().is_empty() {
                let (type_filter, type_rest) = parse_type_phrase(prefix);
                if type_rest.trim().is_empty() && !matches!(type_filter, TargetFilter::Any) {
                    return Some(QuantityRef::ObjectCount {
                        filter: TargetFilter::And {
                            filters: vec![type_filter, TargetFilter::ExiledBySource],
                        },
                    });
                }
            }
        }
    }

    // CR 106.1 + CR 109.1: "color among [type-phrase]" — distinct colors among
    // matching objects. Used by Faeburrow Elder's "+1/+1 for each color among
    // permanents you control" and by the Converge mechanic adjacent class.
    if let Ok((after_among, _)) = tag::<_, _, OracleError<'_>>("color among ").parse(clause) {
        let (filter, remainder) = parse_type_phrase(after_among);
        if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
            return Some(QuantityRef::DistinctColorsAmongPermanents { filter });
        }
    }

    if let Ok((rest, (relation, action))) = parse_optional_offer_accepted_clause(clause) {
        if rest.is_empty() {
            return Some(QuantityRef::PlayerCount {
                filter: PlayerFilter::PerformedActionThisWay { relation, action },
            });
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
        // CR 608.2c + CR 109.5: "[population] who [verb]ed … this way" — the
        // Tempting Offer cycle's bonus-tutor-per-accepting-opponent step and
        // Wernog's bonus-investigate-per-investigating-opponent step. A single
        // verb-dispatched combinator handles every (population × verb tense ×
        // article) permutation, returning a player-count quantity rather than
        // the object-count `TrackedSetSize` fallback below. Must be tried before
        // that fallback because every "[population] who … this way" clause does
        // contain "this way".
        if let Ok((rest, (relation, action))) = parse_action_this_way(lower.as_str()) {
            if rest.is_empty() {
                return Some(QuantityRef::PlayerCount {
                    filter: PlayerFilter::PerformedActionThisWay { relation, action },
                });
            }
        }
        // CR 608.2c + CR 122.1: "[counter-type] counter[s] removed this way" — the
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
        // CR 608.2c + CR 400.7: "nontoken creature you controlled that was
        // destroyed this way" — tracked set members matching the filter prefix.
        // Use terminated(take_until(suffix), tag(suffix)) to split the clause
        // at each recognized suffix and parse the prefix as a type filter.
        // Only emit `FilteredTrackedSetSize` when the filter restricts the
        // tracked set. Bare "destroyed this way" still falls through to the
        // unfiltered `TrackedSetSize`.
        if let Some(qty) = parse_filtered_destroyed_this_way(&lower) {
            return Some(qty);
        }
        // CR 608.2c + CR 205.2a: "card type[s] among cards <verb> this way" —
        // distinct card types among the cause-filtered chain tracked set (Occult
        // Epiphany #3307). Must precede the bare `TrackedSetSize` fallback.
        if let Ok(("", qty)) =
            crate::parser::oracle_nom::quantity::parse_distinct_card_types_among_tracked_set(&lower)
        {
            return Some(qty);
        }
        return Some(QuantityRef::TrackedSetSize);
    }

    // "opponent who lost life this turn"
    if clause.contains("opponent") && clause.contains("lost life") {
        return Some(QuantityRef::PlayerCount {
            filter: PlayerFilter::OpponentLostLife,
        });
    }

    // CR 121.1 / CR 403.3: "opponent who drew N or more cards this turn" and
    // "opponent who had N or more [type] enter the battlefield under their
    // control this turn" (Smuggler's Share class).
    if let Some(qty) = parse_for_each_opponent_player_attribute_clause(clause) {
        return Some(qty);
    }

    // "opponent who gained life this turn"
    if clause.contains("opponent") && clause.contains("gained life") {
        return Some(QuantityRef::PlayerCount {
            filter: PlayerFilter::OpponentGainedLife,
        });
    }

    // CR 104.3: "player(s) who have/has lost the game" (Rampant Frogantua).
    if let Some(((), rest)) = nom_on_lower(clause, clause, |i| {
        value(
            (),
            (
                alt((tag("player "), tag("players "))),
                alt((tag("who "), tag("that "))),
                alt((tag("has "), tag("have "))),
                tag("lost the game"),
            ),
        )
        .parse(i)
    }) {
        if rest.is_empty() {
            return Some(QuantityRef::PlayerCount {
                filter: PlayerFilter::HasLostTheGame,
            });
        }
    }

    // CR 106.4: "unspent [color] mana you have" (Omnath, Locus of Mana) — the
    // amount of floating mana of that color (or any color) in the controller's
    // pool. A bare color word is optional, so "unspent mana you have" counts
    // all colors.
    if let Some((color, rest)) = nom_on_lower(clause, clause, |i| {
        let (i, _) = tag::<_, _, OracleError<'_>>("unspent ").parse(i)?;
        let (i, color) = opt(nom_primitives::parse_color).parse(i)?;
        let (i, _) = opt(tag::<_, _, OracleError<'_>>(" ")).parse(i)?;
        let (i, _) = tag::<_, _, OracleError<'_>>("mana you have").parse(i)?;
        Ok((i, color))
    }) {
        if rest.trim().is_empty() {
            return Some(QuantityRef::UnspentMana { color });
        }
    }

    // CR 120.1 + CR 510.1: "opponent that was dealt combat damage this turn"
    // / "opponent who was dealt combat damage this turn". Mirrors the
    // lost-life / gained-life arms above, but consumes the full clause instead
    // of doing substring dispatch.
    if let Ok((_, source)) = parse_opponent_dealt_combat_damage_clause(clause) {
        return Some(QuantityRef::PlayerCount {
            filter: PlayerFilter::OpponentDealtCombatDamage {
                source: source.map(Box::new),
            },
        });
    }

    // CR 508.6: "opponent you attacked this turn".
    if parse_opponents_attacked_clause(clause).is_ok() {
        return Some(QuantityRef::PlayerCount {
            filter: PlayerFilter::OpponentAttacked {
                subject: AttackSubject::You,
                scope: AttackScope::ThisTurn,
            },
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
    // type. The filter is the type-phrase plus a
    // `FilterProp::Counters { OfType(t), GE, Fixed(1) }`.
    // This must be checked BEFORE the self-counter fallback below, which would
    // otherwise misroute any clause containing "counter on" to CountersOnSelf and
    // discard the subject type phrase (Inspiring Call bug: "creature you control
    // with a +1/+1 counter on it" → CountersOnSelf{ "creature you control with a +1/+1" }).
    if let Ok((_, type_part)) = take_until::<_, _, OracleError<'_>>(" with ").parse(clause) {
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

    // Delegate to the nom for-each clause parser for patterns it covers
    // (e.g. "counter on this equipment" — any-counter source form).
    if let Ok((rest, qty)) = nom_quantity::parse_for_each_clause_ref.parse(clause) {
        if rest.is_empty() {
            return Some(qty);
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

    // "spell you've cast this turn" / "spells you've cast this turn" /
    // "spell you've cast this turn from anywhere other than your hand".
    // Direct dispatch before type-phrase fallback to handle spell-casting quantity
    // patterns; the shared helper locates the verb phrase mid-clause so a trailing
    // cast-origin qualifier survives. CR 400.1 + CR 601.2a.
    if let Some((scope, filter)) = parse_spell_history_clause(clause, CountScope::Controller) {
        return Some(QuantityRef::SpellsCastThisTurn { scope, filter });
    }

    if let Some(qty) = parse_suspended_card_clause(clause) {
        return Some(qty);
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

    if let Ok((rest, _)) = terminated(
        alt((tag::<_, _, OracleError<'_>>("creature"), tag("creatures"))),
        alt((
            tag(" you attacked with this turn"),
            tag(" you attacked with"),
        )),
    )
    .parse(clause)
    {
        if rest.is_empty() {
            return Some(QuantityRef::AttackedThisTurn {
                scope: CountScope::Controller,
                filter: None,
            });
        }
    }

    // "creature you control", "artifact you control", etc.
    // Use parse_type_phrase_with_ctx (not parse_target) to avoid generating
    // spurious target-fallback warnings for quantity text that isn't a target
    // clause.
    //
    // CR 109.5 + CR 109.4: thread the relative player scope so "they control"
    // binds to the iterating/targeted/chosen player rather than collapsing to the
    // caster. "you control" still resolves to ControllerRef::You inside
    // parse_type_phrase_with_ctx (its suffix arm is ctx-independent), so The Scarab
    // God and other caster-relative counts are unchanged. CR 608.2c: the controller
    // follows instructions in order, so a per-player-scoped count reads the
    // iterating player.
    let mut tp_ctx = for_each_anaphor_context(ctx, &they_controller);
    let (filter, remainder) = parse_type_phrase_with_ctx(clause, &mut tp_ctx);
    if !matches!(filter, TargetFilter::Any) && remainder.trim().is_empty() {
        return Some(QuantityRef::ObjectCount { filter });
    }

    None
}

fn for_each_anaphor_context(ctx: &ParseContext, they_controller: &ControllerRef) -> ParseContext {
    ParseContext {
        relative_player_scope: Some(they_controller.clone()),
        subject: ctx.subject.clone(),
        card_name: ctx.card_name.clone(),
        host_self_reference: ctx.host_self_reference.clone(),
        current_trigger_index: ctx.current_trigger_index,
        ..Default::default()
    }
}

/// CR 608.2c: Parse the object set named by a "for each [object]"
/// clause when the following instruction acts on each object itself rather than
/// only needing the count. This preserves object identity for patterns such as
/// "for each token you control that entered this turn, create a token that's a
/// copy of it".
pub(crate) fn parse_for_each_object_filter_clause(clause: &str) -> Option<TargetFilter> {
    match parse_for_each_clause(clause)? {
        QuantityRef::ObjectCount { filter } => Some(filter),
        QuantityRef::EnteredThisTurn { filter } => {
            Some(add_filter_property(filter, FilterProp::EnteredThisTurn))
        }
        _ => None,
    }
}

fn add_filter_property(filter: TargetFilter, property: FilterProp) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            if !typed
                .properties
                .iter()
                .any(|existing| existing == &property)
            {
                typed.properties.push(property);
            }
            TargetFilter::Typed(typed)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|filter| add_filter_property(filter, property.clone()))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|filter| add_filter_property(filter, property.clone()))
                .collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(add_filter_property(*filter, property)),
        },
        other => other,
    }
}

fn parse_for_each_kicker_count(clause: &str) -> Option<QuantityRef> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("time ").parse(clause).ok()?;
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("it was kicked"),
        tag("this spell was kicked"),
    ))
    .parse(rest)
    .ok()?;
    rest.is_empty().then_some(QuantityRef::KickerCount)
}

fn parse_for_each_target_controlled_type(clause: &str) -> Option<QuantityRef> {
    let (rest, type_text) = alt((
        terminated(
            take_until::<_, _, OracleError<'_>>(" target opponent controls"),
            tag(" target opponent controls"),
        ),
        terminated(
            take_until::<_, _, OracleError<'_>>(" target player controls"),
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
        CardTypeSetSource, Comparator, ControllerRef, FilterProp, PtStat, PtValueScope,
        RoundingMode, TypeFilter, TypedFilter,
    };
    use crate::types::mana::ManaColor;

    /// The expected `QuantityExpr::Difference` for "power and toughness" order:
    /// `Difference { Ref(Power{Recipient}), Ref(Toughness{Recipient}) }`.
    /// Operand order is irrelevant at resolution (`Difference` resolves to an
    /// unsigned magnitude — an Oracle templating convention) but the
    /// constructor pins it for assertion.
    fn pt_difference() -> QuantityExpr {
        QuantityExpr::Difference {
            left: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Recipient,
                },
            }),
            right: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::Toughness {
                    scope: ObjectScope::Recipient,
                },
            }),
        }
    }

    /// CR 400.7 + CR 700.4: "the total power of <subtype> that died this turn"
    /// aggregates the death-time power snapshot over this turn's battlefield→
    /// graveyard records, not live battlefield objects.
    #[test]
    fn total_power_of_subtype_that_died_this_turn_is_zone_change_aggregate() {
        assert_eq!(
            parse_quantity_ref("the total power of Daleks that died this turn"),
            Some(QuantityRef::ZoneChangeAggregateThisTurn {
                from: Some(Zone::Battlefield),
                to: Some(Zone::Graveyard),
                filter: TargetFilter::Typed(TypedFilter::default().subtype("Dalek".to_string())),
                function: AggregateFunction::Sum,
                property: ObjectProperty::Power,
            })
        );
    }

    /// CR 109.5: "under your control" scopes the aggregate to the source's
    /// controller via `inject_controller_you` (controller=You + InZone Battlefield).
    #[test]
    fn total_toughness_died_under_your_control_injects_controller() {
        let qty = parse_quantity_ref(
            "the total toughness of creatures that died under your control this turn",
        )
        .expect("must parse");
        let QuantityRef::ZoneChangeAggregateThisTurn {
            from,
            to,
            filter,
            function,
            property,
        } = qty
        else {
            panic!("expected ZoneChangeAggregateThisTurn, got {qty:?}");
        };
        assert_eq!(from, Some(Zone::Battlefield));
        assert_eq!(to, Some(Zone::Graveyard));
        assert_eq!(function, AggregateFunction::Sum);
        assert_eq!(property, ObjectProperty::Toughness);
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter");
        };
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::InZone { zone } if *zone == Zone::Battlefield)));
    }

    /// Regression guard: a present-tense "total power of creatures you control"
    /// (no "died this turn") still resolves to the live-battlefield `Aggregate`,
    /// NOT the zone-change sibling.
    #[test]
    fn total_power_you_control_stays_live_aggregate() {
        assert_eq!(
            parse_quantity_ref("the total power of creatures you control"),
            Some(QuantityRef::Aggregate {
                function: AggregateFunction::Sum,
                property: ObjectProperty::Power,
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            })
        );
    }

    #[test]
    fn difference_between_its_power_and_toughness() {
        assert_eq!(
            parse_cda_quantity("the difference between its power and toughness"),
            Some(pt_difference()),
            "Doran's `where X is` tail must resolve to a typed Difference, not a Variable"
        );
    }

    #[test]
    fn difference_between_self_ref_power_and_toughness() {
        // `~`-normalized self-reference form
        assert_eq!(
            parse_cda_quantity("the difference between ~'s power and toughness"),
            Some(pt_difference()),
        );
    }

    #[test]
    fn difference_between_this_creatures_power_and_toughness() {
        assert_eq!(
            parse_cda_quantity("the difference between this creature's power and toughness"),
            Some(pt_difference()),
        );
    }

    #[test]
    fn difference_between_toughness_and_power_order_irrelevant() {
        // The reversed ordering parses to a Difference with swapped operands;
        // resolution is absolute, so both produce the same value at runtime.
        let expr = parse_cda_quantity("the difference between its toughness and power");
        assert!(
            matches!(
                expr,
                Some(QuantityExpr::Difference { ref left, ref right })
                    if matches!(**left, QuantityExpr::Ref { qty: QuantityRef::Toughness { scope: ObjectScope::Recipient } })
                    && matches!(**right, QuantityExpr::Ref { qty: QuantityRef::Power { scope: ObjectScope::Recipient } })
            ),
            "reversed ordering should still parse to a Difference, got {expr:?}"
        );
    }

    /// CR 107.1a: a "where X is half …, rounded …" binding routes through
    /// `parse_cda_quantity`; before the fractional arm it fell through to
    /// `Variable { name: "<whole phrase>" }` (resolves to 0). These assert the
    /// fractional wrapper composes over the general quantity grammar's inner for
    /// every supported class — life total, "the number of <type> you control",
    /// and "<type> cards in your graveyard".
    #[test]
    fn cda_half_life_total_rounded_up() {
        // Chainer's Torment: "half your life total, rounded up"
        assert_eq!(
            parse_cda_quantity("half your life total, rounded up"),
            Some(QuantityExpr::DivideRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Controller,
                    },
                }),
                divisor: 2,
                rounding: RoundingMode::Up,
            }),
        );
    }

    #[test]
    fn cda_half_number_of_artifacts_you_control_rounded_down() {
        // Imskir Iron-Eater: "half the number of artifacts you control, rounded down"
        let expr = parse_cda_quantity("half the number of artifacts you control, rounded down");
        assert!(
            matches!(
                expr,
                Some(QuantityExpr::DivideRounded { ref inner, divisor: 2, rounding: RoundingMode::Down })
                    if matches!(**inner, QuantityExpr::Ref { qty: QuantityRef::ObjectCount { .. } })
            ),
            "expected DivideRounded{{ Ref(ObjectCount), 2, Down }}, got {expr:?}"
        );
    }

    #[test]
    fn cda_half_creature_cards_in_graveyard_rounded_up() {
        // Ghoulcaller's Harvest: "half the number of creature cards in your graveyard, rounded up"
        let expr =
            parse_cda_quantity("half the number of creature cards in your graveyard, rounded up");
        assert!(
            matches!(
                expr,
                Some(QuantityExpr::DivideRounded {
                    divisor: 2,
                    rounding: RoundingMode::Up,
                    ..
                })
            ),
            "expected DivideRounded with Up rounding, got {expr:?}"
        );
    }

    #[test]
    fn cda_half_highest_opponent_life_rounded_up() {
        // Malignus: "half the highest life total among your opponents, rounded up".
        // The inner is a cross-player life aggregate the general nom grammar does
        // not reach, so the fraction must recurse into the CDA quantity parser.
        let expr =
            parse_cda_quantity("half the highest life total among your opponents, rounded up");
        assert!(
            matches!(
                expr,
                Some(QuantityExpr::DivideRounded { ref inner, divisor: 2, rounding: RoundingMode::Up })
                    if matches!(
                        **inner,
                        QuantityExpr::Ref {
                            qty: QuantityRef::LifeTotal {
                                player: PlayerScope::Opponent { aggregate: AggregateFunction::Max },
                            },
                        },
                    )
            ),
            "expected DivideRounded(LifeTotal(Opponent Max), 2, Up), got {expr:?}"
        );
    }

    #[test]
    fn for_each_counter_on_self_normalized() {
        let qty = parse_for_each_clause("+1/+1 counter on ~").unwrap();
        match qty {
            QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: Some(counter_type),
            } => assert_eq!(counter_type, CounterType::Plus1Plus1),
            other => panic!("Expected CountersOn{{Source, P1P1}}, got {other:?}"),
        }
    }

    #[test]
    fn quantity_ref_age_counters_on_normalized_self() {
        // Phase-1 prerequisite for the dynamic damage-prevention amount
        // (Cover of Winter): "this enchantment" is `~`-normalized before the
        // imperative effect parser sees the clause, so the quantity text that
        // reaches parse_quantity_ref is "the number of age counters on ~".
        let qty = parse_quantity_ref("the number of age counters on ~").unwrap();
        match qty {
            QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: Some(ref counter_type),
            } => assert_eq!(*counter_type, CounterType::Age),
            other => panic!("Expected CountersOn{{Source, age}}, got {other:?}"),
        }
    }

    #[test]
    fn quantity_ref_all_counters_on_normalized_self() {
        for phrase in [
            "the number of counters on ~",
            "the number of counters on it",
        ] {
            let qty = parse_quantity_ref(phrase).unwrap();
            match qty {
                QuantityRef::CountersOn {
                    scope: ObjectScope::Source,
                    counter_type: None,
                } => {}
                other => panic!("Expected CountersOn{{Source, any}} for {phrase}, got {other:?}"),
            }
        }
    }

    #[test]
    fn quantity_ref_all_counters_on_that_object() {
        for phrase in [
            "the number of counters on that creature",
            "the number of counters on that permanent",
        ] {
            let qty = parse_quantity_ref(phrase).unwrap();
            match qty {
                QuantityRef::CountersOn {
                    scope: ObjectScope::Target,
                    counter_type: None,
                } => {}
                other => panic!("Expected CountersOn{{Target, any}} for {phrase}, got {other:?}"),
            }
        }
    }

    #[test]
    fn for_each_any_counter_on_self_type_phrase() {
        // CR 122.1: "counter on this [type]" — untyped, source-scoped.
        // Gavel of the Righteous: "gets +1/+1 for each counter on this Equipment."
        for phrase in [
            "counter on this equipment",
            "counter on this artifact",
            "counter on this permanent",
            "counter on ~",
            "counter on it",
            "counters on this equipment",
        ] {
            let qty = parse_for_each_clause(phrase);
            assert!(
                matches!(
                    qty,
                    Some(QuantityRef::CountersOn {
                        scope: ObjectScope::Source,
                        counter_type: None,
                    })
                ),
                "expected CountersOn{{Source, None}} for {phrase:?}, got {qty:?}"
            );
        }
    }

    #[test]
    fn for_each_singular_counter_on_self() {
        // Singular "counter on ~" (not "counters on ~")
        let qty = parse_for_each_clause("blight counter on it").unwrap();
        assert!(
            matches!(qty, QuantityRef::CountersOn { scope: ObjectScope::Source, counter_type: Some(ref counter_type) } if *counter_type == CounterType::Generic("blight".to_string())),
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
            matches!(qty, QuantityRef::CountersOn { scope: ObjectScope::Target, counter_type: Some(ref counter_type) } if *counter_type == CounterType::Plus1Plus1),
            "counter on that creature should produce CountersOnTarget, not CountersOnSelf"
        );
    }

    #[test]
    fn for_each_this_way_produces_tracked_set_size() {
        let qty = parse_for_each_clause("card put into a graveyard this way").unwrap();
        assert_eq!(qty, QuantityRef::TrackedSetSize);
    }

    #[test]
    fn for_each_card_exiled_from_your_hand_this_way_tracks_hand_exiles() {
        let qty = parse_for_each_clause("card exiled from your hand this way").unwrap();
        assert_eq!(qty, QuantityRef::ExiledFromHandThisResolution);
    }

    /// CR 608.2c + CR 122.1: "[type] counter[s] removed this way" must dispatch
    /// to `PreviousEffectAmount` so the resolver picks up the actual count of
    /// counters removed by the parent `Effect::RemoveCounter`. Coalition Relic
    /// and the Storage Counter cycle depend on this dispatch — without it, the
    /// generic `TrackedSetSize` fallback returns the count of *objects* affected
    /// (always 1 for a self-counter-removal), which is wrong.
    #[test]
    fn for_each_opponent_drew_two_or_more_cards_this_turn() {
        let qty = parse_for_each_clause("opponent who drew two or more cards this turn").unwrap();
        match qty {
            QuantityRef::PlayerCount {
                filter:
                    PlayerFilter::PlayerAttribute {
                        relation: PlayerRelation::Opponent,
                        attr,
                        comparator: Comparator::GE,
                        value,
                    },
            } => {
                assert_eq!(*value, QuantityExpr::Fixed { value: 2 });
                assert!(matches!(
                    attr.as_ref(),
                    QuantityRef::CardsDrawnThisTurn { .. }
                ));
            }
            other => panic!("expected PlayerCount PlayerAttribute draw filter, got {other:?}"),
        }
    }

    #[test]
    fn for_each_opponent_had_two_lands_enter_this_turn() {
        let qty = parse_for_each_clause(
            "opponent who had two or more lands enter the battlefield under their control this turn",
        )
        .unwrap();
        match qty {
            QuantityRef::PlayerCount {
                filter:
                    PlayerFilter::PlayerAttribute {
                        relation: PlayerRelation::Opponent,
                        attr,
                        comparator: Comparator::GE,
                        value,
                    },
            } => {
                assert_eq!(*value, QuantityExpr::Fixed { value: 2 });
                assert!(matches!(
                    attr.as_ref(),
                    QuantityRef::BattlefieldEntriesThisTurn { .. }
                ));
            }
            other => {
                panic!("expected PlayerCount PlayerAttribute land-entry filter, got {other:?}")
            }
        }
    }

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
    fn quantity_ref_number_of_counters_removed_this_way_is_previous_effect_amount() {
        let qty = parse_quantity_ref("the number of study counters removed this way").unwrap();
        assert_eq!(qty, QuantityRef::PreviousEffectAmount);
    }

    #[test]
    fn for_each_opponent_dealt_combat_damage_is_player_count() {
        for phrase in [
            "opponent that was dealt combat damage this turn",
            "opponent who was dealt combat damage this turn",
            "opponents that were dealt combat damage this turn",
            "opponents who were dealt combat damage",
        ] {
            assert_eq!(
                parse_for_each_clause(phrase),
                Some(QuantityRef::PlayerCount {
                    filter: PlayerFilter::OpponentDealtCombatDamage { source: None },
                }),
                "phrase {phrase:?} must consume as OpponentDealtCombatDamage"
            );
        }
    }

    /// CR 508.6: "the number of opponents you attacked [this turn]" (Militant
    /// Angel) routes to `PlayerCount { OpponentAttacked { You, ThisTurn } }`.
    /// The trailing " this turn" is optional (durations may be stripped
    /// upstream), and the singular "opponent" form hits the same arm.
    #[test]
    fn quantity_ref_opponents_you_attacked_is_player_count() {
        for phrase in [
            "the number of opponents you attacked this turn",
            "the number of opponents you attacked",
            "the number of opponent you attacked this turn",
        ] {
            assert_eq!(
                parse_quantity_ref(phrase),
                Some(QuantityRef::PlayerCount {
                    filter: PlayerFilter::OpponentAttacked {
                        subject: AttackSubject::You,
                        scope: AttackScope::ThisTurn,
                    },
                }),
                "phrase {phrase:?} must route to OpponentAttacked {{ You, ThisTurn }}"
            );
        }
    }

    /// CR 508.6: the for-each clause form ("opponent you attacked this turn")
    /// reaches the same `PlayerCount { OpponentAttacked { You, ThisTurn } }`.
    #[test]
    fn for_each_opponent_you_attacked_is_player_count() {
        assert_eq!(
            parse_for_each_clause("opponent you attacked this turn"),
            Some(QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentAttacked {
                    subject: AttackSubject::You,
                    scope: AttackScope::ThisTurn,
                },
            }),
        );
    }

    /// Collision guard: "creature you attacked WITH this turn" (the source-
    /// referential attacked-with form) must stay `QuantityRef::AttackedThisTurn { filter: None }`
    /// — the " with" subject distinguishes it from the player-population
    /// "opponents you attacked" phrase.
    #[test]
    fn creature_you_attacked_with_this_turn_stays_attacked_this_turn() {
        let qty = parse_for_each_clause("creature you attacked with this turn").unwrap();
        assert_eq!(
            qty,
            QuantityRef::AttackedThisTurn {
                scope: CountScope::Controller,
                filter: None,
            }
        );
    }

    #[test]
    fn for_each_creature_attacking_you_counts_attacking_controller() {
        let qty = parse_for_each_clause("creature attacking you").unwrap();
        assert_eq!(
            qty,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().properties(vec![
                    FilterProp::Attacking {
                        defender: Some(ControllerRef::You)
                    }
                ])),
            },
        );
    }

    #[test]
    fn for_each_creature_you_attacked_with_this_turn_counts_attacking_creatures() {
        let qty = parse_for_each_clause("creature you attacked with this turn").unwrap();
        assert_eq!(
            qty,
            QuantityRef::AttackedThisTurn {
                scope: CountScope::Controller,
                filter: None,
            }
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
            matches!(qty, QuantityRef::CountersOn { scope: ObjectScope::Target, counter_type: Some(ref counter_type) } if *counter_type == CounterType::Plus1Plus1),
            "counters on that creature should produce CountersOnTarget"
        );
    }

    #[test]
    fn quantity_ref_singular_counter_on_target() {
        let qty = parse_quantity_ref("charge counter on that permanent").unwrap();
        assert!(
            matches!(qty, QuantityRef::CountersOn { scope: ObjectScope::Target, counter_type: Some(ref counter_type) } if *counter_type == CounterType::Generic("charge".to_string())),
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
                assert_eq!(counter_type, Some(CounterType::Plus1Plus1));
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

    // A1: "the number of opponents who control <filter>" → PlayerCount over the
    // opponents satisfying the shared "who controls …" control predicate.
    #[test]
    fn parse_quantity_ref_opponents_who_control_artifact() {
        let qty = parse_quantity_ref("the number of opponents who control an artifact").unwrap();
        match qty {
            // "who control an artifact" ≡ count >= 1 (old `Controls`).
            QuantityRef::PlayerCount {
                filter:
                    PlayerFilter::ControlsCount {
                        relation: PlayerRelation::Opponent,
                        filter: TargetFilter::Typed(typed),
                        comparator: Comparator::GE,
                        count,
                    },
            } => {
                assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
                assert_eq!(typed.type_filters, vec![TypeFilter::Artifact]);
            }
            other => panic!("Expected PlayerCount{{ControlsCount(artifact)}}, got {other:?}"),
        }
    }

    // A1 (summon: yojimbo): the controlled-permanent filter carries the
    // power/toughness comparison parsed by the shared type-phrase combinator.
    #[test]
    fn parse_quantity_ref_opponents_who_control_creature_power4() {
        use crate::types::ability::{PtStat, PtValueScope};
        let qty = parse_quantity_ref(
            "the number of opponents who control a creature with power 4 or greater",
        )
        .unwrap();
        match qty {
            QuantityRef::PlayerCount {
                filter:
                    PlayerFilter::ControlsCount {
                        relation: PlayerRelation::Opponent,
                        filter: TargetFilter::Typed(typed),
                        comparator: Comparator::GE,
                        count,
                    },
            } => {
                assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
                assert_eq!(typed.type_filters, vec![TypeFilter::Creature]);
                assert!(
                    typed.properties.contains(&FilterProp::PtComparison {
                        stat: PtStat::Power,
                        scope: PtValueScope::Current,
                        comparator: Comparator::GE,
                        value: QuantityExpr::Fixed { value: 4 },
                    }),
                    "Expected power>=4 PtComparison, got {:?}",
                    typed.properties
                );
            }
            other => {
                panic!("Expected PlayerCount{{ControlsCount(creature+pt)}}, got {other:?}")
            }
        }
    }

    #[test]
    fn parse_cda_quantity_permanents_sacrificed_this_turn() {
        let expr = parse_cda_quantity("the number of permanents you've sacrificed this turn")
            .expect("should parse");
        assert_eq!(
            expr,
            QuantityExpr::Ref {
                qty: QuantityRef::SacrificedThisTurn {
                    player: PlayerScope::Controller,
                    filter: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Permanent],
                        ..Default::default()
                    }),
                }
            }
        );
    }

    #[test]
    fn parse_for_each_sacrificed_this_turn_permanents() {
        let qty = parse_for_each_clause("permanent you've sacrificed this turn")
            .expect("should parse for-each sacrificed-this-turn permanents");
        assert_eq!(
            qty,
            QuantityRef::SacrificedThisTurn {
                player: PlayerScope::Controller,
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Permanent],
                    ..Default::default()
                }),
            }
        );
    }

    #[test]
    fn parse_cda_quantity_sacrificed_this_turn_creatures() {
        let expr = parse_cda_quantity("the number of creatures you sacrificed this turn")
            .expect("should parse cda quantity sacrificed-this-turn creatures");
        assert_eq!(
            expr,
            QuantityExpr::Ref {
                qty: QuantityRef::SacrificedThisTurn {
                    player: PlayerScope::Controller,
                    filter: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        ..Default::default()
                    }),
                }
            }
        );
    }

    #[test]
    fn parse_for_each_sacrificed_this_turn_no_contraction() {
        // "you sacrificed" without "'ve" contraction
        let qty = parse_for_each_clause("artifact you sacrificed this turn")
            .expect("should parse for-each sacrificed-this-turn without contraction");
        assert_eq!(
            qty,
            QuantityRef::SacrificedThisTurn {
                player: PlayerScope::Controller,
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Artifact],
                    ..Default::default()
                }),
            }
        );
    }

    #[test]
    fn parse_sacrificed_this_turn_rejects_last_turn() {
        // "last turn" is not "this turn" — must not parse
        assert!(
            parse_cda_quantity("the number of creatures you sacrificed last turn").is_none(),
            "should not parse 'sacrificed last turn'"
        );
    }

    // A1 "one plus" path: the Offset arm wraps the inner PlayerCount unchanged
    // (no dedicated change to the offset path was needed).
    #[test]
    fn parse_cda_quantity_one_plus_opponents_who_control_artifact() {
        let expr =
            parse_cda_quantity("one plus the number of opponents who control an artifact").unwrap();
        match expr {
            QuantityExpr::Offset { inner, offset } => {
                assert_eq!(offset, 1);
                assert!(
                    matches!(
                        *inner,
                        QuantityExpr::Ref {
                            qty: QuantityRef::PlayerCount {
                                filter: PlayerFilter::ControlsCount {
                                    relation: PlayerRelation::Opponent,
                                    comparator: Comparator::GE,
                                    ..
                                },
                            },
                        }
                    ),
                    "Expected Offset over PlayerCount{{ControlsCount}}, got {inner:?}"
                );
            }
            other => panic!("Expected Offset{{+1}}, got {other:?}"),
        }
    }

    // A1 negative: with no object after "control", the shared core rejects the
    // everything-matching `TargetFilter::Any`, so we must NOT emit a
    // PlayerCount{ControlsCount} that would silently match all opponents.
    #[test]
    fn parse_quantity_ref_opponents_who_control_no_object_rejected() {
        let qty = parse_quantity_ref("the number of opponents who control");
        assert!(
            !matches!(
                qty,
                Some(QuantityRef::PlayerCount {
                    filter: PlayerFilter::ControlsCount { .. },
                })
            ),
            "Bare 'who control' with no object must not yield ControlsCount, got {qty:?}"
        );
    }

    // A1 comparative (Oreskos Explorer): "the number of players who control more
    // lands than you" → PlayerCount{ControlsCount{All, <bare land>, GT,
    // Ref(ObjectCount{<land>.controller(You)})}}. The "players" population word
    // sets relation All; the comparative branch sets GT against the controller's
    // own land count.
    #[test]
    fn parse_quantity_ref_players_who_control_more_lands_than_you() {
        use crate::types::ability::ControllerRef;
        let qty =
            parse_quantity_ref("the number of players who control more lands than you").unwrap();
        match qty {
            QuantityRef::PlayerCount {
                filter:
                    PlayerFilter::ControlsCount {
                        relation: PlayerRelation::All,
                        filter: TargetFilter::Typed(bare),
                        comparator: Comparator::GT,
                        count,
                    },
            } => {
                assert_eq!(bare.type_filters, vec![TypeFilter::Land]);
                assert_eq!(
                    bare.controller, None,
                    "carried ControlsCount filter must be controller-free"
                );
                match *count {
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ObjectCount {
                                filter: TargetFilter::Typed(you_filter),
                            },
                    } => {
                        assert_eq!(you_filter.type_filters, vec![TypeFilter::Land]);
                        assert_eq!(
                            you_filter.controller,
                            Some(ControllerRef::You),
                            "comparative count must read the controller's own lands"
                        );
                    }
                    other => panic!("Expected Ref(ObjectCount) count, got {other:?}"),
                }
            }
            other => {
                panic!("Expected PlayerCount{{ControlsCount(more lands than you)}}, got {other:?}")
            }
        }
    }

    // A1 comparative (Heidegger, Shinra Executive): "the number of opponents who
    // control more creatures than you" → relation Opponent + GT against the
    // controller's own creature count.
    #[test]
    fn parse_quantity_ref_opponents_who_control_more_creatures_than_you() {
        use crate::types::ability::ControllerRef;
        let qty = parse_quantity_ref("the number of opponents who control more creatures than you")
            .unwrap();
        match qty {
            QuantityRef::PlayerCount {
                filter:
                    PlayerFilter::ControlsCount {
                        relation: PlayerRelation::Opponent,
                        filter: TargetFilter::Typed(bare),
                        comparator: Comparator::GT,
                        count,
                    },
            } => {
                assert_eq!(bare.type_filters, vec![TypeFilter::Creature]);
                assert_eq!(bare.controller, None);
                match *count {
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ObjectCount {
                                filter: TargetFilter::Typed(you_filter),
                            },
                    } => {
                        assert_eq!(you_filter.type_filters, vec![TypeFilter::Creature]);
                        assert_eq!(you_filter.controller, Some(ControllerRef::You));
                    }
                    other => panic!("Expected Ref(ObjectCount) count, got {other:?}"),
                }
            }
            other => {
                panic!(
                    "Expected PlayerCount{{ControlsCount(more creatures than you)}}, got {other:?}"
                )
            }
        }
    }

    // A1 comparative (Priest of the Blessed Graf): "the number of opponents who
    // control more lands than you" → relation Opponent + GT against the
    // controller's own land count.
    #[test]
    fn parse_quantity_ref_opponents_who_control_more_lands_than_you() {
        let qty =
            parse_quantity_ref("the number of opponents who control more lands than you").unwrap();
        match qty {
            QuantityRef::PlayerCount {
                filter:
                    PlayerFilter::ControlsCount {
                        relation: PlayerRelation::Opponent,
                        comparator: Comparator::GT,
                        ..
                    },
            } => {}
            other => {
                panic!("Expected PlayerCount{{ControlsCount(opponents more lands)}}, got {other:?}")
            }
        }
    }

    // CR 122.1f: Glissa's Retriever — "the number of opponents who have three or
    // more poison counters" → PlayerCount{PlayerAttribute{Opponent,
    // PlayerCounter{Poison}, GE, Fixed(3)}}. The "N or more <kind> counters"
    // clause routes the poison kind through the shared player-counter-kind
    // grammar, so rad / experience / ticket parse for free.
    #[test]
    fn parse_quantity_ref_opponents_who_have_n_poison_counters() {
        use crate::types::player::PlayerCounterKind;
        let qty =
            parse_quantity_ref("the number of opponents who have three or more poison counters")
                .unwrap();
        match qty {
            QuantityRef::PlayerCount {
                filter:
                    PlayerFilter::PlayerAttribute {
                        relation: PlayerRelation::Opponent,
                        attr,
                        comparator: Comparator::GE,
                        value,
                    },
            } => {
                assert_eq!(
                    *attr,
                    QuantityRef::PlayerCounter {
                        kind: PlayerCounterKind::Poison,
                        scope: CountScope::ScopedPlayer,
                    }
                );
                assert_eq!(*value, QuantityExpr::Fixed { value: 3 });
            }
            other => panic!("Expected PlayerCount{{PlayerAttribute(poison)}}, got {other:?}"),
        }
    }

    // CR 122.1: the player-counter attribute clause covers the whole counter
    // class, not just Glissa's poison — rad / experience parse identically.
    #[test]
    fn parse_quantity_ref_opponents_who_have_n_counters_covers_class() {
        use crate::types::player::PlayerCounterKind;
        for (phrase, kind) in [
            ("rad", PlayerCounterKind::Rad),
            ("experience", PlayerCounterKind::Experience),
        ] {
            let text = format!("the number of opponents who have two or more {phrase} counters");
            match parse_quantity_ref(&text) {
                Some(QuantityRef::PlayerCount {
                    filter:
                        PlayerFilter::PlayerAttribute {
                            attr,
                            comparator: Comparator::GE,
                            value,
                            ..
                        },
                }) => {
                    assert_eq!(
                        *attr,
                        QuantityRef::PlayerCounter {
                            kind,
                            scope: CountScope::ScopedPlayer,
                        }
                    );
                    assert_eq!(*value, QuantityExpr::Fixed { value: 2 });
                }
                other => panic!("Expected PlayerAttribute({phrase}), got {other:?}"),
            }
        }
    }

    // CR 402.1: Wolfcaller's Howl — "the number of your opponents with four or
    // more cards in hand" → PlayerCount{PlayerAttribute{Opponent, HandSize, GE,
    // Fixed(4)}}. The optional leading "your " possessive is stripped before the
    // population word.
    #[test]
    fn parse_quantity_ref_your_opponents_with_n_cards_in_hand() {
        let qty =
            parse_quantity_ref("the number of your opponents with four or more cards in hand")
                .unwrap();
        match qty {
            QuantityRef::PlayerCount {
                filter:
                    PlayerFilter::PlayerAttribute {
                        relation: PlayerRelation::Opponent,
                        attr,
                        comparator: Comparator::GE,
                        value,
                    },
            } => {
                assert_eq!(
                    *attr,
                    QuantityRef::HandSize {
                        player: PlayerScope::ScopedPlayer,
                    }
                );
                assert_eq!(*value, QuantityExpr::Fixed { value: 4 });
            }
            other => panic!("Expected PlayerCount{{PlayerAttribute(hand)}}, got {other:?}"),
        }
    }

    // The "your " possessive is optional — bare "opponents with N or more cards
    // in hand" parses to the same shape.
    #[test]
    fn parse_quantity_ref_opponents_with_n_cards_in_hand_no_possessive() {
        match parse_quantity_ref("the number of opponents with two or more cards in hand") {
            Some(QuantityRef::PlayerCount {
                filter:
                    PlayerFilter::PlayerAttribute {
                        relation: PlayerRelation::Opponent,
                        attr,
                        comparator: Comparator::GE,
                        value,
                    },
            }) => {
                assert_eq!(
                    *attr,
                    QuantityRef::HandSize {
                        player: PlayerScope::ScopedPlayer,
                    }
                );
                assert_eq!(*value, QuantityExpr::Fixed { value: 2 });
            }
            other => panic!("Expected PlayerAttribute(hand, no possessive), got {other:?}"),
        }
    }

    #[test]
    fn parse_quantity_ref_that_creature_card_toughness() {
        assert_eq!(
            parse_quantity_ref("that creature card's toughness"),
            Some(QuantityRef::Toughness {
                scope: ObjectScope::Target
            })
        );
    }

    #[test]
    fn cda_quantity_uses_relative_player_scope_for_they_control() {
        let mut ctx = ParseContext {
            relative_player_scope: Some(ControllerRef::DefendingPlayer),
            ..Default::default()
        };
        let qty = parse_cda_quantity_with_context("the number of artifacts they control", &mut ctx)
            .unwrap();

        match qty {
            QuantityExpr::Ref {
                qty:
                    QuantityRef::ObjectCount {
                        filter:
                            TargetFilter::Typed(TypedFilter {
                                controller: Some(ControllerRef::DefendingPlayer),
                                type_filters,
                                ..
                            }),
                    },
            } => assert_eq!(type_filters, vec![TypeFilter::Artifact]),
            other => panic!("Expected defending-player artifact count, got {other:?}"),
        }
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

    /// CR 120.1 + CR 510.1: Tymna the Weaver — "the number of opponents that
    /// were dealt combat damage this turn" must route to the dedicated
    /// `PlayerCount { OpponentDealtCombatDamage }` and NOT fall through into
    /// the generic type-phrase fallback that produces an empty `ObjectCount`
    /// (the latter matched every battlefield object and drained the deck).
    #[test]
    fn cda_quantity_opponents_dealt_combat_damage() {
        let qty =
            parse_cda_quantity("the number of opponents that were dealt combat damage this turn")
                .unwrap();
        assert_eq!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::OpponentDealtCombatDamage { source: None },
                }
            }
        );
    }

    /// Symmetric singular form ("opponent that was dealt combat damage this
    /// turn") must hit the same `PlayerFilter::OpponentDealtCombatDamage` arm.
    #[test]
    fn cda_quantity_opponent_singular_dealt_combat_damage() {
        let qty =
            parse_cda_quantity("the number of opponent that was dealt combat damage this turn")
                .unwrap();
        assert_eq!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::OpponentDealtCombatDamage { source: None },
                }
            }
        );
    }

    /// CR 120.1 + CR 510.1: Upstream `strip_trailing_duration` removes the
    /// "this turn" suffix before the draw-count parser path reaches
    /// `parse_quantity_ref`. The phrase must still resolve to
    /// `PlayerFilter::OpponentDealtCombatDamage` without the suffix —
    /// otherwise cards like Moonshae Pixie ("draw cards equal to the number
    /// of opponents who were dealt combat damage this turn") regress to
    /// `Effect::Unimplemented`. The "this turn" tail is informational at
    /// this layer: `PlayerCount{OpponentDealtCombatDamage}` already queries
    /// `state.damage_dealt_this_turn`.
    #[test]
    fn cda_quantity_opponents_dealt_combat_damage_strip_suffix() {
        for phrase in [
            "the number of opponents who were dealt combat damage",
            "the number of opponents that were dealt combat damage",
            "the number of opponent who was dealt combat damage",
            "the number of opponent that was dealt combat damage",
        ] {
            let qty = parse_cda_quantity(phrase)
                .unwrap_or_else(|| panic!("phrase {phrase:?} must parse to PlayerCount"));
            assert_eq!(
                qty,
                QuantityExpr::Ref {
                    qty: QuantityRef::PlayerCount {
                        filter: PlayerFilter::OpponentDealtCombatDamage { source: None },
                    }
                },
                "phrase {phrase:?} must route to OpponentDealtCombatDamage",
            );
        }
    }

    /// CR 120.9 + CR 608.2i: Estinien Varlineau — "the number of your opponents
    /// who were dealt combat damage by ~ or a Dragon this turn" must parse the
    /// `by <source>` restriction into `Some(Or[SelfRef, Typed{Dragon}])`. The
    /// `your ` possessive head and the trailing ` this turn` duration are both
    /// consumed by combinators; the source phrase folds via `parse_target` +
    /// `merge_or_filters`. Previously this produced a bare `Variable("...")`
    /// that resolved to 0 at runtime.
    #[test]
    fn opponent_dealt_combat_damage_by_self_or_dragon() {
        let qty = parse_quantity_ref(
            "the number of your opponents who were dealt combat damage by ~ or a Dragon this turn",
        )
        .expect("must parse to PlayerCount");
        assert_eq!(
            qty,
            QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentDealtCombatDamage {
                    source: Some(Box::new(TargetFilter::Or {
                        filters: vec![
                            TargetFilter::SelfRef,
                            TargetFilter::Typed(
                                TypedFilter::default().subtype("Dragon".to_string())
                            ),
                        ],
                    })),
                },
            }
        );
    }

    /// CR 120.9 + CR 608.2i: a single-source `by ~` restriction parses to
    /// `Some(SelfRef)` — the source filter is the ability source alone.
    #[test]
    fn opponent_dealt_combat_damage_by_self_only() {
        let qty = parse_quantity_ref(
            "the number of opponents who were dealt combat damage by ~ this turn",
        )
        .expect("must parse to PlayerCount");
        assert_eq!(
            qty,
            QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentDealtCombatDamage {
                    source: Some(Box::new(TargetFilter::SelfRef)),
                },
            }
        );
    }

    /// CR 120.1 + CR 510.1: the unfiltered class (Tymna the Weaver, Moonshae
    /// Pixie) — no `by <source>` clause — must still parse to `source: None`,
    /// including with the optional `your ` possessive head.
    #[test]
    fn opponent_dealt_combat_damage_unfiltered_is_none() {
        for phrase in [
            "the number of opponents who were dealt combat damage this turn",
            "the number of your opponents who were dealt combat damage this turn",
            "the number of opponents that were dealt combat damage",
        ] {
            assert_eq!(
                parse_quantity_ref(phrase),
                Some(QuantityRef::PlayerCount {
                    filter: PlayerFilter::OpponentDealtCombatDamage { source: None },
                }),
                "phrase {phrase:?} must parse to unfiltered OpponentDealtCombatDamage"
            );
        }
    }

    /// CR 109.1: Defense-in-depth — when `parse_type_phrase` returns an
    /// empty-shaped `Typed` filter (no type words, no controller, no
    /// properties), `parse_quantity_ref` must decline rather than emit an
    /// `ObjectCount` that would match every battlefield permanent.
    ///
    /// The exact text exercised here ("opponents that were dealt combat
    /// damage this turn", without the `the number of` prefix) is the
    /// substring that flows into `parse_type_phrase` for Tymna's body. If
    /// `parse_quantity_ref` is ever called on it directly (e.g. by a future
    /// quantity context that didn't bind the `PlayerCount` arm), the
    /// empty-Typed guard ensures it declines rather than returning an
    /// `ObjectCount` against an empty filter.
    #[test]
    fn parse_quantity_ref_empty_typed_filter_falls_through() {
        // Strip "the number of " then exercise the empty-Typed guard via a
        // remainder that produces a Typed filter with no type predicates.
        let result = parse_quantity_ref("the number of  ");
        assert!(
            !matches!(
                result,
                Some(QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(ref typed),
                }) if typed.type_filters.is_empty()
                    && typed.controller.is_none()
                    && typed.properties.is_empty(),
            ),
            "empty Typed filter must not produce ObjectCount, got {:?}",
            result
        );
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
                        exclude: None,
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
            } => assert_eq!(counter_type, CounterType::Plus1Plus1),
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
                assert_eq!(counter_type, Some(CounterType::Plus1Plus1));
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
    fn cda_quantity_total_mana_value_of_those_exiled_cards_is_tracked_set_aggregate() {
        // CR 609.3 + CR 202.3: the plural anaphor "those exiled cards" aggregates
        // over the most recent chain tracked set (the set the preceding effect
        // published), NOT over live battlefield/exile objects via a type filter.
        // Drives Ensnared by the Mara's "deals damage equal to the total mana
        // value of those exiled cards".
        let qty = parse_cda_quantity("the total mana value of those exiled cards").unwrap();
        assert!(
            matches!(
                qty,
                QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetAggregate {
                        function: AggregateFunction::Sum,
                        property: ObjectProperty::ManaValue,
                    }
                }
            ),
            "expected TrackedSetAggregate(Sum, ManaValue), got {qty:?}"
        );

        // The "the exiled cards" anaphor variant maps to the same set.
        let qty2 = parse_cda_quantity("the total mana value of the exiled cards").unwrap();
        assert!(
            matches!(
                qty2,
                QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetAggregate {
                        function: AggregateFunction::Sum,
                        property: ObjectProperty::ManaValue,
                    }
                }
            ),
            "expected TrackedSetAggregate(Sum, ManaValue) for 'the exiled cards', got {qty2:?}"
        );
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
    fn parse_event_context_quantity_previous_effect_this_way_variants() {
        for phrase in [
            "the life lost this way",
            "the amount of life paid this way",
            "the damage dealt this way",
            "the amount of excess damage dealt this way",
            "opponents dealt damage this way",
            "the number of stun counters removed this way",
        ] {
            assert_eq!(
                parse_event_context_quantity(phrase),
                Some(QuantityExpr::Ref {
                    qty: QuantityRef::PreviousEffectAmount,
                }),
                "phrase {phrase:?} must map to PreviousEffectAmount"
            );
        }
    }

    /// CR 614.1a: "that much life plus N" — Heron of Hope / Angel of Vitality /
    /// Leyline of Hope class. Issue #317 follow-up: parser must emit the typed
    /// `Offset { inner: EventContextAmount, offset: N }` shape the runtime now
    /// consumes via `resolve_event_replacement_quantity`.
    #[test]
    fn parse_event_context_quantity_that_much_life_plus_one() {
        let result = parse_event_context_quantity("that much life plus 1");
        assert_eq!(
            result,
            Some(QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }),
                offset: 1,
            })
        );
    }

    /// CR 614.1a: "that much life minus N" — negative offset variant. Covers
    /// the mirror case for damage/life reduction replacement effects.
    #[test]
    fn parse_event_context_quantity_that_much_life_minus_two() {
        let result = parse_event_context_quantity("that much life minus 2");
        assert_eq!(
            result,
            Some(QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }),
                offset: -2,
            })
        );
    }

    /// CR 614.1a: Bare-quantifier "that much plus N" — no noun phrase.
    /// Verifies the noun arm's empty-tag alternative.
    #[test]
    fn parse_event_context_quantity_that_much_plus_one_bare() {
        let result = parse_event_context_quantity("that much plus 1");
        assert_eq!(
            result,
            Some(QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }),
                offset: 1,
            })
        );
    }

    /// CR 614.1a: "that many cards minus N" — preserves the pre-#317
    /// negative-offset Mill / Draw cards path now subsumed by the unified
    /// combinator.
    #[test]
    fn parse_event_context_quantity_that_many_cards_minus_one() {
        let result = parse_event_context_quantity("that many cards minus 1");
        assert_eq!(
            result,
            Some(QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }),
                offset: -1,
            })
        );
    }

    /// CR 107.x + CR 506.2: Mr. Foxglove's binary-minus draw count composes
    /// `Sum[left, Multiply{-1, right}]` over two dynamic hand-size quantities,
    /// the right one negated. This is the arithmetic-aware shape the draw
    /// effect-construction fallback (`try_parse_equal_to_quantity_effect`)
    /// reaches via `parse_cda_quantity`.
    #[test]
    fn parse_cda_quantity_defending_minus_your_hand() {
        let result = parse_cda_quantity(
            "the number of cards in defending player's hand minus the number of cards in your hand",
        );
        assert_eq!(
            result,
            Some(QuantityExpr::Sum {
                exprs: vec![
                    QuantityExpr::Ref {
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::DefendingPlayer,
                        },
                    },
                    // CR 402: "the number of cards in your hand" inside
                    // `parse_cda_quantity` routes through the "the number of"
                    // arm to the typed `HandSize { Controller }` ref (not the
                    // bare-suffix `ZoneCardCount` form).
                    QuantityExpr::Multiply {
                        factor: -1,
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::HandSize {
                                player: PlayerScope::Controller,
                            },
                        }),
                    },
                ],
            })
        );
    }

    #[test]
    fn parse_event_context_quantity_its_power() {
        assert_eq!(
            parse_event_context_quantity("its power"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Anaphoric
                }
            })
        );
    }

    #[test]
    fn parse_event_context_quantity_its_toughness() {
        assert_eq!(
            parse_event_context_quantity("its toughness"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Toughness {
                    scope: ObjectScope::Anaphoric
                }
            })
        );
    }

    #[test]
    fn parse_event_context_quantity_its_mana_value() {
        assert_eq!(
            parse_event_context_quantity("its mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Anaphoric
                }
            })
        );
    }

    /// CR 119.3 + CR 208.1: "its power plus its toughness" / "its toughness plus
    /// its power" — sum of Anaphoric power + toughness refs. Both orderings are
    /// accepted; the result is always Sum([Power(Anaphoric), Toughness(Anaphoric)]).
    /// Class: Phthisis ("lose life equal to its power plus its toughness").
    #[test]
    fn parse_event_context_quantity_its_power_plus_toughness() {
        let expected = QuantityExpr::Sum {
            exprs: vec![
                QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Anaphoric,
                    },
                },
                QuantityExpr::Ref {
                    qty: QuantityRef::Toughness {
                        scope: ObjectScope::Anaphoric,
                    },
                },
            ],
        };
        assert_eq!(
            parse_event_context_quantity("its power plus its toughness"),
            Some(expected.clone()),
            "power then toughness ordering"
        );
        assert_eq!(
            parse_event_context_quantity("its toughness plus its power"),
            Some(expected),
            "toughness then power ordering should yield the same Sum"
        );
    }

    /// CR 608.2c: bare demonstrative "that spell" inside a triggered ability or
    /// delayed-trigger continuation is an instruction-order referent — it
    /// points at the spell introduced by an earlier instruction in the same
    /// ability (typically a counter / copy / reveal), not at the cost-paid
    /// object. It selects `ObjectScope::Demonstrative` (the noun-phrase referent,
    /// distinct from the pronoun "its") so the subject-injection rewrite never
    /// rebinds it; slot priority differs from `CostPaidObject`
    /// (effect_context_object first vs. cost_paid_object first); see
    /// `classify_possessive_referent` and `resolve_object_mana_value`'s
    /// `Demonstrative` arm. Mana Drain is the canonical delayed-trigger member of
    /// this class — `snapshot_quantity_ref`
    /// (`game/effects/delayed_trigger.rs`) bakes the resolved value into
    /// `Fixed` at delayed-trigger creation time using the parent's target
    /// snapshot, so slot priority at firing time is irrelevant for that card.
    #[test]
    fn parse_event_context_quantity_spell_mana_value() {
        assert_eq!(
            parse_event_context_quantity("that spell's mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Demonstrative
                }
            })
        );
    }

    /// CR 608.2c — Yuriko, the Tiger's Shadow / Dark Confidant class
    /// (issue #511). A reveal in an earlier instruction binds "that card's
    /// mana value" to the revealed card. The bare demonstrative prefix "that
    /// card" selects `ObjectScope::Demonstrative` so the runtime resolver
    /// reads `effect_context_object` (the revealed card) before the trigger
    /// source (the Ninja that dealt combat damage).
    #[test]
    fn parse_event_context_possessive_that_card_mana_value_demonstrative() {
        assert_eq!(
            parse_event_context_quantity("that card's mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Demonstrative
                }
            })
        );
    }

    /// CR 608.2c — bare demonstrative "that permanent" inside a triggered ability
    /// is an instruction-order referent like "that card" / "that creature".
    #[test]
    fn parse_event_context_possessive_that_permanent_power_demonstrative() {
        assert_eq!(
            parse_event_context_quantity("that permanent's power"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Demonstrative
                }
            })
        );
    }

    /// CR 608.2c — battles are objects and can be referenced by bare
    /// demonstrative possessives the same way cards, permanents, and spells are.
    #[test]
    fn parse_event_context_possessive_that_battle_mana_value_demonstrative() {
        assert_eq!(
            parse_event_context_quantity("that battle's mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Demonstrative
                }
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

    /// Negative guard for `classify_possessive_referent`'s `bare_types`
    /// allowlist — an unknown type word ("wizard") must NOT silently classify
    /// as anaphoric just because it follows a `"that "` / `"the "` determiner.
    /// Pairs with the positive `parse_event_context_possessive_that_card_*`
    /// tests to lock both sides of the classifier.
    #[test]
    fn parse_event_context_possessive_unknown_type_returns_none() {
        assert_eq!(
            parse_event_context_quantity("that wizard's mana value"),
            None
        );
        assert_eq!(parse_event_context_quantity("the wizard's power"), None);
    }

    /// Negative guard for the participle word-boundary fix: a prefix like
    /// `"the targeted player"` must NOT classify as `CostPaidObject` just
    /// because it begins with `"targeted"`. CR 608.2k only references
    /// objects, not players, so a player-possessive must be rejected here
    /// and fall through to `parse_quantity_ref` (or remain unmatched).
    /// Without the trailing-space guard in `classify_possessive_referent`,
    /// `tag("targeted")` would match `"targeted player"` and silently emit
    /// `Power { CostPaidObject }` for any combination of (participle root
    /// prefix) + (non-type-word suffix) — a regression vector that the old
    /// `starts_with(adj)` shape also shared.
    #[test]
    fn parse_event_context_possessive_participle_requires_word_boundary() {
        // Concocted to flex the word-boundary guard — the bare phrase has no
        // existing card today, but the failure mode it prevents is real.
        assert_eq!(parse_event_context_quantity("the targeter's power"), None);
        assert_eq!(
            parse_event_context_quantity("the targeted player's power"),
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

    // CR 608.2k — Greater Good / issue #338: `sacrificed`/`exiled`/`discarded`
    // and the other participle-possessive prefixes (`destroyed`, `countered`,
    // `returned`, `targeted`, `revealed`, `drawn`, `copied`) are positively
    // classified as `ObjectScope::CostPaidObject` by
    // `classify_possessive_referent`. They are siblings to the
    // bare-anaphoric / `Anaphoric` tests above (`that card's mana value`,
    // `that creature's toughness`) and together pin both halves of the
    // possessive classifier.
    #[test]
    fn parse_event_context_possessive_sacrificed_creature_power() {
        assert_eq!(
            parse_event_context_quantity("the sacrificed creature's power"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::CostPaidObject
                }
            })
        );
    }

    #[test]
    fn parse_event_context_possessive_sacrificed_creature_toughness() {
        assert_eq!(
            parse_event_context_quantity("the sacrificed creature's toughness"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Toughness {
                    scope: ObjectScope::CostPaidObject
                }
            })
        );
    }

    /// CR 608.2c — "that creature" is a bare demonstrative possessive: it points
    /// at the most recent earlier-instruction object (a revealed / sacrificed-by-
    /// effect / moved permanent), so the parser emits
    /// `ObjectScope::Demonstrative` (distinct from the pronoun "its" so the
    /// subject-injection rewrite never rebinds it — this is what protects
    /// Creature Bond's "that creature's toughness" from the LKI-toughness fix's
    /// generalized rebind). The runtime resolver consults `effect_context_object`
    /// first, then the trigger source, then `cost_paid_object` — the inverse of
    /// `CostPaidObject`'s slot order. Participle-possessive forms (`the sacrificed
    /// creature's toughness`, `the destroyed creature's power`) continue to map to
    /// `CostPaidObject` — see the sibling regression tests below.
    #[test]
    fn parse_event_context_possessive_that_creature_toughness_demonstrative() {
        assert_eq!(
            parse_event_context_quantity("that creature's toughness"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Toughness {
                    scope: ObjectScope::Demonstrative
                }
            })
        );
    }

    #[test]
    fn parse_event_context_possessive_exiled_card_mana_value() {
        assert_eq!(
            parse_event_context_quantity("the exiled card's mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject
                }
            })
        );
    }

    #[test]
    fn parse_event_context_possessive_discarded_creature_power() {
        assert_eq!(
            parse_event_context_quantity("the discarded creature's power"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::CostPaidObject
                }
            })
        );
    }

    #[test]
    fn parse_event_context_possessive_destroyed_creature_power() {
        assert_eq!(
            parse_event_context_quantity("the destroyed creature's power"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::CostPaidObject
                }
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

    /// CR 608.2k — `milled` is a participle-possessive cost referent
    /// (Patchwork Automaton, Demilich, Court of Cunning class). The mill
    /// resolves to an event-context object whose mana value is then queried.
    #[test]
    fn parse_event_context_possessive_milled_card_mana_value() {
        assert_eq!(
            parse_event_context_quantity("the milled card's mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject
                }
            })
        );
    }

    /// CR 608.2k — `discovered` is a participle-possessive trigger-condition
    /// referent (LCI mechanic). The discovered card's mana value is queried
    /// in the same instruction that introduced it.
    #[test]
    fn parse_event_context_possessive_discovered_card_mana_value() {
        assert_eq!(
            parse_event_context_quantity("the discovered card's mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject
                }
            })
        );
    }

    /// CR 608.2k — sacrificed-`artifact` and sacrificed-`enchantment`
    /// participle-possessives (Krark-Clan Ironworks family, Goblin Welder
    /// adjacent). Locks the type-word axis: any single-word card type
    /// (`creature`/`artifact`/`enchantment`/`card`/`spell`/`permanent`/
    /// `land`/`battle`/`planeswalker`) must classify as `CostPaidObject` when
    /// preceded by a participle, not just `creature`.
    #[test]
    fn parse_event_context_possessive_sacrificed_artifact_mana_value() {
        assert_eq!(
            parse_event_context_quantity("the sacrificed artifact's mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject
                }
            })
        );
    }

    #[test]
    fn parse_event_context_possessive_sacrificed_enchantment_mana_value() {
        assert_eq!(
            parse_event_context_quantity("the sacrificed enchantment's mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject
                }
            })
        );
    }

    /// CR 608.2k + CR 205.4a — supertype composition: the participle prefix is
    /// `participle + opt(supertype) + type_word`. Real Oracle text does not
    /// presently use forms like `"the sacrificed legendary creature's power"`,
    /// but the grammar accepts it via decomposition from
    /// `parse_supertype_prefix` + `parse_type_filter_word`. This test pins the
    /// composition contract so future printings of multi-word possessives are
    /// covered the moment they ship.
    #[test]
    fn parse_event_context_possessive_legendary_creature_supertype_composition() {
        assert_eq!(
            parse_event_context_quantity("the sacrificed legendary creature's power"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::CostPaidObject
                }
            })
        );
    }

    /// CR 109.1 / CR 110.5 — tokens are objects and can anchor possessive
    /// references. The `token` referent is accepted explicitly (it is not a
    /// CR 205 card type so `parse_type_filter_word` would not match it).
    #[test]
    fn parse_event_context_possessive_sacrificed_token_power() {
        assert_eq!(
            parse_event_context_quantity("the sacrificed token's power"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::CostPaidObject
                }
            })
        );
    }

    /// Negative guard — a player-possessive prefix (one that does not begin
    /// with the `"that "` / `"the "` determiner) must NOT be classified as
    /// either `CostPaidObject` or `Anaphoric`. The determiner gate in
    /// `classify_possessive_referent` is the load-bearing rejection: without
    /// it, `"an opponent's"` would split to prefix `"an opponent"` and
    /// silently fall through to the player-possessive parser. Locking this
    /// behavior at the classifier boundary prevents future regressions if the
    /// determiner alt is ever loosened.
    #[test]
    fn parse_event_context_possessive_rejects_an_opponent_prefix() {
        assert_eq!(
            parse_event_context_quantity("an opponent's life total"),
            None
        );
    }

    /// Negative guard — plural possessive (`creatures'`) is ungrammatical in
    /// Oracle text. `parse_type_filter_word` accepts plural forms for the
    /// targeting branch, but the possessive object-type combinator must
    /// reject them so `"the sacrificed creatures' power"` does NOT classify.
    #[test]
    fn parse_event_context_possessive_rejects_plural_type() {
        // Real possessives are always singular. Split happens on "'s ", so a
        // plural-with-apostrophe form like `creatures'` won't even reach
        // `classify_possessive_referent` — this test belt-and-suspenders the
        // singular-only contract via the trailing-'s' guard.
        assert_eq!(
            parse_event_context_quantity("the sacrificed creatures' power"),
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
                filter: None,
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
                filter: None,
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

    /// CR 109.5 + CR 608.2c: "creature they control" inside a "for each [player]"
    /// clause threads the relative player scope into the `ObjectCount` filter's
    /// controller. Edit 1b swapped the no-ctx fallback (`parse_type_phrase`) for
    /// the ctx-aware `parse_type_phrase_with_ctx`. Reverting Edit 1b discards the
    /// scope, so "they control" collapses to `ControllerRef::You` and this assert
    /// (ScopedPlayer) fails. Discriminating fail-on-revert guard for the parser fix.
    #[test]
    fn for_each_they_control_threads_scoped_player() {
        let ctx = ParseContext {
            relative_player_scope: Some(ControllerRef::ScopedPlayer),
            ..Default::default()
        };
        let qty = parse_for_each_clause_with_context("creature they control", &ctx).unwrap();
        let QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(typed),
        } = qty
        else {
            panic!("Expected ObjectCount over Typed filter, got {qty:?}");
        };
        assert_eq!(
            typed.controller,
            Some(ControllerRef::ScopedPlayer),
            "\"they control\" must bind to the iterating player, not the caster"
        );
        assert!(typed.type_filters.contains(&TypeFilter::Creature));
    }

    /// CR 109.4 + CR 115.1: "creature they control" with a `TargetPlayer` relative
    /// scope (e.g. Burden of Greed's "for each artifact that player controls")
    /// threads `TargetPlayer` through the fallback. Same fail-on-revert axis as
    /// `for_each_they_control_threads_scoped_player` but for the targeted-player
    /// scope.
    #[test]
    fn for_each_they_control_threads_target_player() {
        let ctx = ParseContext {
            relative_player_scope: Some(ControllerRef::TargetPlayer),
            ..Default::default()
        };
        let qty = parse_for_each_clause_with_context("artifact they control", &ctx).unwrap();
        let QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(typed),
        } = qty
        else {
            panic!("Expected ObjectCount over Typed filter, got {qty:?}");
        };
        assert_eq!(typed.controller, Some(ControllerRef::TargetPlayer));
    }

    /// CR 109.5: "creature you control" stays bound to `ControllerRef::You` even
    /// when a relative player scope is present, because the "you control" suffix
    /// arm is context-independent. Confirms Edit 1b does not disturb caster-relative
    /// counts (The Scarab God).
    #[test]
    fn for_each_you_control_stays_caster_with_scope_present() {
        let ctx = ParseContext {
            relative_player_scope: Some(ControllerRef::ScopedPlayer),
            ..Default::default()
        };
        let qty = parse_for_each_clause_with_context("creature you control", &ctx).unwrap();
        let QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(typed),
        } = qty
        else {
            panic!("Expected ObjectCount over Typed filter, got {qty:?}");
        };
        assert_eq!(typed.controller, Some(ControllerRef::You));
    }

    #[test]
    fn for_each_other_creature_you_control_with_exact_base_power() {
        let qty = parse_for_each_clause("other creature you control with base power 1").unwrap();
        let QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(typed),
        } = qty
        else {
            panic!("Expected ObjectCount over Typed filter, got {qty:?}");
        };
        assert_eq!(typed.controller, Some(ControllerRef::You));
        assert!(typed.type_filters.contains(&TypeFilter::Creature));
        assert!(typed.properties.contains(&FilterProp::Another));
        assert!(typed.properties.contains(&FilterProp::PtComparison {
            stat: PtStat::Power,
            scope: PtValueScope::Base,
            comparator: Comparator::EQ,
            value: QuantityExpr::Fixed { value: 1 },
        }));
    }

    #[test]
    fn quantity_number_of_nonland_cards_milled_this_way_uses_event_count() {
        assert_eq!(
            parse_quantity_ref("the number of nonland cards milled this way"),
            Some(QuantityRef::EventContextAmount)
        );
        assert_eq!(
            parse_cda_quantity("the number of nonland cards milled this way"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            })
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

    #[test]
    fn for_each_opponent_who_does_counts_accepted_optional_offer() {
        let qty = parse_for_each_clause("opponent who does").unwrap();
        assert_eq!(
            qty,
            QuantityRef::PlayerCount {
                filter: PlayerFilter::PerformedActionThisWay {
                    relation: PlayerRelation::Opponent,
                    action: PlayerActionKind::AcceptedOptionalEffect,
                },
            }
        );
    }

    /// CR 608.2c + CR 109.5 + CR 701.16a: Wernog, Rider's Chaplain — "the number
    /// of opponents who investigated this way" must count the player population
    /// that performed the optional `Investigate`, NOT battlefield objects. The
    /// verb-dispatched combinator shared with the search-this-way path returns
    /// `PerformedActionThisWay { Opponent, Investigate }`.
    #[test]
    fn the_number_of_opponents_who_investigated_this_way_is_player_count() {
        let qty = parse_quantity_ref("the number of opponents who investigated this way").unwrap();
        assert_eq!(
            qty,
            QuantityRef::PlayerCount {
                filter: PlayerFilter::PerformedActionThisWay {
                    relation: PlayerRelation::Opponent,
                    action: PlayerActionKind::Investigate,
                },
            }
        );
    }

    /// CR 604.3 + CR 609.3: Wernog's full repeat count "one plus the number of
    /// opponents who investigated this way" composes the "N plus" Offset arm
    /// over the inner player-action count. Before the inner resolved, the whole
    /// phrase collapsed to an opaque `Variable`.
    #[test]
    fn one_plus_opponents_who_investigated_this_way_is_offset_player_count() {
        let expr = parse_cda_quantity("one plus the number of opponents who investigated this way")
            .unwrap();
        let QuantityExpr::Offset { inner, offset } = expr else {
            panic!("expected Offset, got {expr:?}");
        };
        assert_eq!(offset, 1);
        assert_eq!(
            *inner,
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::PerformedActionThisWay {
                        relation: PlayerRelation::Opponent,
                        action: PlayerActionKind::Investigate,
                    },
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
    /// lowers to `ObjectCount` over a filter that includes `FilterProp::Counters`,
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
                            FilterProp::Counters {
                                counters: crate::types::counter::CounterMatch::OfType(counter_type),
                                ..
                            } if counter_type == &crate::types::counter::CounterType::Plus1Plus1
                        )),
                        "expected Counters {{ OfType(Plus1Plus1), .. }}, got properties {:?}",
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
                        filter: None,
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
    fn cda_owned_instant_and_sorcery_exile_plus_graveyard_count() {
        let result = parse_cda_quantity(
            "the total number of instant and sorcery cards you own in exile and in your graveyard",
        );
        let Some(QuantityExpr::Sum { exprs }) = result else {
            panic!("expected summed zone counts, got {result:?}");
        };
        assert_eq!(exprs.len(), 2);
        for (expr, expected_zone) in [(&exprs[0], ZoneRef::Exile), (&exprs[1], ZoneRef::Graveyard)]
        {
            match expr {
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::ZoneCardCount {
                            zone,
                            card_types,
                            filter: None,
                            scope,
                        },
                } => {
                    assert_eq!(*zone, expected_zone);
                    assert_eq!(card_types, &vec![TypeFilter::Instant, TypeFilter::Sorcery]);
                    assert_eq!(*scope, CountScope::Owner);
                }
                other => panic!("expected ZoneCardCount segment, got {other:?}"),
            }
        }
    }

    #[test]
    fn zone_card_filter_list_named_before_type_is_order_independent() {
        let (_, filter) =
            parse_zone_card_that_are_filter_list("named Slime Against Humanity or are Oozes")
                .expect("reversed filter order must parse");
        assert_slime_against_humanity_filter(&filter);
    }

    /// Slime Against Humanity: cards-with-filter suffix after zone list.
    #[test]
    fn issue_2370_slime_ooze_and_named_card_zone_count() {
        let result = parse_cda_quantity(
            "two plus the total number of cards you own in exile and in your graveyard that are Oozes or are named Slime Against Humanity",
        )
        .expect("slime quantity must parse");
        let QuantityExpr::Offset { inner, offset } = result else {
            panic!("expected Offset of 2 + zone count, got {result:?}");
        };
        assert_eq!(offset, 2);
        let QuantityExpr::Sum { exprs: zones } = *inner else {
            panic!("expected summed exile+graveyard counts");
        };
        assert_eq!(zones.len(), 2);
        for expr in zones.iter() {
            match expr {
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::ZoneCardCount {
                            card_types,
                            filter: Some(filter),
                            scope,
                            ..
                        },
                } => {
                    assert_eq!(scope, &CountScope::Owner);
                    assert!(card_types.is_empty());
                    assert_slime_against_humanity_filter(filter);
                }
                other => panic!("expected ZoneCardCount, got {other:?}"),
            }
        }
    }

    #[test]
    fn issue_2370_named_card_zone_count_is_order_independent() {
        let result = parse_cda_quantity(
            "the total number of cards you own in your graveyard that are named Slime Against Humanity or are Oozes",
        )
        .expect("named-first slime quantity must parse");
        let QuantityExpr::Ref {
            qty:
                QuantityRef::ZoneCardCount {
                    zone,
                    card_types,
                    filter: Some(filter),
                    scope,
                },
        } = result
        else {
            panic!("expected filtered ZoneCardCount, got {result:?}");
        };
        assert_eq!(zone, ZoneRef::Graveyard);
        assert_eq!(scope, CountScope::Owner);
        assert!(card_types.is_empty());
        assert_slime_against_humanity_filter(&filter);
    }

    fn assert_slime_against_humanity_filter(filter: &TargetFilter) {
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert!(filters.iter().any(|filter| {
            matches!(
                filter,
                TargetFilter::Typed(TypedFilter { type_filters, .. })
                    if type_filters.iter().any(|tf| matches!(tf, TypeFilter::Subtype(s) if s == "Ooze"))
            )
        }));
        assert!(filters.iter().any(|filter| {
            matches!(
                filter,
                TargetFilter::Typed(TypedFilter { properties, .. })
                    if properties.iter().any(|prop| matches!(prop, FilterProp::Named { name } if name == "Slime Against Humanity"))
            )
        }));
    }

    #[test]
    fn cda_untyped_graveyard_count_still_works() {
        let result = parse_cda_quantity("the number of cards in your graveyard");
        assert_eq!(
            result,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::GraveyardSize {
                    player: PlayerScope::Controller,
                },
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
    fn for_each_beyond_the_first_clamps_offset_base_count() {
        let result = parse_for_each_clause_expr("creature blocking it beyond the first");
        let Some(QuantityExpr::ClampMin { inner, minimum }) = result else {
            panic!("expected ClampMin, got {result:?}");
        };
        assert_eq!(minimum, 0);
        let QuantityExpr::Offset { inner, offset } = *inner else {
            panic!("expected clamped Offset, got {inner:?}");
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

    /// CR 701.17a + CR 701.17c + CR 400.7j: "the milled card's mana value"
    /// resolves to `ObjectManaValue { CostPaidObject }` via the existing
    /// previously-referenced-object quantity path.
    /// Heed the Mists: "draw cards equal to the milled card's mana value."
    #[test]
    fn event_context_milled_card_mana_value() {
        assert_eq!(
            parse_event_context_quantity("the milled card's mana value"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject,
                },
            }),
            "milled card's mana value must resolve to ObjectManaValue{{CostPaidObject}}"
        );
    }

    /// CR 119.3 + CR 700.1: "for each of your opponents who lost life this
    /// turn" → `PlayerCount { OpponentLostLife }` (Belbe, Corrupted Observer).
    #[test]
    fn parse_for_each_opponents_who_lost_life() {
        let qty = parse_for_each_clause("of your opponents who lost life this turn")
            .expect("for-each opponent-lost-life clause must parse");
        assert_eq!(
            qty,
            QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentLostLife,
            }
        );
        let gained = parse_for_each_clause("opponents who gained life this turn")
            .expect("for-each opponent-gained-life clause must parse");
        assert_eq!(
            gained,
            QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentGainedLife,
            }
        );
    }

    /// CR 104.3: "for each player who has lost the game" (Rampant Frogantua).
    #[test]
    fn parse_for_each_player_who_has_lost_the_game() {
        let qty = parse_for_each_clause("player who has lost the game")
            .expect("lost-game for-each clause must parse");
        assert_eq!(
            qty,
            QuantityRef::PlayerCount {
                filter: PlayerFilter::HasLostTheGame,
            }
        );
        let number = parse_quantity_ref("the number of players who have lost the game")
            .expect("lost-game number-of clause must parse");
        assert_eq!(
            number,
            QuantityRef::PlayerCount {
                filter: PlayerFilter::HasLostTheGame,
            }
        );
    }

    /// Extract the `controller` of an `Aggregate` filter for snapshot tests.
    fn aggregate_filter_controller(qty: &QuantityExpr) -> Option<ControllerRef> {
        match qty {
            QuantityExpr::Ref {
                qty:
                    QuantityRef::Aggregate {
                        filter: TargetFilter::Typed(tf),
                        ..
                    },
            } => tf.controller.clone(),
            _ => None,
        }
    }

    /// CR 608.2h: present-tense snapshot — "the greatest power among creatures
    /// you control as you cast this spell" (Monstrous Onslaught). The trailing
    /// snapshot suffix must not block the Aggregate match, and the filter must
    /// still carry `ControllerRef::You`.
    #[test]
    fn cda_quantity_greatest_power_snapshot_cast_present() {
        let qty = parse_cda_quantity(
            "the greatest power among creatures you control as you cast this spell",
        )
        .unwrap();
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
        assert_eq!(
            aggregate_filter_controller(&qty),
            Some(ControllerRef::You),
            "present-tense snapshot must preserve controller You"
        );
    }

    /// CR 608.2h: "as you activate this ability" snapshot variant (Lukka, Bound
    /// to Ruin).
    #[test]
    fn cda_quantity_greatest_power_snapshot_activate_ability() {
        let qty = parse_cda_quantity(
            "the greatest power among creatures you control as you activate this ability",
        )
        .unwrap();
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
        assert_eq!(aggregate_filter_controller(&qty), Some(ControllerRef::You));
    }

    /// CR 608.2i: past-tense look-back — "the greatest power among creatures you
    /// controlled as you cast this spell" (Lifestream's Blessing). The
    /// discriminating ordering test: `take_until(" you controlled ")` must run
    /// BEFORE parse_type_phrase so the controller is still resolved to You and
    /// the head filter is "creatures" (not corrupted by "you control"
    /// prefix-matching "you controlled").
    #[test]
    fn cda_quantity_greatest_power_snapshot_past_tense() {
        let qty = parse_cda_quantity(
            "the greatest power among creatures you controlled as you cast this spell",
        )
        .unwrap();
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
        assert_eq!(
            aggregate_filter_controller(&qty),
            Some(ControllerRef::You),
            "past-tense look-back must re-inject controller You via inject_controller_you"
        );
    }

    /// Regression: the existing no-snapshot present-tense aggregate must still
    /// parse unchanged after the snapshot relaxation.
    #[test]
    fn cda_quantity_greatest_power_no_snapshot_regression() {
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
        assert_eq!(aggregate_filter_controller(&qty), Some(ControllerRef::You));
    }

    /// CR 119.1 + CR 102.1: cross-player life extremum → LifeTotal/PlayerScope.
    /// "highest … among all players" → AllPlayers{Max} (Sorin, Grim Nemesis;
    /// Arbiter of Knollridge; Scourge inner).
    #[test]
    fn cda_quantity_highest_life_total_among_all_players() {
        let qty = parse_cda_quantity("the highest life total among all players").unwrap();
        assert_eq!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::AllPlayers {
                        aggregate: AggregateFunction::Max,
                        exclude: None,
                    },
                },
            }
        );
    }

    /// "lowest … among all players" → AllPlayers{Min} (Repay in Kind).
    #[test]
    fn cda_quantity_lowest_life_total_among_all_players() {
        let qty = parse_cda_quantity("the lowest life total among all players").unwrap();
        assert_eq!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::AllPlayers {
                        aggregate: AggregateFunction::Min,
                        exclude: None,
                    },
                },
            }
        );
    }

    /// "highest … among your opponents" → Opponent{Max}.
    #[test]
    fn cda_quantity_highest_life_total_among_opponents() {
        let qty = parse_cda_quantity("the highest life total among your opponents").unwrap();
        assert_eq!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Max,
                    },
                },
            }
        );
    }

    /// "lowest … among your opponents" → Opponent{Min} (Mortal Flesh Is Weak).
    #[test]
    fn cda_quantity_lowest_life_total_among_opponents() {
        let qty = parse_cda_quantity("the lowest life total among your opponents").unwrap();
        assert_eq!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Min,
                    },
                },
            }
        );
    }

    /// Bare "the highest life total among players" (no "all") → AllPlayers{Max}.
    /// Confirms the longest-first "all players" / "players" alt ordering.
    #[test]
    fn cda_quantity_highest_life_total_among_players_bare() {
        let qty = parse_cda_quantity("the highest life total among players").unwrap();
        assert_eq!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::AllPlayers {
                        aggregate: AggregateFunction::Max,
                        exclude: None,
                    },
                },
            }
        );
    }

    /// CR 122.1 + CR 607.2a + CR 109.5: Oversimplify's "the total power of
    /// creatures they controlled that were exiled this way" composite
    /// quantity. Lowers to `Aggregate{Sum, Power, And[Typed{Creature,
    /// ScopedPlayer}, ExiledBySource]}`. Building-block test, not a card
    /// test — any future card with the same "<filter> they controlled that
    /// were exiled this way" shape must parse through this exact path.
    #[test]
    fn total_power_of_creatures_they_controlled_exiled_this_way() {
        let qty = parse_quantity_ref(
            "the total power of creatures they controlled that were exiled this way",
        )
        .expect("composite quantity must parse");
        let expected = QuantityRef::Aggregate {
            function: AggregateFunction::Sum,
            property: ObjectProperty::Power,
            filter: TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::ScopedPlayer),
                    ),
                    TargetFilter::ExiledBySource,
                ],
            },
        };
        assert_eq!(qty, expected);
    }

    /// CR 608.2c + CR 400.7: "the number of creatures you controlled that were
    /// destroyed this way" (Kaya's Wrath, issue #2943) must lower to
    /// `FilteredTrackedSetSize`, not `Effect::Unimplemented`. The for-each
    /// path already handled this shape; the `parse_quantity_ref` "the number
    /// of …" path must mirror it before `parse_type_phrase` strips the tail.
    #[test]
    fn parse_quantity_ref_creatures_you_controlled_destroyed_this_way() {
        let qty = parse_quantity_ref(
            "the number of creatures you controlled that were destroyed this way",
        )
        .expect("must parse");
        match qty {
            QuantityRef::FilteredTrackedSetSize { filter, .. } => match *filter {
                TargetFilter::Typed(ref tf) => {
                    assert!(
                        tf.type_filters.contains(&TypeFilter::Creature),
                        "filter must require Creature"
                    );
                    assert!(
                        tf.controller
                            .as_ref()
                            .is_some_and(|c| matches!(c, ControllerRef::You)),
                        "filter must require controller=You"
                    );
                }
                other => panic!("expected Typed filter, got {other:?}"),
            },
            other => panic!("expected FilteredTrackedSetSize, got {other:?}"),
        }
    }

    /// Type-qualified "the number of … destroyed this way" via
    /// `parse_quantity_ref` emits `FilteredTrackedSetSize` when the type
    /// phrase restricts the tracked set.
    #[test]
    fn parse_quantity_ref_permanents_destroyed_this_way_uses_filtered_tracked_set() {
        let qty =
            parse_quantity_ref("the number of permanents destroyed this way").expect("must parse");
        match qty {
            QuantityRef::FilteredTrackedSetSize { filter, .. } => {
                assert!(
                    matches!(filter.as_ref(), TargetFilter::Typed(_)),
                    "expected typed permanent filter, got {filter:?}"
                );
            }
            other => panic!("expected FilteredTrackedSetSize, got {other:?}"),
        }
    }

    /// Same composite shape with "you controlled" — verifies the controller
    /// axis is parameterized correctly across "you / they / an opponent".
    /// CR 608.2c + CR 400.7: "nontoken creature you controlled that was
    /// destroyed this way" must emit FilteredTrackedSetSize, not the plain
    /// TrackedSetSize that the fallback returns for unfiltered "this way"
    /// clauses. Covers Ceaseless Conflict (issue #1503).
    #[test]
    fn nontoken_creature_you_controlled_destroyed_this_way_uses_filtered_tracked_set() {
        let qty =
            parse_for_each_clause("nontoken creature you controlled that was destroyed this way")
                .expect("must parse");
        match qty {
            QuantityRef::FilteredTrackedSetSize { filter, .. } => {
                // Filter must include NonToken and ControlledByYou (controller=You).
                match *filter {
                    TargetFilter::Typed(ref tf) => {
                        assert!(
                            tf.properties
                                .contains(&crate::types::ability::FilterProp::NonToken),
                            "filter must include NonToken"
                        );
                        assert!(
                            tf.controller
                                .as_ref()
                                .is_some_and(|c| matches!(c, ControllerRef::You)),
                            "filter must require controller=You"
                        );
                    }
                    other => panic!("expected Typed filter, got {other:?}"),
                }
            }
            other => panic!("expected FilteredTrackedSetSize, got {other:?}"),
        }
    }

    /// Subtype-only filters must not collapse to plain `TrackedSetSize`; the
    /// parent destroy can move a wider set than the subtype named by the count.
    #[test]
    fn vampire_destroyed_this_way_uses_filtered_tracked_set() {
        let qty = parse_for_each_clause("vampire that was destroyed this way").expect("must parse");
        match qty {
            QuantityRef::FilteredTrackedSetSize { filter, .. } => match *filter {
                TargetFilter::Typed(ref tf) => assert!(
                    tf.type_filters
                        .contains(&TypeFilter::Subtype("Vampire".to_string())),
                    "filter must preserve the Vampire subtype"
                ),
                other => panic!("expected Typed filter, got {other:?}"),
            },
            other => panic!("expected FilteredTrackedSetSize, got {other:?}"),
        }
    }

    /// CR 608.2c + CR 701.9a: "nonland card discarded this way" (Seasoned
    /// Pyromancer) must emit `FilteredTrackedSetSize` with a `[Card, NonLand]`
    /// filter, not the plain `TrackedSetSize` fallback. The filter must include
    /// `TypeFilter::Card` (every card type) and `Non(Land)`, and must NOT be
    /// narrowed to `TypeFilter::Permanent` — a discarded nonland INSTANT or
    /// SORCERY is still a nonland card and must be counted, so the token count
    /// equals every nonland card discarded (CR 701.9a).
    /// (Primary engine fix for issue #740 is in `ability_or_branch_references_tracked_set`.)
    #[test]
    fn nonland_card_discarded_this_way_uses_filtered_tracked_set_nonland() {
        let qty = parse_for_each_clause("nonland card discarded this way").expect("must parse");
        match qty {
            QuantityRef::FilteredTrackedSetSize { filter, caused_by } => {
                assert_eq!(
                    caused_by,
                    Some(crate::types::ability::ThisWayCause::Discarded),
                    "cause must be Discarded"
                );
                match *filter {
                    TargetFilter::Typed(ref tf) => {
                        assert!(
                            tf.type_filters
                                .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))),
                            "filter must include NonLand; got {tf:?}"
                        );
                        // CR 701.9a: must NOT narrow to Permanent — a nonland
                        // instant/sorcery discarded by Seasoned Pyromancer counts.
                        assert!(
                            !tf.type_filters.contains(&TypeFilter::Permanent),
                            "filter must not exclude nonland instants/sorceries; got {tf:?}"
                        );
                    }
                    other => panic!("expected Typed filter, got {other:?}"),
                }
            }
            other => panic!("expected FilteredTrackedSetSize, got {other:?}"),
        }
    }

    /// CR 406.6 + CR 614.1c: "for each instant and sorcery card exiled with it"
    /// (Murktide Regent's Delve ETB counter). The type-phrase prefix intersects
    /// the linked-exile set; `ExiledBySource.extract_in_zone()` is Exile, so the
    /// `ObjectCount` scans the exile zone rather than the battlefield default.
    /// Building-block test: any "<type> exiled with it" for-each count uses this
    /// path.
    #[test]
    fn for_each_typed_card_exiled_with_it_counts_linked_exile() {
        let qty =
            parse_for_each_clause("instant and sorcery card exiled with it").expect("must parse");
        let QuantityRef::ObjectCount { filter } = qty else {
            panic!("expected ObjectCount, got {qty:?}");
        };
        let TargetFilter::And { filters } = filter else {
            panic!("expected And filter, got {filter:?}");
        };
        assert!(
            filters.contains(&TargetFilter::ExiledBySource),
            "And must include ExiledBySource: {filters:?}"
        );
        assert!(
            filters.iter().any(|f| matches!(f, TargetFilter::Or { .. })),
            "instant-and-sorcery type union should be an Or branch: {filters:?}"
        );
    }

    #[test]
    fn total_toughness_of_creatures_you_controlled_exiled_this_way() {
        let qty = parse_quantity_ref(
            "the total toughness of creatures you controlled that were exiled this way",
        )
        .expect("composite quantity must parse");
        let expected = QuantityRef::Aggregate {
            function: AggregateFunction::Sum,
            property: ObjectProperty::Toughness,
            filter: TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                    TargetFilter::ExiledBySource,
                ],
            },
        };
        assert_eq!(qty, expected);
    }

    // ===================================================================
    // Cluster-01: trailing "for each …" multiplier + spell-history cast-origin
    // + suspended-card primitive. CR 400.1 / CR 601.2a / CR 702.62b.
    // ===================================================================

    use crate::parser::oracle_target::cast_capable_zones_except;
    use crate::types::counter::CounterMatch;

    /// Extract the `InAnyZone` zones from a `Typed` spell-history filter.
    fn in_any_zone_of(filter: &TargetFilter) -> Option<&Vec<Zone>> {
        let TargetFilter::Typed(typed) = filter else {
            return None;
        };
        typed.properties.iter().find_map(|p| match p {
            FilterProp::InAnyZone { zones } => Some(zones),
            _ => None,
        })
    }

    // BLOCKER 2 proof: the exact noun-phrase string that the spell-history clause
    // helper reconstructs must yield the cast-origin `InAnyZone` filter.
    #[test]
    fn spell_history_filter_cast_origin_noun_phrase() {
        let filter = parse_spell_history_filter("spell from anywhere other than your hand")
            .expect("noun-phrase form must parse to a cast-origin filter");
        let zones = in_any_zone_of(&filter)
            .expect("filter must carry FilterProp::InAnyZone for the cast-origin restriction");
        assert_eq!(
            zones,
            &cast_capable_zones_except(Zone::Hand),
            "cast-origin zones must be every cast-capable zone except Hand"
        );
    }

    // Shared helper: mid-clause verb-phrase split + noun-phrase reconstruction.
    #[test]
    fn spell_history_clause_cast_origin() {
        let (scope, filter) = parse_spell_history_clause(
            "spell you've cast this turn from anywhere other than your hand",
            CountScope::Controller,
        )
        .expect("cast-origin spell-history clause must parse");
        assert_eq!(scope, CountScope::Controller);
        let filter = filter.expect("cast-origin clause must carry a filter");
        assert_eq!(
            in_any_zone_of(&filter).expect("filter must carry InAnyZone"),
            &cast_capable_zones_except(Zone::Hand),
        );
    }

    #[test]
    fn spell_history_clause_bare_no_filter() {
        // Regression guard: the bare form keeps filter: None.
        assert_eq!(
            parse_spell_history_clause("spells you've cast this turn", CountScope::Controller),
            Some((CountScope::Controller, None)),
        );
    }

    #[test]
    fn spell_history_clause_type_only_fallback() {
        let (scope, filter) =
            parse_spell_history_clause("instant you've cast this turn", CountScope::Controller)
                .expect("type-qualified spell-history clause must parse");
        assert_eq!(scope, CountScope::Controller);
        let filter = filter.expect("type-qualified clause must carry a filter");
        // Must be a typed Instant filter, not the cast-origin InAnyZone shape.
        assert!(
            in_any_zone_of(&filter).is_none(),
            "type-only clause must not carry a cast-origin filter"
        );
        assert!(
            matches!(&filter, TargetFilter::Typed(t) if t.type_filters.contains(&TypeFilter::Instant)),
            "expected a typed Instant filter, got {filter:?}"
        );
    }

    // Finding-5 regression: a non-empty `tail` that is NOT the cast-origin
    // qualifier (an unrelated compound clause) must return `None` so later arms
    // get a chance — not `Some((scope, None))`, which would swallow the trailing
    // text and mis-quantify as a bare spell count.
    #[test]
    fn spell_history_clause_compound_tail_returns_none() {
        assert_eq!(
            parse_spell_history_clause(
                "spell you've cast this turn and each creature you control",
                CountScope::Controller,
            ),
            None,
            "an unrelated trailing clause must not be swallowed",
        );
    }

    // For-each arm: the multiplier clause routes through the shared helper.
    #[test]
    fn for_each_spell_cast_origin() {
        let qty =
            parse_for_each_clause("spell you've cast this turn from anywhere other than your hand")
                .expect("cast-origin for-each clause must parse");
        match qty {
            QuantityRef::SpellsCastThisTurn { scope, filter } => {
                assert_eq!(scope, CountScope::Controller);
                let filter = filter.expect("cast-origin clause must carry a filter");
                assert_eq!(
                    in_any_zone_of(&filter).expect("filter must carry InAnyZone"),
                    &cast_capable_zones_except(Zone::Hand),
                );
            }
            other => panic!("expected SpellsCastThisTurn, got {other:?}"),
        }
    }

    #[test]
    fn for_each_spell_bare_no_filter() {
        assert_eq!(
            parse_for_each_clause("spell you've cast this turn"),
            Some(QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Controller,
                filter: None,
            }),
        );
    }

    // Suspended-card primitive (CR 702.62b): exile + suspend + owned{you} + time counter.
    #[test]
    fn for_each_suspended_card_you_own() {
        let qty = parse_for_each_clause("suspended card you own")
            .expect("suspended-card clause must parse");
        let QuantityRef::ObjectCount { filter } = qty else {
            panic!("expected ObjectCount, got {qty:?}");
        };
        let TargetFilter::Typed(typed) = &filter else {
            panic!("expected a Typed filter, got {filter:?}");
        };
        assert!(
            typed
                .properties
                .contains(&FilterProp::InZone { zone: Zone::Exile }),
            "must require exile zone (CR 702.62b)"
        );
        assert!(
            typed.properties.contains(&FilterProp::HasKeywordKind {
                value: KeywordKind::Suspend,
            }),
            "must require suspend keyword (CR 702.62b)"
        );
        assert!(
            typed.properties.contains(&FilterProp::Owned {
                controller: ControllerRef::You,
            }),
            "must require ownership by the controller"
        );
        assert!(
            typed.properties.contains(&FilterProp::Counters {
                counters: CounterMatch::OfType(CounterType::Time),
                comparator: Comparator::GE,
                count: QuantityExpr::Fixed { value: 1 },
            }),
            "must require at least one time counter (CR 702.62b)"
        );
    }

    // Rose Tyler's compound: "suspended card you own and each other permanent you
    // control with a time counter on it" → Sum of two ObjectCounts.
    #[test]
    fn for_each_expr_rose_tyler_compound_sums_two_counts() {
        let expr = parse_for_each_clause_expr(
            "suspended card you own and each other permanent you control with a time counter on it",
        )
        .expect("compound for-each clause must parse");
        match expr {
            QuantityExpr::Sum { exprs } => {
                assert_eq!(
                    exprs.len(),
                    2,
                    "expected two summed object counts, got {exprs:?}"
                );
                assert!(
                    exprs.iter().all(|t| matches!(
                        t,
                        QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount { .. }
                        }
                    )),
                    "both terms must be ObjectCount, got {exprs:?}"
                );
            }
            other => panic!("expected Sum of two ObjectCounts, got {other:?}"),
        }
    }

    // CDA arm (MATERIAL GAP proof): "the number of spells … from …".
    #[test]
    fn cda_spells_cast_this_turn_cast_origin() {
        let expr = parse_cda_quantity(
            "the number of spells you've cast this turn from anywhere other than your hand",
        )
        .expect("cast-origin CDA spell-history clause must parse");
        match expr {
            QuantityExpr::Ref {
                qty: QuantityRef::SpellsCastThisTurn { scope, filter },
            } => {
                assert_eq!(scope, CountScope::Controller);
                let filter = filter.expect("cast-origin clause must carry a filter");
                assert_eq!(
                    in_any_zone_of(&filter).expect("filter must carry InAnyZone"),
                    &cast_capable_zones_except(Zone::Hand),
                );
            }
            other => panic!("expected SpellsCastThisTurn ref, got {other:?}"),
        }
    }

    #[test]
    fn cda_spells_cast_this_turn_cast_origin_qualifier_before_time() {
        // CR 601.2a + CR 400.1: the cast-origin qualifier and the "this turn"
        // timing window are independent axes and may appear in either order.
        // Impending Flux uses qualifier-then-time ("…cast from anywhere other than
        // your hand this turn"); the count must bind identically to the
        // time-then-qualifier order above.
        let expr = parse_cda_quantity(
            "the number of spells you've cast from anywhere other than your hand this turn",
        )
        .expect("qualifier-then-time cast-origin clause must parse");
        match expr {
            QuantityExpr::Ref {
                qty: QuantityRef::SpellsCastThisTurn { scope, filter },
            } => {
                assert_eq!(scope, CountScope::Controller);
                let filter = filter.expect("cast-origin clause must carry a filter");
                assert_eq!(
                    in_any_zone_of(&filter).expect("filter must carry InAnyZone"),
                    &cast_capable_zones_except(Zone::Hand),
                );
            }
            other => panic!("expected SpellsCastThisTurn ref, got {other:?}"),
        }
    }

    #[test]
    fn cda_spells_cast_this_turn_bare_no_filter() {
        assert_eq!(
            parse_cda_quantity("the number of spells you've cast this turn"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::SpellsCastThisTurn {
                    scope: CountScope::Controller,
                    filter: None,
                },
            }),
        );
    }
}
