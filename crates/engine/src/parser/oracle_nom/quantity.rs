//! Quantity expression combinators for Oracle text parsing.
//!
//! Parses quantity expressions from Oracle text: fixed numbers, dynamic references
//! like "the number of creatures you control", "its power", "your life total",
//! "equal to" phrases, and "for each" phrases.

use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until, take_while1};
use nom::combinator::{all_consuming, map, opt, value};
use nom::multi::separated_list1;
use nom::sequence::{pair, preceded, terminated};
use nom::Parser;

use super::context::ParseContext;
use super::duration::parse_cast_snapshot_suffix;
use super::error::{oracle_err, OracleResult};
use super::primitives::{
    parse_article, parse_counter_type_typed, parse_keyword_name, parse_number,
};
use super::target::parse_type_filter_word;
use crate::parser::oracle_target::{
    parse_shared_quality, parse_shared_quality_clause, parse_target_with_syntax, parse_type_phrase,
    TargetSyntax,
};
use crate::parser::oracle_util::parse_subtype;
use crate::types::ability::{
    AggregateFunction, CardTypeSetSource, CastManaObjectScope, CastManaSpentMetric, ControllerRef,
    CountScope, DamageKindFilter, DevotionColors, FilterProp, ObjectProperty, ObjectScope,
    PlayerScope, QuantityExpr, QuantityRef, RoundingMode, SharedQuality, TargetFilter,
    ThisWayCause, TypeFilter, TypedFilter, ZoneRef,
};
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::keywords::Keyword;
use crate::types::player::PlayerCounterKind;
use crate::types::zones::Zone;

/// Parse a quantity expression: either a fractional expression, a dynamic reference,
/// or a fixed number. Fractional forms ("half X, rounded up/down") compose over the
/// same `parse_quantity_ref` / `parse_number` primitives used for plain quantities.
pub fn parse_quantity(input: &str) -> OracleResult<'_, QuantityExpr> {
    alt((
        parse_max_quantity,
        parse_fraction_rounded,
        map(parse_quantity_ref, |qty| QuantityExpr::Ref { qty }),
        map(parse_number, |n| QuantityExpr::Fixed { value: n as i32 }),
    ))
    .parse(input)
}

pub fn parse_quantity_ref_complete(input: &str) -> OracleResult<'_, QuantityRef> {
    let input = input.trim().trim_end_matches('.');
    all_consuming(parse_quantity_ref).parse(input)
}

pub fn parse_for_each_clause_ref_complete(input: &str) -> OracleResult<'_, QuantityRef> {
    let input = input.trim().trim_end_matches('.');
    all_consuming(parse_for_each_clause_ref).parse(input)
}

fn parse_quantity_operand(input: &str) -> OracleResult<'_, QuantityExpr> {
    alt((
        parse_fraction_rounded,
        map(parse_quantity_ref, |qty| QuantityExpr::Ref { qty }),
        map(parse_number, |n| QuantityExpr::Fixed { value: n as i32 }),
    ))
    .parse(input)
}

/// CR 107.1 + CR 120.4a/120.10: Parse "A or B, whichever is greater" into
/// the maximum of independently parsed integer quantity operands. The suffix is
/// mandatory so ordinary "or" type phrases and modal choices keep falling
/// through to their specialized parsers.
pub fn parse_max_quantity(input: &str) -> OracleResult<'_, QuantityExpr> {
    let (rest, (left, _, right, _)) = (
        parse_quantity_operand,
        tag(" or "),
        parse_quantity_operand,
        alt((tag(", whichever is greater"), tag(" whichever is greater"))),
    )
        .parse(input)?;
    Ok((
        rest,
        QuantityExpr::Max {
            exprs: vec![left, right],
        },
    ))
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

pub(crate) fn parse_fraction_divisor(input: &str) -> OracleResult<'_, u32> {
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
                filter: None,
            },
            tag("library"),
        ),
        value(
            QuantityRef::ZoneCardCount {
                zone: ZoneRef::Hand,
                card_types: Vec::new(),
                scope: CountScope::Controller,
                filter: None,
            },
            tag("hand"),
        ),
        value(
            QuantityRef::ZoneCardCount {
                zone: ZoneRef::Graveyard,
                card_types: Vec::new(),
                scope: CountScope::Controller,
                filter: None,
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
                filter: None,
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

fn attach_property_to_quantity_filter(filter: &mut TargetFilter, property: FilterProp) {
    match filter {
        TargetFilter::Typed(TypedFilter { properties, .. })
            if !properties
                .iter()
                .any(|existing| property.same_kind(existing)) =>
        {
            properties.push(property);
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            for filter in filters {
                attach_property_to_quantity_filter(filter, property.clone());
            }
        }
        TargetFilter::Not { filter } => attach_property_to_quantity_filter(filter, property),
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

fn parse_quantity_controller_suffix(input: &str) -> OracleResult<'_, ControllerRef> {
    alt((
        value(ControllerRef::You, tag(" you control")),
        value(
            ControllerRef::SourceChosenPlayer,
            tag(" the chosen player controls"),
        ),
        // CR 109.4: "your opponents control" — aggregate across each opponent's
        // permanents (Angry Mob, Chameleon Spirit, Entropic Specter class).
        value(ControllerRef::Opponent, tag(" your opponents control")),
    ))
    .parse(input)
}

fn parse_pre_controller_chosen_filter_suffix(input: &str) -> OracleResult<'_, FilterProp> {
    alt((
        // CR 105.4: "of the chosen color" filters by the source's chosen color.
        value(FilterProp::IsChosenColor, tag(" of the chosen color")),
        value(FilterProp::IsChosenColor, tag(" of that color")),
    ))
    .parse(input)
}

/// CR 121.1 + CR 604.3: "card(s) [you('ve) / your opponents have] drawn this
/// turn". Reuses the runtime `CardsDrawnThisTurn` quantity ref already wired for
/// condition checks (Duelist of the Mind CDA) and now for the opponents'-draw
/// cost reduction (Heliod, the Warped Eclipse).
///
/// The leading "card" word is optionally plural so this combinator serves both
/// surface forms uniformly: the "the number of *cards* …" count phrase (plural)
/// and the "for each *card* …" cost-mod clause (singular). The scope tails come
/// from a shared sub-combinator; opponents arms come FIRST so their longer,
/// more-specific phrase wins over the controller arms (longest-match-first,
/// avoiding a controller arm shadowing the opponents phrase on the shared
/// "card[s] " prefix).
fn parse_number_of_cards_drawn_this_turn(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("card").parse(input)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    let (rest, _) = tag(" ").parse(rest)?;
    let (rest, player) = alt((
        // CR 121.1 + CR 102.2/102.3: opponents' draws this turn, summed across
        // all opponents.
        value(
            PlayerScope::Opponent {
                aggregate: AggregateFunction::Sum,
            },
            tag("your opponents have drawn this turn"),
        ),
        // CR 121.1: the caster's own draws this turn.
        value(PlayerScope::Controller, tag("you've drawn this turn")),
        value(PlayerScope::Controller, tag("you have drawn this turn")),
    ))
    .parse(rest)?;
    Ok((rest, QuantityRef::CardsDrawnThisTurn { player }))
}

/// CR 701.9 + CR 603.4: "card(s) [you('ve)] discarded this turn". Reuses the
/// runtime `CardsDiscardedThisTurn` quantity ref already wired for condition
/// checks; this routes it into the dynamic "for each" count path (Misty Knight,
/// Green Goblin, Astonishing Spider-Man: "draw a card for each card you've
/// discarded this turn"). Mirrors `parse_number_of_cards_drawn_this_turn`: the
/// leading "card" word is optionally plural so both the "the number of *cards* …"
/// count phrase and the "for each *card* …" clause are served uniformly.
fn parse_number_of_cards_discarded_this_turn(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("card").parse(input)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    let (rest, _) = tag(" ").parse(rest)?;
    let (rest, player) = alt((
        // CR 701.9 + CR 102.2/102.3: opponents' discards this turn, summed
        // across all opponents.
        value(
            PlayerScope::Opponent {
                aggregate: AggregateFunction::Sum,
            },
            tag("your opponents have discarded this turn"),
        ),
        // CR 701.9: the caster's own discards this turn.
        value(PlayerScope::Controller, tag("you've discarded this turn")),
        value(PlayerScope::Controller, tag("you have discarded this turn")),
        // CR 701.9 + CR 115.1: a single targeted opponent's discards this turn.
        value(
            PlayerScope::Target,
            tag("target opponent discarded this turn"),
        ),
    ))
    .parse(rest)?;
    Ok((rest, QuantityRef::CardsDiscardedThisTurn { player }))
}

/// Parse an optional ", rounded up/down" / ", round up/down" suffix.
///
/// CR 107.1a: Oracle text must specify rounding direction for fractional
/// expressions. When absent (malformed text or upstream trimming), defaults
/// to `Down` — the more common direction in actual Magic cards and a safe
/// fallback for misparses.
pub(crate) fn parse_rounding_suffix(input: &str) -> OracleResult<'_, RoundingMode> {
    let (rest, rounding) = opt(parse_explicit_rounding_suffix).parse(input)?;
    Ok((rest, rounding.unwrap_or(RoundingMode::Down)))
}

pub(crate) fn parse_explicit_rounding_suffix(input: &str) -> OracleResult<'_, RoundingMode> {
    alt((
        value(RoundingMode::Up, tag(", rounded up")),
        value(RoundingMode::Down, tag(", rounded down")),
        value(RoundingMode::Up, tag(", round up")),
        value(RoundingMode::Down, tag(", round down")),
    ))
    .parse(input)
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
        parse_object_property_aggregate_ref,
        parse_distinct_card_types_exiled_with_source,
        // Group mana-value aggregate parsers to reduce alt arity
        alt((
            parse_linked_exile_mana_value_ref,
            parse_greatest_commander_mana_value_ref,
            parse_commander_mana_value_ref,
        )),
        parse_distinct_card_types_in_zone,
        // CR 608.2c + CR 205.2a: "card type[s] among cards <verb> this way" must
        // precede the generic `among <objects>` arm so the chain-tracked-set,
        // cause-filtered count wins on the "card type among cards" prefix. Nested
        // with `parse_distinct_card_types_among_objects` to keep the outer `alt`
        // within nom's tuple arity (nom 8.0 max: 21 items).
        alt((
            parse_distinct_card_types_among_tracked_set,
            parse_distinct_card_types_among_objects,
            // CR 201.2 + CR 603.4: "different <power|mana value> among <type>"
            // distinct-by-quality count (nested here to stay within nom's
            // tuple arity).
            parse_distinct_quality_among_objects,
        )),
        // CR 406.6: "cards exiled with ~" — must precede `parse_cards_in_zone_ref`
        // so "cards exiled with …" wins over the generic "cards in …" zone phrase.
        parse_cards_exiled_with_source,
        parse_life_total_ref,
        // CR 700.8: party-size phrasings — must precede `parse_speed_ref`
        // and zone counts so the leading "your " possessive routes to the
        // dedicated party combinator instead of a generic zone fallback.
        parse_party_size_ref,
        parse_speed_ref,
        // CR 121.1: bare "card(s) [you('ve) / your opponents have] drawn this
        // turn" (no "the number of" prefix) — reached from the for-each cost-mod
        // path (Heliod, the Warped Eclipse) and other bare-quantity contexts.
        // Nested with `parse_cards_in_zone_ref` to keep the outer `alt` within
        // nom's tuple arity. The draws arm must precede the zone arm: the zone
        // arm requires a " in " tag after the card word and so cannot consume
        // "cards your opponents have …", while the draws arm only fires on the
        // exact complete phrase (no greedy prefix consumption).
        alt((
            parse_number_of_cards_drawn_this_turn,
            parse_number_of_cards_discarded_this_turn,
            parse_cards_in_zone_ref,
        )),
        parse_self_power_ref,
        parse_self_toughness_ref,
        parse_damage_dealt_this_turn_ref,
        parse_life_lost_ref,
        parse_life_gained_ref,
        parse_starting_life_ref,
        parse_object_mana_value_ref,
        // CR 608.2k + CR 400.7j + CR 202.3: previously-referenced object's
        // mana value — must precede `parse_event_context_refs` so the
        // cost/effect referent resolver wins over the generic event-source
        // resolver for sacrificed/exiled/milled possessives (Food Chain, Burnt
        // Offering, Metamorphosis, Heed the Mists). The two cost-paid-object
        // front-forms (possessive "the sacrificed permanent's mana value" and
        // prepositional "the mana value of the sacrificed permanent" —
        // Morbid Curiosity) are nested to keep the outer `alt` within nom 8.0's
        // 21-item tuple arity; both resolve the same `ObjectScope::CostPaidObject`.
        // The chosen/revealed prepositional power/toughness form (the beheld
        // cost-paid object — Close Encounter, Monstrous Emergence) shares the
        // same `ObjectScope::CostPaidObject` referent, so it joins this nest.
        alt((
            parse_cost_paid_object_ref,
            parse_cost_paid_object_prepositional_ref,
            parse_cost_paid_object_chosen_revealed_ref,
        )),
        parse_event_context_refs,
    ))
    .or(alt((
        parse_target_power_ref,
        parse_target_life_ref,
        parse_basic_land_type_count,
        // Bare suffix form — reachable when a parent combinator has already
        // consumed "there are N " (see `parse_there_are_conditions`). Anaphoric
        // "they control" binds to a target player here (not a for-each scope).
        |i| parse_basic_land_types_among_lands_controlled_by_ref(i, ControllerRef::TargetPlayer),
        parse_devotion_ref,
        parse_chroma_devotion_ref,
        parse_graveyard_chroma_ref,
        parse_counters_among_ref,
        // CR 402.1: "the player with the {most|fewest} cards in hand" — the
        // cross-player hand-size extremum, the hand-zone peer of the life
        // extremum. Distinctive "the player with the " prefix; no ordering
        // hazard with sibling arms.
        parse_player_with_extremum_cards_in_hand,
    )))
    .parse(input)
}

/// CR 109.3 + CR 205.3m: Parse "the greatest/fewest/total number of
/// <type-phrase> that have/share [a] <quality> in common" into a grouped
/// object-count quantity.
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
    // CR 702.167c: "...the exiled card used to craft it." — the craft-material
    // qualifier is the same linked-exile set, so consume the optional suffix and
    // emit the same aggregate (Jadeheart Attendant).
    let (rest, _) = opt(parse_craft_materials_suffix).parse(rest)?;
    Ok((
        rest,
        QuantityRef::Aggregate {
            function: AggregateFunction::Sum,
            property: ObjectProperty::ManaValue,
            filter: linked_exile_owned_filter(),
        },
    ))
}

/// CR 702.167c: The `And { [ExiledBySource, Owned { You }] }` filter shared by
/// every craft-material / linked-exile reference. `ExiledBySource` resolves the
/// source's linked-exile pool (which includes `ExileLinkKind::CraftMaterial`);
/// `Owned { You }` rebinds per owner under player-scope iteration, matching the
/// existing Skyclave linked-exile precedent (`parse_linked_exile_mana_value_ref`).
fn linked_exile_owned_filter() -> TargetFilter {
    TargetFilter::And {
        filters: vec![
            TargetFilter::ExiledBySource,
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Owned {
                controller: ControllerRef::You,
            }])),
        ],
    }
}

/// CR 702.167c: Consume the craft-material qualifier "used to craft <self>",
/// where `<self>` is the source self-anaphor (`it` / `~` / `this creature` /
/// `this permanent` / `this artifact` / …). "An ability of a permanent may refer
/// to the exiled cards used to craft it." This is a pure suffix combinator —
/// callers decide which linked-exile ref to emit; it only confirms the qualifier
/// is present and returns the remainder.
fn parse_craft_materials_suffix(input: &str) -> OracleResult<'_, ()> {
    let (rest, _) = tag(" used to craft ").parse(input)?;
    let (rest, _) = parse_source_self_anaphor(rest)?;
    Ok((rest, ()))
}

/// CR 109.5: The source self-anaphor used by craft / linked-exile references:
/// `it`, `~`, or `this <noun>` (creature / permanent / artifact / card). Mirrors
/// the anaphor `alt` already used by `parse_cards_exiled_with_source`, factored
/// out so the craft-suffix and the craft noun-phrase combinator share it.
fn parse_source_self_anaphor(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag("~"),
            tag("it"),
            preceded(
                tag("this "),
                take_while1(|c: char| c.is_ascii_alphabetic() || c == '-'),
            ),
        )),
    )
    .parse(input)
}

/// CR 702.167c: Parse the craft-material reference noun phrase
/// "the exiled card[s] used to craft <self>" into the shared linked-exile
/// filter. Single building block reused by the aggregate-property,
/// distinct-colors, and for-each-color (mana) paths so "total power of …",
/// "number of colors among …", and "for each color among …" all resolve over
/// the same `ExileLinkKind::CraftMaterial` pool without per-card phrase tables.
pub(crate) fn parse_craft_materials_filter(input: &str) -> OracleResult<'_, TargetFilter> {
    let (rest, _) = tag("the exiled card").parse(input)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    let (rest, _) = parse_craft_materials_suffix(rest)?;
    Ok((rest, linked_exile_owned_filter()))
}

/// CR 202.3: Parse "mana value" or "converted mana cost" phrase.
fn parse_mana_value_phrase(input: &str) -> OracleResult<'_, ObjectProperty> {
    let (rest, _) = alt((tag("mana value"), tag("converted mana cost"))).parse(input)?;
    Ok((rest, ObjectProperty::ManaValue))
}

/// CR 108.3: Parse ownership phrase - handles "you own" and per-player "they own".
/// CR 109.5: "they own" in each-player contexts binds to ScopedPlayer (the iterating player),
/// not Opponent. This ensures "each player ... a commander they own" selects each player's own commander.
fn parse_commander_owner_phrase(input: &str) -> OracleResult<'_, ControllerRef> {
    alt((
        value(ControllerRef::You, tag("you own ")),
        value(ControllerRef::ScopedPlayer, tag("they own ")),
    ))
    .parse(input)
}

/// CR 903.3d: Parse zone disjunction - "on the battlefield or in the command zone".
fn parse_commander_zone_disjunction(input: &str) -> OracleResult<'_, TargetFilter> {
    let (rest, _) = tag("on the battlefield or in the command zone").parse(input)?;

    // Build zone disjunction filter using InAnyZone for efficiency
    Ok((
        rest,
        TargetFilter::Typed(TypedFilter {
            controller: None,
            type_filters: vec![],
            properties: vec![
                FilterProp::IsCommander,
                FilterProp::InAnyZone {
                    zones: vec![Zone::Battlefield, Zone::Command],
                },
            ],
        }),
    ))
}

/// Parse "the greatest mana value of a commander you own on the battlefield or in the command zone".
///
/// CR 202.3: Superlative "greatest" requires aggregate-max.
/// CR 903.3d: Commander references by zone.
///
/// Used for flashback costs with "where X is the greatest mana value of a commander you own
/// on the battlefield or in the command zone".
///
/// Maps to `QuantityRef::Aggregate` with Max function to handle partner commanders.
fn parse_greatest_commander_mana_value_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("the greatest ").parse(input)?;
    let (rest, property) = parse_mana_value_phrase(rest)?;
    let (rest, _) = tag(" of a commander ").parse(rest)?;
    let (rest, owner) = parse_commander_owner_phrase(rest)?;
    let (rest, mut zone_filter) = parse_commander_zone_disjunction(rest)?;

    // Add ownership to the zone filter
    if let TargetFilter::Typed(ref mut tf) = zone_filter {
        tf.properties.push(FilterProp::Owned {
            controller: owner.clone(),
        });
    }

    Ok((
        rest,
        QuantityRef::Aggregate {
            function: AggregateFunction::Max,
            property,
            filter: zone_filter,
        },
    ))
}

/// Parse "the mana value of a commander you own on the battlefield or in the command zone".
///
/// CR 202.3: Mana value query without superlative.
/// CR 903.3d: Commander references by zone.
///
/// Used for flashback costs with "where X is the mana value of a commander you own
/// on the battlefield or in the command zone" (Stinging Study).
///
/// Maps to `QuantityRef::CommanderManaValue` to select the first matching commander's mana value.
fn parse_commander_mana_value_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("the ").parse(input)?;
    let (rest, _) = parse_mana_value_phrase(rest)?;
    let (rest, _) = tag(" of a commander ").parse(rest)?;
    let (rest, owner) = parse_commander_owner_phrase(rest)?;
    let (rest, _) = parse_commander_zone_disjunction(rest)?;

    Ok((rest, QuantityRef::CommanderManaValue { owner }))
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

/// CR 122.1: Parse "[kind] counters on [object]" after "the number of".
/// Used for patterns like "equal to the number of charge counters on it".
/// Maps to `QuantityRef::CountersOn` with the appropriate scope and counter type.
fn parse_number_of_counters_on_object(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, counter_type) = parse_counter_type_typed(input)?;
    let (rest, _) = tag(" counters on ").parse(rest)?;
    let (rest, scope) = parse_counter_object_scope(rest)?;
    Ok((
        rest,
        QuantityRef::CountersOn {
            scope,
            counter_type: Some(counter_type),
        },
    ))
}

/// Parse the object scope for counter references: "it", "that creature", "that permanent", etc.
///
/// CR 122.1 + CR 608.2k: A creature's ability that counts "+1/+1 counters on
/// him" / "on her" / "on them" refers to that same source object's counters
/// (Red Hulk's Enrage reflex). The gendered/plural objective pronouns are
/// interchangeable with the neuter "it" for the source — same rationale as
/// `parse_self_possessive`.
fn parse_counter_object_scope(input: &str) -> OracleResult<'_, ObjectScope> {
    alt((
        value(ObjectScope::Source, tag("it")),
        value(ObjectScope::Source, tag("~")),
        value(ObjectScope::Source, tag("him")),
        value(ObjectScope::Source, tag("her")),
        value(ObjectScope::Source, tag("them")),
        value(ObjectScope::Target, tag("that creature")),
        value(ObjectScope::Target, tag("that permanent")),
        value(ObjectScope::Target, tag("that artifact")),
        value(ObjectScope::Target, tag("that enchantment")),
        value(ObjectScope::Target, tag("that land")),
        value(ObjectScope::Target, tag("that planeswalker")),
    ))
    .parse(input)
}

/// Parse "the number of [type] you control" → ObjectCount.
fn parse_the_number_of(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((tag("the total number of "), tag("the number of "))).parse(input)?;
    parse_number_of_inner(rest)
}

/// CR 208.1 + CR 202.3: Parse object-property aggregate quantities such as
/// "the greatest power among <filter>" and "the total mana value of <filter>".
/// The aggregate axis and object-property axis are independent typed choices,
/// so new siblings extend this combinator instead of adding one-off phrase
/// recognition in the legacy quantity entry points.
fn parse_object_property_aggregate_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, (function, property)) = alt((
        value(
            (AggregateFunction::Max, ObjectProperty::Power),
            tag("the greatest power among "),
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
        value(
            (AggregateFunction::Sum, ObjectProperty::Toughness),
            tag("the total toughness of "),
        ),
        value(
            (AggregateFunction::Sum, ObjectProperty::ManaValue),
            tag("the total mana value of "),
        ),
    ))
    .parse(input)?;
    // CR 702.167c: "the total power of the exiled cards used to craft it" — the
    // craft-material aggregate (Mastercraft Raptor). Tried before the bare
    // "the exiled cards" tracked-set anaphor because the craft form shares that
    // prefix but reads the persistent `CraftMaterial` linked-exile pool, not the
    // most-recent chain tracked set.
    if let Ok((craft_rest, filter)) = parse_craft_materials_filter(rest) {
        return Ok((
            craft_rest,
            QuantityRef::Aggregate {
                function,
                property,
                filter,
            },
        ));
    }
    if let Ok((anaphor_rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("those exiled cards"),
        tag("the exiled cards"),
    ))
    .parse(rest)
    {
        return Ok((
            anaphor_rest,
            QuantityRef::TrackedSetAggregate { function, property },
        ));
    }
    let (filter, remainder) = parse_type_phrase(rest);
    let final_remainder = parse_cast_snapshot_suffix(remainder.trim_start())
        .ok()
        .and_then(|(snapshot_rest, _)| snapshot_rest.trim().is_empty().then_some(snapshot_rest))
        .unwrap_or(remainder);
    if !quantity_filter_has_meaningful_content(&filter) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((
        final_remainder,
        QuantityRef::Aggregate {
            function,
            property,
            filter,
        },
    ))
}

/// Parse the inner part after "the number of".
fn parse_number_of_inner(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        parse_distinct_card_types_exiled_with_source,
        parse_distinct_card_types_in_zone,
        // CR 608.2c + CR 205.2a: "card type[s] among cards <verb> this way" must
        // precede the generic `among <objects>` arm (same ordering as
        // `parse_quantity_ref`). Nested with `parse_distinct_card_types_among_objects`
        // to stay within nom's top-level `alt` arity (nom 8.0 max: 21 items).
        alt((
            parse_distinct_card_types_among_tracked_set,
            parse_distinct_card_types_among_objects,
        )),
        // CR 201.2 + CR 603.4: "differently named <type-phrase>" (distinct-by-name)
        // and "different <power|mana value> among <type>" (distinct-by-quality —
        // Celebrate the Harvest's "the number of different powers among ..."
        // routes here after the "the number of " prefix strip). Distinct-
        // population counts that must precede `parse_number_of_controlled_type`
        // so the adjective prefix is consumed before the generic typed-filter
        // fallback. Nested to stay within nom's tuple arity. Named class: Gimbal,
        // Audience with Trostani, Awakened Amalgam, Sandsteppe War Riders,
        // All-Fates Scroll, Fungal Colossus, Euroakus, Neriv, Emil, and other
        // "differently named X" counters.
        alt((
            parse_distinct_named_objects,
            parse_distinct_quality_among_objects,
        )),
        // CR 122.1: "[kind] counters <possessor>" must be tried BEFORE the
        // generic type-filter arm so the typed player-counter ref wins over a
        // "[typeword] you control" misread (no `TypeFilter` for counter kinds).
        parse_player_counter_ref_tail,
        // CR 122.1: "[kind] counters on [object]" — counter count on an object.
        // Must precede generic type-filter arm. Used for patterns like
        // "equal to the number of charge counters on it".
        parse_number_of_counters_on_object,
        // CR 700.8: "creatures in your party" must precede the generic
        // "<type> you control" arm — the trailing "in your party" is what
        // distinguishes party-size from a controlled-creature count.
        parse_creatures_in_your_party_tail,
        // CR 400.7 + CR 700.4 + CR 701.21a: entered-this-turn, died-this-turn,
        // and sacrificed-this-turn zone-change counts share a nested alt to stay
        // within nom's top-level `alt` arity (nom 8.0 max: 21 items).
        // All three arms must precede `parse_number_of_controlled_type` so the
        // leading type-word token does not commit to the generic controlled-type arm.
        alt((
            parse_entered_this_turn_ref,
            parse_number_of_creatures_died_this_turn,
            parse_number_of_sacrificed_this_turn,
        )),
        parse_tokens_created_this_turn_tail,
        parse_number_of_distinct_colors_among_permanents_tail,
        // CR 107.1 + CR 700.1: "[type] controlled by the player who controls
        // the fewest/most" — must precede `parse_number_of_controlled_type`,
        // whose " you control" suffix would otherwise not match but whose
        // type-word prefix overlaps.
        parse_controlled_by_extremum_player,
        // CR 604.3: "<type> of the chosen type on the battlefield" — global CDA
        // count; must precede `parse_number_of_controlled_type`, whose
        // " you control" suffix does not match the battlefield-wide form.
        parse_number_of_chosen_type_on_battlefield,
        // CR 604.3: "<type> on the battlefield with <keyword>" — global CDA
        // count restricted to a keyword; must precede
        // `parse_number_of_controlled_type`, whose " you control" suffix does
        // not match the battlefield-wide form.
        parse_number_of_type_on_battlefield_with_keyword,
        // CR 121.1 + CR 701.9 + CR 603.4: "cards you've drawn this turn" and
        // "cards you've discarded this turn" — must precede generic
        // controlled-type arms whose type words could overlap. Nested together
        // to stay within nom's top-level `alt` arity (nom 8.0 max: 21 items).
        alt((
            parse_number_of_cards_drawn_this_turn,
            parse_number_of_cards_discarded_this_turn,
        )),
        parse_number_of_controlled_type,
        parse_cards_exiled_with_source,
        // CR 109.4 + CR 115.7 + CR 402.1: "cards in …" hand/zone counts share a
        // nested alt to stay within nom's top-level `alt` arity (nom 8.0 max: 21
        // items). Ordering within the nest is load-bearing: chosen-player and
        // extremum-hand phrases must precede the generic target-zone and zone
        // arms they share a "cards in " prefix with.
        alt((
            parse_number_of_cards_in_chosen_player_zone,
            // CR 402.1: "cards in the hand of the {player|opponent} with the
            // {most|fewest} cards in hand" (Adamaro P/T CDA class).
            parse_number_of_cards_in_hand_of_extremum_player,
            parse_number_of_cards_in_target_zone,
            parse_number_of_cards_in_all_players_hands,
            parse_number_of_cards_in_zone,
        )),
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
    // CR 702.167c + CR 105.1: "the number of colors among the exiled cards used
    // to craft it" — distinct colors over the craft-material linked-exile pool
    // (Sunbird Effigy P/T). Tried before the generic type-phrase filter so the
    // craft noun phrase wins.
    if let Ok((craft_rest, filter)) = parse_craft_materials_filter(rest) {
        if matches!(craft_rest.trim(), "" | "." | ",") {
            return Ok(("", QuantityRef::DistinctColorsAmongPermanents { filter }));
        }
    }
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
/// hand-zone analogue is `HandSize { AllPlayers { aggregate } }`, parsed by
/// [`parse_player_with_extremum_cards_in_hand`].
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

/// CR 402.1 + CR 102.2/102.3: Shared core for cross-player hand-size extrema.
/// Two independent nom axes — population scope (`player` ↔ `opponent`) and
/// aggregate direction (`most` ↔ `fewest`) — plus the fixed "cards in hand"
/// zone suffix (CR 402). The hand-zone peer of `parse_cross_player_life_extremum`
/// (the life axis, CR 119): routes to `HandSize`/`PlayerScope`, never the CR
/// 208/202 object-property `Aggregate`.
fn parse_extremum_hand_size_scope_and_aggregate(input: &str) -> OracleResult<'_, PlayerScope> {
    let (rest, player) = alt((
        map(
            (
                tag("player"),
                tag(" with the "),
                alt((
                    value(AggregateFunction::Max, tag("most")),
                    value(AggregateFunction::Min, tag("fewest")),
                )),
            ),
            |(_, _, aggregate)| PlayerScope::AllPlayers {
                aggregate,
                exclude: None,
            },
        ),
        map(
            (
                tag("opponent"),
                tag(" with the "),
                alt((
                    value(AggregateFunction::Max, tag("most")),
                    value(AggregateFunction::Min, tag("fewest")),
                )),
            ),
            |(_, _, aggregate)| PlayerScope::Opponent { aggregate },
        ),
    ))
    .parse(input)?;
    let (rest, _) = tag(" cards in hand").parse(rest)?;
    Ok((rest, player))
}

/// CR 402.1: Parse "the {player|opponent} with the {most|fewest} cards in hand"
/// → `QuantityRef::HandSize`. Used by the catch-up-draw interceptor (Tales of
/// the Ancestors) and any card naming the short cross-player hand-size extremum.
pub(crate) fn parse_player_with_extremum_cards_in_hand(
    input: &str,
) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("the ").parse(input)?;
    let (rest, player) = parse_extremum_hand_size_scope_and_aggregate(rest)?;
    Ok((rest, QuantityRef::HandSize { player }))
}

/// CR 402.1: Parse "cards in the hand of the {player|opponent} with the
/// {most|fewest} cards in hand" after "the number of" → `QuantityRef::HandSize`.
/// Verbose wrapper for P/T CDAs (Adamaro, First to Desire).
fn parse_number_of_cards_in_hand_of_extremum_player(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("cards in the hand of the ").parse(input)?;
    let (rest, player) = parse_extremum_hand_size_scope_and_aggregate(rest)?;
    Ok((rest, QuantityRef::HandSize { player }))
}

/// Parse "[type(s)] you control" / "[type(s)] the chosen player controls" after
/// "the number of". CR 613.1: "the chosen player" is the player persisted on the
/// source via `ChosenAttribute::Player` (Skyshroud War Beast, Lost Order of
/// Jarkeld), distinct from the controller ("you control").
fn parse_number_of_controlled_type(input: &str) -> OracleResult<'_, QuantityRef> {
    if let Ok(parsed) = parse_qualified_controlled_type(input) {
        return Ok(parsed);
    }

    let (rest, head) = parse_type_filter_word(input)?;
    let (rest, controller) = parse_quantity_controller_suffix(rest)?;
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
                controller: Some(controller),
                properties: Vec::new(),
            }),
        },
    ))
}

/// CR 201.2 + CR 109.2: Parse qualified controlled object counts like
/// "permanents named Food Fight you control" or "other creature named Seven
/// Dwarves you control". The named/card-quality parser (`parse_type_phrase`)
/// owns the object description — type word plus any `other`/`named X`
/// qualifier — and this quantity parser owns the trailing controller scope.
/// Shared by the "the number of … you control" and "for each … you control"
/// paths: a `named X` qualifier sits between the type word and the controller
/// suffix, which the bare-`parse_type_filter_word` arms cannot reach.
fn parse_qualified_controlled_type(input: &str) -> OracleResult<'_, QuantityRef> {
    let (mut filter, rest) = parse_type_phrase(input);
    if !quantity_filter_has_meaningful_content(&filter) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }

    let (rest, chosen_prop) = opt(parse_pre_controller_chosen_filter_suffix).parse(rest)?;
    let (rest, controller) = parse_quantity_controller_suffix(rest)?;
    if let Some(prop) = chosen_prop {
        attach_property_to_quantity_filter(&mut filter, prop);
    }
    attach_controller_to_quantity_filter(&mut filter, controller);
    Ok((rest, QuantityRef::ObjectCount { filter }))
}

/// CR 604.3 + CR 613.1: Parse "<type> of the chosen type [on the battlefield]"
/// after "the number of" → a battlefield-wide (any-controller) population count
/// of permanents whose subtypes include the source's chosen creature type.
///
/// Distinct from `parse_number_of_controlled_type`, whose " you control" suffix
/// restricts the count to a single controller. This is the global form that
/// backs characteristic-defining power/toughness abilities such as Caller of
/// the Hunt ("~'s power and toughness are each equal to the number of creatures
/// of the chosen type on the battlefield"). The chosen type is read at
/// evaluation time via `FilterProp::IsChosenCreatureType` (mirrors the existing
/// "<type> you control of the chosen type" filter), so this covers every CDA in
/// the class, not a single card.
///
/// Prefix variants such as "other"/"another"/"non-X"/"legendary" are
/// intentionally out of scope for this global chosen-type CDA class; this mirrors
/// the controlled chosen-type sibling below and avoids shadowing its controller
/// suffix.
fn parse_number_of_chosen_type_on_battlefield(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, head) = parse_type_filter_word(input)?;
    let (rest, _) = alt((tag(" of the chosen type"), tag(" of that type"))).parse(rest)?;
    // CR 400.1: the population is battlefield-wide; tolerate an explicit
    // " on the battlefield" scope phrase without altering the default
    // battlefield zone of the resulting `ObjectCount`.
    let (rest, _) = opt(tag(" on the battlefield")).parse(rest)?;
    Ok((
        rest,
        QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![head],
                controller: None,
                properties: vec![FilterProp::IsChosenCreatureType],
            }),
        },
    ))
}

/// CR 604.3: Parse "<type> on the battlefield with <keyword>" after "the
/// number of" → a battlefield-wide (any-controller) population count of
/// permanents of the given type that have the named keyword.
///
/// Sibling of `parse_number_of_chosen_type_on_battlefield`: same global
/// (`controller: None`) battlefield population, but the predicate is a keyword
/// rather than the chosen creature type. Backs characteristic-defining
/// power/toughness abilities such as Dauthi Warlord ("~'s power is equal to the
/// number of creatures on the battlefield with shadow"). Generalized over every
/// evergreen keyword via `parse_keyword_name` + `FilterProp::WithKeyword`, so it
/// covers the whole class, not one card.
fn parse_number_of_type_on_battlefield_with_keyword(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, head) = parse_type_filter_word(input)?;
    let (rest, _) = tag(" on the battlefield with ").parse(rest)?;
    let (rest, keyword_name) = parse_keyword_name(rest)?;
    let keyword: Keyword = keyword_name.parse().unwrap();
    Ok((
        rest,
        QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![head],
                controller: None,
                properties: vec![FilterProp::WithKeyword { value: keyword }],
            }),
        },
    ))
}

/// CR 613.1: Parse "cards in the chosen player's <zone>" after "the number of"
/// into the general zone-count building block scoped to the source's persisted
/// chosen player.
fn parse_number_of_cards_in_chosen_player_zone(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("cards in the chosen player's ").parse(input)?;
    let (rest, zone) = parse_zone_ref_singular(rest)?;
    Ok((
        rest,
        QuantityRef::ZoneCardCount {
            zone,
            card_types: Vec::new(),
            scope: CountScope::SourceChosenPlayer,
            filter: None,
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

/// CR 506.2 + CR 402: Parse "defending player's hand" → defending-player hand
/// size. Mr. Foxglove's "the number of cards in defending player's hand" — the
/// possessive references the player being attacked (CR 506.2 defines the
/// defending player), resolved at runtime via `PlayerScope::DefendingPlayer`.
/// Does not consume the leading "cards in " — the caller
/// (`parse_zone_card_count`) has stripped that prefix.
fn parse_defending_player_hand_count(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("defending player's ").parse(input)?;
    let (rest, _) = tag("hand").parse(rest)?;
    Ok((
        rest,
        QuantityRef::HandSize {
            player: PlayerScope::DefendingPlayer,
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
        if let Ok((after_zone, q)) = parse_defending_player_hand_count(rest) {
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
            filter: None,
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

/// CR 608.2c + CR 205.2a: "card type[s] among cards <verb> this way" -> distinct
/// card types among the chain tracked set, cause-filtered to <verb> (Occult Epiphany #3307).
pub(crate) fn parse_distinct_card_types_among_tracked_set(
    input: &str,
) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("card type").parse(input)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    let (rest, _) = tag(" among cards ").parse(rest)?;
    let (rest, cause) = alt((
        value(ThisWayCause::Discarded, tag("discarded")),
        value(ThisWayCause::Exiled, tag("exiled")),
        value(ThisWayCause::Milled, tag("milled")),
        value(ThisWayCause::Destroyed, tag("destroyed")),
        value(ThisWayCause::Sacrificed, tag("sacrificed")),
    ))
    .parse(rest)?;
    let (rest, _) = tag(" this way").parse(rest)?;
    Ok((
        rest,
        QuantityRef::DistinctCardTypes {
            source: CardTypeSetSource::TrackedSet {
                caused_by: Some(cause),
            },
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
    // Singular "opponent who lost life this turn" (Gev, Scaled Scorch's per-each
    // counter scaling) and plural "opponents who …" (Belbe, Corrupted Observer)
    // resolve to the same `PlayerCount` over the qualifying-opponents set.
    let (rest, _) = alt((tag("opponents "), tag("opponent "))).parse(rest)?;
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

/// CR 119.3 + CR 603.2c: "1 life you gained" / "1 life you lost" — the per-1
/// multiplier in a "for each 1 life you gained/lost" clause on a
/// `Whenever you gain/lose life` trigger. The triggering `GameEvent::LifeChanged`
/// carries the gained/lost magnitude, which `EventContextAmount` resolves via
/// `extract_amount_from_event` (`game/targeting.rs`: `LifeChanged` => `amount.abs()`).
/// The leading "1 "/"one " disambiguates from the duration class "life you
/// gained/lost this turn" (`LifeGainedThisTurn`/`LifeLostThisTurn`, which has no
/// "1 ") and from Blood Tyrant's "1 life lost or gained this way" (no "you";
/// handled by the `TrackedSetSize` "this way" block).
fn parse_for_each_one_life_changed(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((tag("1 life you "), tag("one life you "))).parse(input)?;
    value(
        QuantityRef::EventContextAmount,
        alt((tag("gained"), tag("lost"))),
    )
    .parse(rest)
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

/// Parse the possessive form of a source self-reference: "its", "~'s",
/// "this creature's", "this card's", or a gendered/plural pronoun ("his",
/// "her", "their").
///
/// CR 208.3 + CR 608.2k: A creature's ability that says "his power" / "her
/// power" / "their power" refers to that same source object's power (recently
/// templated this way on Marvel's Spider-Man cards such as Iron Fist, Living
/// Weapon). The gendered/plural pronouns are interchangeable with the neuter
/// "its" for the purpose of referencing the ability's own source — modern
/// templating used "its" exclusively, so admitting the gendered forms here
/// keeps the whole "his/her/their <characteristic>" class on one path rather
/// than special-casing one card.
fn parse_self_possessive(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag("its"),
            tag("~'s"),
            tag("this creature's"),
            tag("this card's"),
            tag("his"),
            tag("her"),
            tag("their"),
        )),
    )
    .parse(input)
}

/// Parse "its power" / "~'s power" / "this creature's power" / "this card's
/// power" / "his power" / "her power" / "their power".
///
/// CR 400.7 + CR 208.3: Scavenge and other graveyard-activated effects reference
/// the source via "this card's power" because the source is a card (not a
/// creature) when the ability is activated. `SelfPower` is LKI-aware at
/// resolution time (see `game/quantity.rs`), so all phrasings resolve
/// identically. See `parse_self_possessive` for the gendered-pronoun rationale.
fn parse_self_power_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = parse_self_possessive(input)?;
    let (rest, _) = tag(" power").parse(rest)?;
    Ok((
        rest,
        QuantityRef::Power {
            scope: crate::types::ability::ObjectScope::Source,
        },
    ))
}

/// Parse "its toughness" / "~'s toughness" / "this creature's toughness" /
/// "this card's toughness" / "his toughness" / "her toughness" /
/// "their toughness". See `parse_self_power_ref` for the card-vs-creature
/// rationale and `parse_self_possessive` for the gendered-pronoun rationale.
fn parse_self_toughness_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = parse_self_possessive(input)?;
    let (rest, _) = tag(" toughness").parse(rest)?;
    Ok((
        rest,
        QuantityRef::Toughness {
            scope: crate::types::ability::ObjectScope::Source,
        },
    ))
}

/// Parse damage-history references such as Chandra's Incinerator's
/// "total amount of noncombat damage dealt to your opponents this turn" and
/// Knollspine Dragon's "damage dealt to target opponent this turn".
///
/// CR 120.9 + CR 115.1: "damage dealt" refers only to damage dealt to the
/// specified target opponent (115.1 targeting); the count aggregates all such
/// damage this turn (120.9 specified-source semantics).
fn parse_damage_dealt_this_turn_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (input, _) = opt(tag("the ")).parse(input)?;
    alt((
        value(
            QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::Any),
                target: Box::new(TargetFilter::And {
                    filters: vec![
                        TargetFilter::Player,
                        TargetFilter::Typed(
                            TypedFilter::default().controller(ControllerRef::Opponent),
                        ),
                    ],
                }),
                aggregate: AggregateFunction::Sum,
                group_by: None,
                damage_kind: DamageKindFilter::NoncombatOnly,

                excess_only: false,
            },
            tag("total amount of noncombat damage dealt to your opponents this turn"),
        ),
        value(
            QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::Any),
                target: Box::new(TargetFilter::And {
                    filters: vec![
                        TargetFilter::Player,
                        TargetFilter::Typed(
                            TypedFilter::default().controller(ControllerRef::TargetPlayer),
                        ),
                    ],
                }),
                aggregate: AggregateFunction::Sum,
                group_by: None,
                damage_kind: DamageKindFilter::Any,

                excess_only: false,
            },
            tag("damage dealt to target opponent this turn"),
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
        // CR 115.1 + CR 115.10 + CR 119.3 + CR 608.2c: Third-person "they" / "that
        // player" anaphor in a life-change clause refers to the player the
        // surrounding LoseLife/GainLife AFFECTS, never the source's controller. In
        // a TARGETED clause ("target opponent loses life equal to the life that
        // player lost this turn" — Blitzwing, Cruel Tormentor; Astarion Feed) that
        // is the player TARGET (CR 115.1), read from `ability.targets`. In a
        // per-opponent ITERATION ("each opponent loses life equal to the life they
        // lost this turn" — Wound Reflection, Archfiend of Despair, Warlock Class
        // L3) the affected player is not a target (CR 115.10a);
        // `rewrite_player_scope_refs` rebinds this `Target` form to `ScopedPlayer`
        // under the lifted `player_scope` loop, mirroring the "each opponent loses
        // half their life" (Betor / Blood Tribute) `LifeTotal` rewrite. Emitting
        // `Target` here (not `Controller`) is what lets both the targeted and the
        // iterated context resolve to each affected player's OWN life lost this
        // turn. (`LifeGainedThisTurn` has no third-person printing today; this is
        // its symmetric extension point should one appear.)
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Target,
            },
            tag("the life that player lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Target,
            },
            tag("the life they lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Target,
            },
            tag("the amount of life they lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Target,
            },
            tag("the life that player lost"),
        ),
        value(
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Target,
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
    // CR 202.3 + CR 115.1: "mana value of target <filter>" — a count whose value
    // reads the object chosen for this ref's OWN target slot (Fateful Handoff,
    // Knollspine Dragon). Tried before the bare possessive scope so the
    // "target ..." object phrase is captured via the shared `parse_target`
    // building block. Only fires when the phrase actually used the "target"
    // keyword; the bare "that creature's mana value" possessive stays
    // `ObjectManaValue { scope: Target }`.
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("mana value of "),
        tag("converted mana cost of "),
    ))
    .parse(input)
    {
        let (after, filter) = parse_target_with_syntax_target_keyword(rest)?;
        return Ok((
            after,
            QuantityRef::TargetObjectManaValue {
                filter: Box::new(filter),
            },
        ));
    }

    let (rest, scope) = parse_object_possessive_scope(input)?;
    let (rest, _) = alt((tag(" mana value"), tag(" converted mana cost"))).parse(rest)?;
    Ok((rest, QuantityRef::ObjectManaValue { scope }))
}

/// Bridge the `parse_target` building block into the nom `OracleResult` world,
/// requiring the phrase to have used the "target" keyword (CR 115.1). Returns
/// `oracle_err` when the remainder is not a targeted object phrase so the caller
/// falls through to the bare-possessive path.
fn parse_target_with_syntax_target_keyword(input: &str) -> OracleResult<'_, TargetFilter> {
    let mut ctx = ParseContext::default();
    let (filter, rest, syntax) = parse_target_with_syntax(input, &mut ctx);
    if syntax != TargetSyntax::TargetKeyword {
        return Err(oracle_err(input));
    }
    Ok((rest, filter))
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
    // Possessive form: "[the] (sacrificed|…) (permanent|…)'s (mana value|power|…)"
    let (rest, _) = opt(tag("the ")).parse(input)?;
    let (rest, _) = parse_cost_paid_participle_noun(rest)?;
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
        // ManaSymbolCount is produced only via `QuantityRef::Aggregate`, never
        // as a single cost-paid-object reference.
        ObjectProperty::ManaSymbolCount(_) => return Err(oracle_err(input)),
    };
    Ok((rest, qty))
}

/// CR 202.3 + CR 608.2k + CR 400.7j: Prepositional cost-paid mana-value form,
/// e.g. Morbid Curiosity's "the mana value of the sacrificed permanent".
///
/// Mirrors the possessive `parse_cost_paid_object_ref` but reads
/// `[the] mana value of the (sacrificed|exiled|discarded|milled) (creature|permanent|…)`.
/// Reuses the shared participle+noun combinator so both prepositional and
/// possessive front-forms resolve the same `ObjectScope::CostPaidObject` ref.
/// Power/toughness have no idiomatic prepositional Oracle phrasing, so this arm
/// only emits the mana-value reference.
fn parse_cost_paid_object_prepositional_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = opt(tag("the ")).parse(input)?;
    let (rest, _) = alt((
        tag("mana value of the "),
        tag("converted mana cost of the "),
    ))
    .parse(rest)?;
    let (rest, _) = parse_cost_paid_participle_noun(rest)?;
    Ok((
        rest,
        QuantityRef::ObjectManaValue {
            scope: ObjectScope::CostPaidObject,
        },
    ))
}

/// CR 208.1 + CR 608.2 + CR 608.2k + CR 400.7j: Prepositional power/toughness of
/// the additional-cost CHOSEN-or-REVEALED (beheld) object.
///
/// Covers the "behold an object as a cost, then deal damage equal to its power"
/// class where the spell body refers to the beheld object by the choose/reveal
/// verbs rather than the sacrifice/exile/mill participles handled by
/// `parse_cost_paid_object_ref`:
///   - "the power of the chosen creature or card"               (Close Encounter)
///   - "the power of the creature you chose or the card you revealed" (Monstrous Emergence)
///
/// The beheld object is stamped as this ability's `cost_paid_object` by
/// `handle_behold_for_cost` (CR 400.7j: a cost that reveals/moves an object in a
/// public zone makes that object findable by the spell's effects), so the
/// referent resolves to `ObjectScope::CostPaidObject`. CR 208.1 + CR 608.2:
/// power/toughness are read at resolution from that snapshot. The leading
/// "the {power|toughness} of " preposition mirrors
/// `parse_cost_paid_object_prepositional_ref` (mana value); the object phrase is
/// its own `alt()` axis so a new beheld-object phrasing extends one branch.
fn parse_cost_paid_object_chosen_revealed_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = opt(tag("the ")).parse(input)?;
    let (rest, property) = alt((
        value(ObjectProperty::Power, tag("power of ")),
        value(ObjectProperty::Toughness, tag("toughness of ")),
    ))
    .parse(rest)?;
    let (rest, _) = parse_chosen_revealed_object_phrase(rest)?;
    let qty = match property {
        ObjectProperty::Power => QuantityRef::Power {
            scope: ObjectScope::CostPaidObject,
        },
        ObjectProperty::Toughness => QuantityRef::Toughness {
            scope: ObjectScope::CostPaidObject,
        },
        // The leading `alt` only emits Power/Toughness; ManaValue and
        // ManaSymbolCount are unreachable here.
        ObjectProperty::ManaValue | ObjectProperty::ManaSymbolCount(_) => {
            return Err(oracle_err(input))
        }
    };
    Ok((rest, qty))
}

/// Object phrase for the choose/reveal behold referent. Each form names the same
/// single beheld object (CR 608.2k) via the disjunction printed on the card:
///   - "the chosen creature or card"                     (Close Encounter)
///   - "the creature you chose or the card you revealed" (Monstrous Emergence)
///
/// The two legs of each disjunction are alternative descriptions of the SAME
/// stamped `cost_paid_object` (a creature chosen on the battlefield OR a card
/// chosen/revealed elsewhere), so the whole phrase collapses to one referent
/// rather than a multi-object set.
fn parse_chosen_revealed_object_phrase(input: &str) -> OracleResult<'_, ()> {
    alt((
        value((), tag("the chosen creature or card")),
        value((), tag("the creature you chose or the card you revealed")),
    ))
    .parse(input)
}

/// Shared participle + noun matcher for the cost-paid / event-context object
/// class. Each axis is a single `alt()` over independent variants — adding a
/// participle or noun extends one branch and both the possessive and
/// prepositional arms inherit it.
///
/// CR 701.17a: "milled" — card moved library → graveyard by the mill action.
/// "returned" names an object moved to another zone by a previous instruction.
fn parse_cost_paid_participle_noun(input: &str) -> OracleResult<'_, ()> {
    let (rest, _) = alt((
        alt((
            tag("sacrificed "),
            tag("exiled "),
            tag("discarded "),
            tag("milled "),
            tag("targeted "),
        )),
        alt((
            tag("destroyed "),
            tag("countered "),
            tag("returned "),
            tag("revealed "),
            tag("drawn "),
            tag("copied "),
            tag("discovered "),
        )),
    ))
    .parse(input)?;
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
    Ok((rest, ()))
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
        ObjectProperty::Power | ObjectProperty::Toughness | ObjectProperty::ManaSymbolCount(_) => {
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
        // CR 208.3 + CR 608.2k: "that spell's power"/"toughness" — the cast
        // event's source object IS the spell on the stack, and a creature spell
        // has the power/toughness printed on its card (CR 208.3), so these read
        // directly off the trigger-condition referent (CostPaidObject, the same
        // CR 608.2k scope as "that creature's power"/"mana value" above). Covers
        // the class of "Whenever you cast a creature spell, if that spell's
        // power is N or greater, …" cards (Eshki, Temur's Roar — issue #2009).
        value(
            QuantityRef::Power {
                scope: ObjectScope::CostPaidObject,
            },
            tag("that spell's power"),
        ),
        value(
            QuantityRef::Toughness {
                scope: ObjectScope::CostPaidObject,
            },
            tag("that spell's toughness"),
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
    they_controller: ControllerRef,
) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("basic land type").parse(input)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    let (rest, _) = tag(" among lands ").parse(rest)?;
    let (rest, controller) = alt((
        value(ControllerRef::You, tag("you control")),
        // The caller supplies the anaphoric "they control" binding: the iterating
        // player inside a `for each` clause, or a target player in "the number of …".
        value(they_controller, tag("they control")),
    ))
    .parse(rest)?;
    Ok((rest, QuantityRef::BasicLandTypeCount { controller }))
}

/// Parse "the number of basic land types among lands you control" (Domain).
fn parse_basic_land_type_count(input: &str) -> OracleResult<'_, QuantityRef> {
    preceded(
        tag("the number of "),
        // In a quantity reference, anaphoric "they control" binds to a target
        // player rather than a `for each` scoped player.
        |i| parse_basic_land_types_among_lands_controlled_by_ref(i, ControllerRef::TargetPlayer),
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

/// CR 700.5: Chroma — "the number of \<color\> mana symbols in the mana costs of
/// permanents you control" counts the same colored mana symbols among permanents
/// you control as devotion, so it maps to the existing `Devotion` quantity
/// (Outrage Shaman, Primalcrux). The graveyard-scope and single-object Chroma
/// forms are a different population and intentionally not matched here.
fn parse_chroma_devotion_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("the number of ").parse(input)?;
    let (rest, color) = super::primitives::parse_color(rest)?;
    let (rest, _) = tag(" mana symbols in the mana costs of permanents you control").parse(rest)?;
    Ok((
        rest,
        QuantityRef::Devotion {
            colors: DevotionColors::Fixed(vec![color]),
        },
    ))
}

/// CR 202.1 + CR 404.2: Graveyard-scope Chroma — "the number of \<color\> mana symbols in
/// the mana costs of cards in your graveyard" counts colored mana symbols among
/// cards in the owner's graveyard. Distinct from the permanents-scope
/// Chroma (devotion, CR 700.5).
fn parse_graveyard_chroma_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("the number of ").parse(input)?;
    let (rest, color) = super::primitives::parse_color(rest)?;
    let (rest, _) =
        tag(" mana symbols in the mana costs of cards in your graveyard").parse(rest)?;
    // CR 107.4a + CR 202.1: graveyard-scope chroma is the SUM of per-card
    // colored-mana-symbol counts over cards in your graveyard — expressed via the
    // zone-general `Aggregate` / `ObjectProperty::ManaSymbolCount` building block
    // rather than a graveyard-specific `QuantityRef` leaf. The `InZone { Graveyard }`
    // filter makes `Aggregate` scan the graveyard.
    //
    // CR 404.2: a graveyard is a zone owned by a single player; "your graveyard" is
    // the graveyard you own. Scope the population with `Owned { You }` (matches by
    // owner) rather than `.controller(You)`: a card in a graveyard is neither on the
    // stack nor the battlefield, so the controller filter reads the at-departure
    // controller via LKI (CR 109.4), which can diverge from ownership (e.g. a card
    // you owned but an opponent controlled before it died into your graveyard, or
    // one you controlled before it left for theirs). Ownership is the correct,
    // LKI-independent axis here.
    Ok((
        rest,
        QuantityRef::Aggregate {
            function: AggregateFunction::Sum,
            property: ObjectProperty::ManaSymbolCount(color),
            filter: TargetFilter::Typed(TypedFilter::card().properties(vec![
                FilterProp::Owned {
                    controller: ControllerRef::You,
                },
                FilterProp::InZone {
                    zone: Zone::Graveyard,
                },
            ])),
        },
    ))
}

/// Parse "equal to [quantity]" from Oracle text.
///
/// Returns the quantity expression following "equal to ".
pub fn parse_equal_to(input: &str) -> OracleResult<'_, QuantityExpr> {
    let (rest, _) = tag("equal to ").parse(input)?;
    // Try to parse sum expressions first: "the number of X and the number of Y"
    if let Ok((rest, sum_expr)) = parse_equal_to_sum(rest) {
        return Ok((rest, sum_expr));
    }
    parse_quantity(rest)
}

/// Parse sum expressions like "the number of X and the number of Y".
/// Each summand is prefixed with "the number of" to avoid greedy type-list
/// consumption by parse_the_number_of.
fn parse_equal_to_sum(input: &str) -> OracleResult<'_, QuantityExpr> {
    let (rest, refs) = separated_list1(tag(" and "), parse_the_number_of).parse(input)?;
    if refs.len() < 2 {
        return Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((
        rest,
        QuantityExpr::Sum {
            exprs: refs
                .into_iter()
                .map(|qty| QuantityExpr::Ref { qty })
                .collect(),
        },
    ))
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

/// Parse "for each differently named <type>" patterns.
/// Used for patterns like "for each differently named dungeon you've completed".
/// CR 201.2: Distinct-by-name population count.
fn parse_for_each_differently_named(input: &str) -> OracleResult<'_, QuantityRef> {
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

/// Parse "different <quality> among <type-phrase>" patterns (distinct-value
/// population count). Used for "for each different power among creatures you
/// control" (Golden Ratio), "different mana value among nonland permanents you
/// control" (Lunar Insight), "different mana value among nonland cards in your
/// graveyard" (Sudden Insight), and the "the number of different powers among
/// creatures you control" form (Celebrate the Harvest). The quality-generalized
/// sibling of `parse_for_each_differently_named` (which is the Name case).
/// CR 201.2 + CR 603.4: Distinct-by-quality population count.
fn parse_distinct_quality_among_objects(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("different ").parse(input)?;
    let (rest, quality) = parse_shared_quality(rest)?;
    let (rest, _) = tag(" among ").parse(rest)?;
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
            qualities: vec![quality],
        },
    ))
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

/// CR 608.2c + CR 609.3: Read the Runes — "for each card[s] drawn this way".
fn parse_for_each_card_drawn_this_way(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((tag("card drawn this way"), tag("cards drawn this way"))).parse(input)?;
    Ok((rest, QuantityRef::EventContextAmount))
}

fn parse_for_each_clause_ref_with_they_controller(
    input: &str,
    they_controller: ControllerRef,
) -> OracleResult<'_, QuantityRef> {
    alt((
        parse_for_each_card_drawn_this_way,
        alt((
            parse_for_each_one_life_changed,
            parse_for_each_opponents_life_change,
            parse_counter_added_this_turn_for_each,
            parse_color_of_object_for_each,
            parse_object_colors_for_each,
            parse_object_name_word_count_for_each,
            parse_object_typeline_component_count_for_each,
            parse_mana_symbols_in_object_mana_cost_for_each,
            parse_distinct_card_types_in_zone,
            parse_foretold_cards_owned_in_exile,
            parse_zone_card_count,
            parse_for_each_attached_to_source,
            // CR 201.2: "for each differently named <type>" — distinct-by-name
            // iteration. Must precede generic type-filter arm.
            parse_for_each_differently_named,
            // CR 201.2 + CR 603.4: "for each different <power|mana value> among <type>"
            // — distinct-by-quality count (Golden Ratio, Lunar Insight, Sudden
            // Insight). Must precede the generic type-filter arm so the "different
            // <quality>" adjective prefix is consumed before the bare type word.
            parse_distinct_quality_among_objects,
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
            // CR 701.21a: "[type] you['ve] sacrificed this turn" — event-based count
            // of sacrifice events. Must precede `parse_for_each_controlled_type` so the
            // leading type token does not commit to the generic `<type> you control` arm.
            parse_for_each_sacrificed_this_turn,
            // CR 400.7 + CR 603.10a: "creature that left the battlefield under your
            // control this turn" — destination-agnostic zone-change count, distinct
            // from the graveyard-only "died" arm above.
            parse_for_each_creature_left_battlefield_this_turn,
            parse_entered_this_turn_ref,
        )),
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
        // CR 122.1: "counter(s) on [self-ref]" — any counter type on the source
        // permanent (Gavel of the Righteous: "for each counter on this Equipment").
        // Placed before `parse_for_each_controlled_type` so the bare "counter" token
        // does not commit to a type-phrase fallback.
        parse_for_each_counters_on_source,
        // CR 305.6: "for each basic land type among lands you/they control" —
        // domain scaling (Jodah's Codex, Wandering Treefolk, Radha's Firebrand,
        // Scion of Draco). Reuses the shared bare-domain-suffix combinator and
        // must precede the generic `<type> you control` arm so the leading
        // "basic land type" is not mis-consumed as a creature/permanent type.
        // Anaphoric "they control" binds to the iterating/scoped player here.
        |i| parse_basic_land_types_among_lands_controlled_by_ref(i, they_controller.clone()),
        parse_for_each_controlled_type,
        // CR 201.2: "for each [other] <type> named <CardName> you control"
        // (Seven Dwarves). The `named X` qualifier sits between the type word
        // and " you control", so the bare-type `parse_for_each_controlled_type`
        // arm above cannot reach the controller suffix. Tried last so it only
        // catches the qualified case the bare-type arm rejects.
        parse_qualified_controlled_type,
    )))
    .parse(input)
}

/// CR 122.1: Parse "[counter-type] counter(s) on [self-ref]" and
/// "counter(s) on [self-ref]" in a "for each" context. Covers both typed
/// source-scoped costs like Tornado ("for each velocity counter on this
/// enchantment") and untyped source-scoped pumps like Gavel of the Righteous
/// ("for each counter on this Equipment").
fn parse_for_each_counters_on_source(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, counter_type) = alt((
        parse_typed_counter_type_for_each_source,
        value(None, parse_generic_counter_match),
    ))
    .parse(input)?;
    let (rest, _) = tag(" on ").parse(rest)?;
    let (rest, _) = parse_source_self_ref(rest)?;
    Ok((
        rest,
        QuantityRef::CountersOn {
            scope: ObjectScope::Source,
            counter_type,
        },
    ))
}

fn parse_typed_counter_type_for_each_source(input: &str) -> OracleResult<'_, Option<CounterType>> {
    let (rest, counter_type) = parse_counter_type_typed(input)?;
    let (rest, _) = parse_counter_word(rest)?;
    Ok((rest, Some(counter_type)))
}

/// CR 122.1: Match a source self-reference phrase: "~", "it", or any shared
/// self-reference type phrase from Oracle text.
fn parse_source_self_ref(input: &str) -> OracleResult<'_, ()> {
    if let Ok(result) = alt((
        value((), tag::<_, _, OracleError<'_>>("~")),
        value((), tag("it")),
    ))
    .parse(input)
    {
        return Ok(result);
    }

    for phrase in crate::parser::oracle_util::SELF_REF_TYPE_PHRASES {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(*phrase).parse(input) {
            return Ok((rest, ()));
        }
    }

    Err(nom::Err::Error(OracleError::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
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
        // CR 201.5: self-reference ("on ~" ← "on Beast") binds the counter-added
        // filter to the source object (Beast, Erudite Aerialist).
        value(TargetFilter::SelfRef, tag("~")),
        value(
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            // number axis × controller-phrase axis (PATTERNS.md §8b) — "creatures"
            // before "creature" since alt() is short-circuit.
            (
                alt((tag("creatures"), tag("creature"))),
                alt((tag(" under your control"), tag(" you control"))),
            ),
        ),
        value(
            TargetFilter::Typed(TypedFilter::creature()),
            alt((tag("creatures"), tag("creature"))),
        ),
        value(
            TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::You)),
            (
                alt((tag("permanents"), tag("permanent"))),
                alt((tag(" under your control"), tag(" you control"))),
            ),
        ),
        value(
            TargetFilter::Typed(TypedFilter::permanent()),
            alt((tag("permanents"), tag("permanent"))),
        ),
    ))
    .parse(rest)
}

/// CR 205.4a + CR 205.2a + CR 205.3: Parse "supertype, card type, and subtype
/// <object> has" (Embiggen) into a scoped typeline-component count.
fn parse_object_typeline_component_count_for_each(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) =
        tag::<_, _, OracleError<'_>>("supertype, card type, and subtype ").parse(input)?;
    let (rest, scope) = parse_object_typeline_scope(rest)?;
    let (rest, _) = tag(" has").parse(rest)?;
    Ok((rest, QuantityRef::ObjectTypelineComponentCount { scope }))
}

fn parse_object_typeline_scope(input: &str) -> OracleResult<'_, ObjectScope> {
    alt((parse_object_color_of_scope, parse_object_possessive_scope)).parse(input)
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
    Ok((
        rest,
        QuantityRef::ManaSymbolsInManaCost {
            scope,
            color: Some(color),
        },
    ))
}

/// CR 105.1 + CR 601.2f: "for each color[s] of <object>" — scoped object-color
/// count for cost reductions and similar per-color riders. Delegates object
/// binding to `parse_object_color_of_scope` (target/enchanted/equipped/it-
/// targets anaphors).
fn parse_color_of_object_for_each(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((tag("color of "), tag("colors of "))).parse(input)?;
    let (rest, scope) = parse_object_color_of_scope(rest)?;
    Ok((rest, QuantityRef::ObjectColorCount { scope }))
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
        value(FilterProp::Attacking { defender: None }, tag("attacking ")),
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
        value(FilterProp::Attacking { defender: None }, tag("attacking ")),
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
        value(
            ObjectScope::Target,
            (
                alt((tag("the "), tag("a "))),
                alt((tag("creature"), tag("permanent"))),
                tag(" it targets"),
            ),
        ),
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

/// CR 400.7 + CR 700.4: Parse a trailing "that died [under your control] this
/// turn" qualifier, returning the controller scope. "died" = battlefield→
/// graveyard (CR 700.4), applied as a constant zone pair at the construction site
/// exactly as `creatures_died_this_turn_ref` does. CR 109.5: "under your control"
/// scopes to the source's controller (`ControllerRef::You`); unqualified forms
/// return `None` (every player's deaths). Longer tags precede shorter so the
/// qualified suffix isn't shadowed by `alt`. Building block shared by the
/// aggregate this-turn-death quantity form.
pub(crate) fn parse_died_this_turn_suffix(input: &str) -> OracleResult<'_, Option<ControllerRef>> {
    alt((
        value(
            Some(ControllerRef::You),
            tag("that died under your control this turn"),
        ),
        value(
            Some(ControllerRef::You),
            tag("that died under your control"),
        ),
        value(None, tag("that died this turn")),
        value(None, tag("that died")),
    ))
    .parse(input)
}

/// CR 700.4: Shared tail for "creature(s) that died" / graveyard-from-battlefield
/// phrasing. Engine tracking is per-turn-only, so the trailing "this turn"
/// qualifier is semantically redundant when present.
///
/// Returns `(controller scope, nontoken-only)` where controller is
/// `Some(ControllerRef::You)` for forms qualified by "under your control" /
/// "your graveyard" (CR 109.5: "your" graveyard = the source's controller),
/// and `None` for unqualified forms that count every player's deaths. The
/// longer qualified tags MUST precede the bare "that died" /
/// "a graveyard" tags so the qualified suffix isn't shadowed by `alt`.
fn parse_creatures_died_this_turn_tail(
    input: &str,
) -> OracleResult<'_, (Option<ControllerRef>, bool)> {
    let (rest, nontoken) = opt(tag("nontoken ")).parse(input)?;
    let (rest, controller) = alt((
        value(
            Some(ControllerRef::You),
            tag("creatures that died under your control this turn"),
        ),
        value(
            Some(ControllerRef::You),
            tag("creatures that died under your control"),
        ),
        value(None, tag("creatures that died this turn")),
        value(None, tag("creatures that died")),
        value(
            Some(ControllerRef::You),
            tag("creature that died under your control this turn"),
        ),
        value(
            Some(ControllerRef::You),
            tag("creature that died under your control"),
        ),
        value(None, tag("creature that died this turn")),
        value(None, tag("creature that died")),
        // CR 700.4: "creature put into [a/your] graveyard from the battlefield"
        // is the long form of "died" — both reference the same battlefield→
        // graveyard transition tracked in `zone_changes_this_turn`. CR 109.5:
        // "your" graveyard scopes the count to the source's controller.
        value(
            Some(ControllerRef::You),
            tag("creatures put into your graveyard from the battlefield this turn"),
        ),
        value(
            Some(ControllerRef::You),
            tag("creatures put into your graveyard from the battlefield"),
        ),
        value(
            None,
            tag("creatures put into a graveyard from the battlefield this turn"),
        ),
        value(
            None,
            tag("creatures put into a graveyard from the battlefield"),
        ),
        value(
            Some(ControllerRef::You),
            tag("creature put into your graveyard from the battlefield this turn"),
        ),
        value(
            Some(ControllerRef::You),
            tag("creature put into your graveyard from the battlefield"),
        ),
        value(
            None,
            tag("creature put into a graveyard from the battlefield this turn"),
        ),
        value(
            None,
            tag("creature put into a graveyard from the battlefield"),
        ),
    ))
    .parse(rest)?;
    Ok((rest, (controller, nontoken.is_some())))
}

/// CR 700.4: Parse "creature(s) that died" → filtered zone-change count for
/// "for each creature that died this turn" iteration sources.
fn parse_for_each_creature_died_this_turn(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, (controller, nontoken)) = parse_creatures_died_this_turn_tail(input)?;
    Ok((rest, creatures_died_this_turn_ref(controller, nontoken)))
}

/// CR 700.4: Parse "the number of creature(s) that died this turn" → the same
/// `ZoneChangeCountThisTurn` quantity ref used by for-each iteration.
fn parse_number_of_creatures_died_this_turn(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, (controller, nontoken)) = parse_creatures_died_this_turn_tail(input)?;
    Ok((rest, creatures_died_this_turn_ref(controller, nontoken)))
}

/// CR 701.21a: Parse "[type] you['ve] sacrificed this turn" -> `TargetFilter`.
/// Shared inner combinator for both `parse_number_of_sacrificed_this_turn` and
/// `parse_for_each_sacrificed_this_turn`.
fn parse_sacrificed_this_turn_filter(input: &str) -> OracleResult<'_, TargetFilter> {
    // CR 701.21a: sacrifice moves the permanent directly to its owner's graveyard
    // (not destroyed — bypasses indestructible and regeneration).
    let (filter, rest) = parse_type_phrase(input);
    if !quantity_filter_has_meaningful_content(&filter) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }

    let (rest, _) = (tag(" you"), opt(tag("'ve")), tag(" sacrificed this turn")).parse(rest)?;
    Ok((rest, filter))
}

/// CR 701.21a: "the number of [type] you['ve] sacrificed this turn" →
/// `QuantityRef::SacrificedThisTurn`. Wired into the nested inner alt of
/// `parse_number_of_inner` alongside `parse_entered_this_turn_ref` and
/// `parse_number_of_creatures_died_this_turn`.
///
/// Structurally identical to `parse_for_each_sacrificed_this_turn` by convention
/// (mirrors the `parse_number_of_/parse_for_each_creature_died_this_turn` pair).
/// If opponent/any-player sacrifice forms are ever added, diverge the logic here.
fn parse_number_of_sacrificed_this_turn(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, filter) = parse_sacrificed_this_turn_filter(input)?;
    Ok((
        rest,
        QuantityRef::SacrificedThisTurn {
            player: PlayerScope::Controller,
            filter,
        },
    ))
}

/// CR 701.21a: "[type] you['ve] sacrificed this turn" in a "for each" context →
/// `QuantityRef::SacrificedThisTurn`. Separate named fn per the
/// `parse_number_of_/parse_for_each_creature_died_this_turn` convention.
///
/// Structurally identical to `parse_number_of_sacrificed_this_turn` by convention.
/// If opponent/any-player sacrifice forms are ever added, diverge the logic here.
fn parse_for_each_sacrificed_this_turn(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, filter) = parse_sacrificed_this_turn_filter(input)?;
    Ok((
        rest,
        QuantityRef::SacrificedThisTurn {
            player: PlayerScope::Controller,
            filter,
        },
    ))
}

/// CR 400.7 + CR 603.10a: Parse "creature that left the battlefield under your
/// control [this turn]" -> filtered zone-change count where the destination is
/// unconstrained ("left the battlefield" = battlefield -> *any* zone, unlike
/// "died" which is battlefield -> graveyard). CR 603.10a classes
/// leaves-the-battlefield as a look-back zone-change event, so the count is
/// taken over `zone_changes_this_turn` records using each object's last-known
/// characteristics.
///
/// "under your control" scopes the count to creatures controlled by the
/// source's controller at the time they left (`ControllerRef::You`). The
/// trailing "this turn" qualifier is engine-redundant (tracking is per-turn)
/// and is stripped upstream by `strip_trailing_duration`, mirroring
/// `parse_for_each_creature_died_this_turn`.
fn parse_for_each_creature_left_battlefield_this_turn(
    input: &str,
) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = alt((
        tag("creature that left the battlefield under your control this turn"),
        tag("creature that left the battlefield under your control"),
    ))
    .parse(input)?;
    Ok((
        rest,
        QuantityRef::ZoneChangeCountThisTurn {
            from: Some(Zone::Battlefield),
            to: None,
            filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        },
    ))
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

/// CR 700.4: "died" = put into a graveyard from the battlefield, so the count
/// is taken over `zone_changes_this_turn` records from battlefield to graveyard.
/// CR 109.5: when the phrasing is qualified by "under your control" / "your
/// graveyard", `controller` is `Some(ControllerRef::You)` and the count is
/// scoped to creatures controlled by the source's controller when they died;
/// otherwise it is `None` and every player's deaths are counted.
fn creatures_died_this_turn_ref(controller: Option<ControllerRef>, nontoken: bool) -> QuantityRef {
    let mut tf = TypedFilter::creature();
    if let Some(c) = controller {
        tf = tf.controller(c);
    }
    if nontoken {
        tf = tf.properties(vec![FilterProp::NonToken]);
    }
    QuantityRef::ZoneChangeCountThisTurn {
        from: Some(Zone::Battlefield),
        to: Some(Zone::Graveyard),
        filter: TargetFilter::Typed(tf),
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
        // CR 301.5a + CR 303.4: source-anaphoric gendered pronoun denotes the
        // ability source (same id as `~`) — Winter Soldier, Captain America
        // (MSH templates). Maps to AttachedToSource, identical to the `~` arm.
        // Distinct from the recipient pronoun "it"/"that creature" arm below.
        // Only the unambiguously source-anaphoric "him"/"her" are accepted; the
        // singular-they "them" is excluded because it is recipient-anaphoric for
        // player-enchanting Auras (Curse of Thirst: "Curses attached to them" =
        // the enchanted player, not the Aura source), which would bind the wrong
        // object set.
        value(
            FilterProp::AttachedToSource,
            alt((tag(" attached to him"), tag(" attached to her"))),
        ),
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
                properties: vec![FilterProp::Attacking {
                    defender: Some(ControllerRef::You),
                }],
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
    let (rest, shared_quality) = parse_shared_quality_clause(rest, &ParseContext::default())?;

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

    /// CR 400.7 + CR 700.4 + CR 109.5: the shared death-suffix combinator returns
    /// the controller scope for all four "that died" tag forms and rejects
    /// unrelated text.
    #[test]
    fn test_parse_died_this_turn_suffix_controller_scopes() {
        assert_eq!(
            parse_died_this_turn_suffix("that died under your control this turn").unwrap(),
            ("", Some(ControllerRef::You))
        );
        assert_eq!(
            parse_died_this_turn_suffix("that died under your control").unwrap(),
            ("", Some(ControllerRef::You))
        );
        assert_eq!(
            parse_died_this_turn_suffix("that died this turn").unwrap(),
            ("", None)
        );
        assert_eq!(
            parse_died_this_turn_suffix("that died").unwrap(),
            ("", None)
        );
        assert!(parse_died_this_turn_suffix("you control").is_err());
    }

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
    fn parse_object_property_aggregate_greatest_power() {
        let (rest, q) =
            parse_quantity_ref("the greatest power among dinosaurs you control").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            q,
            QuantityRef::Aggregate {
                function: AggregateFunction::Max,
                property: ObjectProperty::Power,
                ..
            }
        ));
    }

    /// CR 702.167c: the craft-material noun phrase recognizes every self-anaphor
    /// variant ("it" / "~" / "this <noun>") and rejects unrelated exile phrases.
    #[test]
    fn parse_craft_materials_filter_anaphors() {
        for phrase in [
            "the exiled card used to craft it",
            "the exiled cards used to craft it",
            "the exiled cards used to craft ~",
            "the exiled cards used to craft this creature",
            "the exiled cards used to craft this permanent",
            "the exiled cards used to craft this artifact",
        ] {
            let (rest, filter) = parse_craft_materials_filter(phrase)
                .unwrap_or_else(|e| panic!("craft phrase {phrase:?} should parse: {e:?}"));
            assert_eq!(rest, "", "craft phrase {phrase:?} must fully consume");
            assert_eq!(filter, linked_exile_owned_filter(), "phrase {phrase:?}");
        }
        // Bare exile anaphors (no "used to craft") must NOT match the craft form.
        assert!(parse_craft_materials_filter("the exiled cards").is_err());
        assert!(parse_craft_materials_filter("those exiled cards").is_err());
    }

    /// CR 702.167c + CR 208.1: "the total power of the exiled cards used to craft
    /// it" routes to the linked-exile aggregate, NOT the tracked-set anaphor
    /// (Mastercraft Raptor). The shared "the exiled cards" prefix must resolve to
    /// the craft pool when the craft suffix follows.
    #[test]
    fn parse_total_power_of_craft_materials_is_aggregate() {
        let (rest, q) =
            parse_quantity_ref("the total power of the exiled cards used to craft it").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::Aggregate {
                function: AggregateFunction::Sum,
                property: ObjectProperty::Power,
                filter,
            } => assert_eq!(filter, linked_exile_owned_filter()),
            other => panic!("expected craft-material power aggregate, got {other:?}"),
        }
    }

    /// CR 702.167c + CR 105.1: "the number of colors among the exiled cards used
    /// to craft it" routes to the distinct-colors ref over the craft pool
    /// (Sunbird Effigy P/T).
    #[test]
    fn parse_colors_among_craft_materials_is_distinct_colors() {
        let (rest, q) =
            parse_quantity_ref("the number of colors among the exiled cards used to craft it")
                .unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::DistinctColorsAmongPermanents { filter } => {
                assert_eq!(filter, linked_exile_owned_filter())
            }
            other => panic!("expected DistinctColorsAmongPermanents, got {other:?}"),
        }
    }

    /// CR 702.167c + CR 202.3: "the mana value of the exiled card used to craft
    /// it" still resolves to the linked-exile mana-value aggregate even with the
    /// craft suffix appended (Jadeheart Attendant).
    #[test]
    fn parse_mana_value_of_craft_material_is_aggregate() {
        let (rest, q) =
            parse_quantity_ref("the mana value of the exiled card used to craft it").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            q,
            QuantityRef::Aggregate {
                function: AggregateFunction::Sum,
                property: ObjectProperty::ManaValue,
                ..
            }
        ));
    }

    #[test]
    fn parse_max_quantity_whichever_greater() {
        let (rest, qty) = parse_max_quantity(
            "2 or the greatest power among dinosaurs you control, whichever is greater",
        )
        .expect("max-of-two quantity should parse");
        assert_eq!(rest, "");
        let QuantityExpr::Max { exprs } = qty else {
            panic!("expected QuantityExpr::Max, got {qty:?}");
        };
        assert_eq!(exprs.len(), 2);
        assert!(matches!(exprs[0], QuantityExpr::Fixed { value: 2 }));
        assert!(matches!(
            exprs[1],
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
    fn parse_max_quantity_rejects_bare_or() {
        assert!(parse_max_quantity("2 or the greatest power among dinosaurs you control").is_err());
    }

    #[test]
    fn parse_number_of_chosen_type_on_battlefield_global_count() {
        // CR 604.3: Caller of the Hunt — "the number of creatures of the chosen
        // type on the battlefield" is a battlefield-wide CDA count (any
        // controller), distinct from the " you control" controlled-type form.
        for text in [
            "the number of creatures of the chosen type on the battlefield",
            "the number of creatures of the chosen type",
        ] {
            let (rest, q) = parse_quantity_ref(text).unwrap();
            assert_eq!(rest, "", "{text:?} should fully consume");
            match q {
                QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(tf),
                } => {
                    assert_eq!(tf.controller, None, "{text:?}: counts every controller");
                    assert!(
                        tf.properties.contains(&FilterProp::IsChosenCreatureType),
                        "{text:?}: must gate on the source's chosen creature type"
                    );
                }
                other => panic!("{text:?}: expected ObjectCount, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_number_of_type_on_battlefield_with_keyword_global_count() {
        // CR 604.3: Dauthi Warlord — "the number of creatures on the
        // battlefield with shadow" is a battlefield-wide CDA count (any
        // controller) gated on a keyword, generalized over the KEYWORDS table.
        for (text, kw) in [
            (
                "the number of creatures on the battlefield with shadow",
                Keyword::Shadow,
            ),
            (
                "the number of creatures on the battlefield with flying",
                Keyword::Flying,
            ),
        ] {
            let (rest, q) = parse_quantity_ref(text).unwrap();
            assert_eq!(rest, "", "{text:?} should fully consume");
            match q {
                QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(tf),
                } => {
                    assert_eq!(tf.controller, None, "{text:?}: counts every controller");
                    assert!(
                        tf.properties
                            .contains(&FilterProp::WithKeyword { value: kw }),
                        "{text:?}: must gate on the named keyword"
                    );
                }
                other => panic!("{text:?}: expected ObjectCount, got {other:?}"),
            }
        }
    }

    /// CR 604.3 + CR 109.4: opponent-controlled and chosen-player CDA counts.
    #[test]
    fn parse_number_of_controlled_type_opponent_and_chosen_player_cda() {
        let (rest, q) = parse_quantity_ref("the number of Swamps your opponents control").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(tf),
            } => {
                assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Subtype("Swamp".into())));
            }
            other => panic!("expected ObjectCount, got {other:?}"),
        }

        let (rest, q) =
            parse_quantity_ref("the number of tapped lands the chosen player controls").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(tf),
            } => {
                assert_eq!(tf.controller, Some(ControllerRef::SourceChosenPlayer));
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert!(tf.properties.contains(&FilterProp::Tapped));
            }
            other => panic!("expected ObjectCount, got {other:?}"),
        }
    }

    /// CR 121.1 + CR 604.3: cards drawn this turn as a CDA quantity (Duelist of the Mind).
    #[test]
    fn parse_number_of_cards_drawn_this_turn_cda() {
        for text in [
            "the number of cards you've drawn this turn",
            "the number of cards you have drawn this turn",
        ] {
            let (rest, q) = parse_quantity_ref(text).unwrap();
            assert_eq!(rest, "", "{text:?} should fully consume");
            assert_eq!(
                q,
                QuantityRef::CardsDrawnThisTurn {
                    player: PlayerScope::Controller,
                },
                "{text:?}"
            );
        }
    }

    /// CR 121.1 + CR 102.2/102.3: the opponents'-draw form must parse to a
    /// SUM-across-opponents scope, both bare (for-each cost-mod path, Heliod,
    /// the Warped Eclipse) and behind "the number of". The controller forms must
    /// still resolve to `Controller` (regression lock against the opponents arm
    /// shadowing them).
    #[test]
    fn parse_cards_drawn_this_turn_opponents_sum_and_controller_regression() {
        // Bare opponents form — reachable only via the new top-level arm.
        for text in [
            "cards your opponents have drawn this turn",
            "the number of cards your opponents have drawn this turn",
        ] {
            let (rest, q) = parse_quantity_ref(text).unwrap();
            assert_eq!(rest, "", "{text:?} should fully consume");
            assert_eq!(
                q,
                QuantityRef::CardsDrawnThisTurn {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Sum,
                    },
                },
                "{text:?} must be opponents' SUM, not ObjectCount or Controller"
            );
        }

        // Controller forms (bare + the-number-of) still resolve to Controller.
        for text in [
            "cards you've drawn this turn",
            "cards you have drawn this turn",
            "the number of cards you've drawn this turn",
            "the number of cards you have drawn this turn",
        ] {
            let (rest, q) = parse_quantity_ref(text).unwrap();
            assert_eq!(rest, "", "{text:?} should fully consume");
            assert_eq!(
                q,
                QuantityRef::CardsDrawnThisTurn {
                    player: PlayerScope::Controller,
                },
                "{text:?} must remain Controller-scoped"
            );
        }
    }

    /// CR 601.2f: the for-each cost-mod path (Heliod) routes "card your opponents
    /// have drawn this turn" through `parse_for_each_clause`. Previously this fell
    /// to `None`/`ObjectCount{Card}`; it must now yield the opponents' SUM ref.
    #[test]
    fn parse_for_each_clause_opponents_cards_drawn() {
        use crate::parser::oracle_quantity::parse_for_each_clause;

        let qty = parse_for_each_clause("card your opponents have drawn this turn");
        assert_eq!(
            qty,
            Some(QuantityRef::CardsDrawnThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Sum,
                },
            }),
            "for-each over opponents' draws must yield the SUM-scoped ref, not None/ObjectCount"
        );
    }

    /// End-to-end: CDA static lines must lower once the quantity arms parse.
    #[test]
    fn parse_cda_static_lines_opponent_drawn_and_chosen_player() {
        use crate::parser::oracle_static::parse_static_line;

        for line in [
            "~'s power is equal to the number of cards you've drawn this turn.",
            "~'s power is equal to the number of tapped lands the chosen player controls.",
            "~'s power and toughness are each equal to 2 plus the number of Swamps your opponents control.",
        ] {
            let def = parse_static_line(line).unwrap_or_else(|| panic!("{line:?} should parse"));
            assert!(
                def.characteristic_defining,
                "{line:?} should be a CDA"
            );
            assert!(
                !def.modifications.is_empty(),
                "{line:?} should emit dynamic P/T mods"
            );
        }
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
    fn parse_for_each_attached_to_source_gendered_animate_pronoun() {
        // CR 301.5a + CR 303.4: MSH/Marvel templates phrase the attachment count
        // with the source-anaphoric gendered pronoun "him"/"her"
        // (Winter Soldier "for each Equipment attached to him"). These denote the
        // SAME object id as `~`, so the combinator must emit `AttachedToSource`.
        // Fail-before: no "attached to him/her" arm → Err.
        for pronoun in ["him", "her"] {
            let clause = format!("equipment attached to {pronoun}");
            let (rest, q) = parse_for_each_clause_ref(&clause)
                .unwrap_or_else(|e| panic!("expected Ok for {clause:?}, got {e:?}"));
            assert_eq!(rest, "", "remainder for {clause:?}");
            match q {
                QuantityRef::ObjectCount { filter } => match filter {
                    TargetFilter::Typed(TypedFilter {
                        type_filters,
                        controller,
                        properties,
                    }) => {
                        assert_eq!(controller, None, "controller for {clause:?}");
                        assert_eq!(
                            properties,
                            vec![FilterProp::AttachedToSource],
                            "properties for {clause:?}"
                        );
                        assert_eq!(
                            type_filters,
                            vec![TypeFilter::Subtype("Equipment".into())],
                            "type_filters for {clause:?}"
                        );
                    }
                    other => panic!("expected Typed filter for {clause:?}, got {other:?}"),
                },
                other => panic!("expected ObjectCount for {clause:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_for_each_attached_to_them_not_source_bound() {
        // CR 301.5a + CR 303.4: the singular-they "them" is recipient-anaphoric for
        // player-enchanting Auras (Curse of Thirst: "Curses attached to them" = the
        // enchanted player), so it must NOT bind to the source. The gendered arm
        // deliberately omits "them"; this combinator therefore does not produce an
        // AttachedToSource count for it (the clause is left unconsumed). Guards
        // against a future re-add that would silently count the wrong object set.
        let result = parse_for_each_attached_to_source("curse attached to them");
        match result {
            Err(_) => {}
            Ok((rest, q)) => {
                // If some other arm consumes it, it must not be AttachedToSource.
                if let QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(TypedFilter { properties, .. }),
                } = &q
                {
                    assert!(
                        !properties.contains(&FilterProp::AttachedToSource),
                        "\"attached to them\" must not bind to the source, got {q:?} (rest {rest:?})"
                    );
                }
            }
        }
    }

    #[test]
    fn parse_for_each_attached_to_recipient_it_preserved_after_gendered_arm() {
        // Discrimination/regression: the recipient authority ("it") must stay
        // AttachedToRecipient even with the new source-pronoun arm above it.
        let (rest, q) = parse_for_each_clause_ref("aura attached to it").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter { properties, .. }),
            } => {
                assert_eq!(properties, vec![FilterProp::AttachedToRecipient]);
            }
            other => panic!("expected recipient ObjectCount, got {other:?}"),
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
    fn parse_for_each_clause_expr_other_attacking_creature_sharing_type() {
        let expr = crate::parser::oracle_quantity::parse_for_each_clause_expr(
            "other attacking creature that shares a creature type with it",
        )
        .expect("for-each expr");
        assert!(matches!(
            expr,
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { .. }
            }
        ));
    }

    #[test]
    fn parse_for_each_other_attacking_creature_sharing_via_oracle_quantity_fallback() {
        let qty = crate::parser::oracle_quantity::parse_for_each_clause(
            "other attacking creature that shares a creature type with it",
        )
        .expect("oracle_quantity type-phrase fallback should parse Shared Animosity for-each");
        let QuantityRef::ObjectCount { filter } = qty else {
            panic!("expected object count");
        };
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected typed");
        };
        assert!(tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::SharesQuality {
                quality: SharedQuality::CreatureType,
                ..
            }
        )));
    }

    /// CR 119.3 + CR 603.2c: "for each 1 life you gained/lost" — the per-1
    /// multiplier on a `Whenever you gain/lose life` trigger resolves to the
    /// triggering event's amount via `EventContextAmount` (Cradle of Vitality,
    /// Transcendence, Lich's Tomb). Without the dedicated arm the for-each parse
    /// fails and the count silently stays `Fixed{1}`.
    #[test]
    fn parse_for_each_one_life_changed_yields_event_amount() {
        use crate::parser::oracle_quantity::{parse_for_each_clause, parse_for_each_clause_expr};

        for clause in ["1 life you gained", "1 life you lost"] {
            assert_eq!(
                parse_for_each_clause(clause),
                Some(QuantityRef::EventContextAmount),
                "{clause:?} must resolve to the triggering life-change amount",
            );
        }
        // "one life you ..." spelled-out variant.
        assert_eq!(
            parse_for_each_clause("one life you lost"),
            Some(QuantityRef::EventContextAmount),
        );
        // Expr wrapper used by the for-each effect path.
        assert_eq!(
            parse_for_each_clause_expr("1 life you lost"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            }),
        );
    }

    /// No-regression: "life you gained/lost this turn" (no leading "1 ") must
    /// keep its duration-class lower, NOT the per-1 event-amount arm.
    #[test]
    fn parse_for_each_one_life_changed_requires_one_prefix() {
        let (rest, q) = parse_quantity_ref("life you gained this turn").unwrap();
        assert_eq!(
            q,
            QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::Controller,
            }
        );
        assert_eq!(rest, "");
        let (rest, q) = parse_quantity_ref("life you lost this turn").unwrap();
        assert_eq!(
            q,
            QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Controller,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_for_each_other_attacking_goblin_via_type_phrase_fallback() {
        let qty = crate::parser::oracle_quantity::parse_for_each_clause("other attacking Goblin")
            .expect("oracle_quantity fallback should parse other attacking Goblin");
        assert!(matches!(
            qty,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(_)
            }
        ));
    }

    #[test]
    fn parse_for_each_other_attacking_creature_sharing_type_with_it() {
        use crate::types::ability::{
            ControllerRef, FilterProp, SharedQuality, SharedQualityRelation, TargetFilter,
            TypeFilter, TypedFilter,
        };
        let ctx = ParseContext {
            subject: Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            )),
            ..Default::default()
        };
        let qty = crate::parser::oracle_quantity::parse_for_each_clause_with_context(
            "other attacking creature that shares a creature type with it",
            &ctx,
        )
        .expect("for-each clause with trigger subject");
        let QuantityRef::ObjectCount { filter } = qty else {
            panic!("expected object count");
        };
        let TargetFilter::Typed(TypedFilter {
            type_filters,
            properties,
            ..
        }) = filter
        else {
            panic!("expected typed filter");
        };
        assert_eq!(type_filters, vec![TypeFilter::Creature]);
        assert!(properties.contains(&FilterProp::Another));
        assert!(properties.contains(&FilterProp::Attacking { defender: None }));
        assert!(properties.iter().any(|p| matches!(
            p,
            FilterProp::SharesQuality {
                quality: SharedQuality::CreatureType,
                reference: Some(reference),
                relation: SharedQualityRelation::Shares,
            } if matches!(reference.as_ref(), TargetFilter::TriggeringSource)
        )));
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

    /// CR 201.2 + CR 109.4: "for each [other] <type> named <CardName> you
    /// control" must keep the `named X` qualifier AND the controller scope —
    /// not drop the whole DynamicQty. Seven Dwarves ("gets +1/+1 for each other
    /// creature named Seven Dwarves you control") regressed to a swallowed
    /// clause once the named-X terminator correctly stopped the card name at
    /// " you control": the bare-type `parse_for_each_controlled_type` arm could
    /// not reach the controller suffix past the qualifier. Tests the class:
    /// the `named X`/`other`/controller triple survives for any card name.
    #[test]
    fn parse_for_each_other_named_creature_you_control_keeps_dynamic_quantity() {
        let (rest, q) =
            parse_for_each_clause_ref("other creature named seven dwarves you control").unwrap();
        assert_eq!(rest, "");
        let QuantityRef::ObjectCount {
            filter:
                TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller,
                    properties,
                }),
        } = q
        else {
            panic!("expected ObjectCount(Typed), got {q:?}");
        };
        assert_eq!(type_filters, vec![TypeFilter::Creature]);
        assert_eq!(controller, Some(ControllerRef::You));
        assert!(properties.contains(&FilterProp::Another));
        assert!(properties.iter().any(|p| matches!(
            p,
            FilterProp::Named { name } if name == "seven dwarves"
        )));
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

    /// MSH Wave 2 (Beast, Erudite Aerialist): "you've put one or more +1/+1
    /// counters on ~ this turn" (self-ref normalized from "on Beast") must parse
    /// the counter-added target as `TargetFilter::SelfRef`, so the runtime quantity
    /// resolver counts only counters placed on the source object (CR 201.5). Without
    /// the `~` arm the target is unmatched and the whole condition fails to parse.
    #[test]
    fn parse_counter_added_condition_accepts_self_ref_target() {
        let (rest, q) = parse_counter_added_this_turn_condition(
            "you've put one or more +1/+1 counters on ~ this turn",
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
                target: TargetFilter::SelfRef,
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
    fn test_parse_for_each_typeline_components_it_has() {
        let (rest, q) =
            parse_for_each("for each supertype, card type, and subtype it has").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectTypelineComponentCount {
                scope: crate::types::ability::ObjectScope::Recipient,
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

    /// CR 105.1 + CR 601.2f + CR 115.1: generalized "color of <object>" for-each.
    #[test]
    fn test_parse_for_each_color_of_object() {
        let (rest, q) = parse_for_each_clause_ref("color of the creature it targets").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectColorCount {
                scope: crate::types::ability::ObjectScope::Target
            }
        );
        assert_eq!(rest, "");

        let (rest, q) = parse_for_each_clause_ref("color of target creature").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectColorCount {
                scope: crate::types::ability::ObjectScope::Target
            }
        );
        assert_eq!(rest, "");

        let (rest, q) = parse_for_each_clause_ref("colors of the enchanted creature").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectColorCount {
                scope: crate::types::ability::ObjectScope::Recipient
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
                color: Some(ManaColor::White),
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
    fn parse_for_each_typed_counter_on_source() {
        let (rest, q) = parse_for_each_clause_ref("velocity counter on this enchantment").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            q,
            QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: Some(_),
            }
        ));
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
                filter: None,
            }
        );
        assert_eq!(rest, "");
    }

    /// CR 202.3 + CR 608.2k: prepositional cost-paid mana-value form
    /// (Morbid Curiosity) resolves the same `CostPaidObject` referent as the
    /// possessive "the sacrificed permanent's mana value".
    #[test]
    fn parse_quantity_ref_cost_paid_object_prepositional_mana_value() {
        for phrase in [
            "the mana value of the sacrificed permanent",
            "mana value of the sacrificed permanent",
            "the mana value of the exiled creature",
            "the converted mana cost of the sacrificed artifact",
            "the mana value of the returned creature",
        ] {
            let (rest, q) = parse_quantity_ref(phrase).unwrap();
            assert_eq!(
                q,
                QuantityRef::ObjectManaValue {
                    scope: crate::types::ability::ObjectScope::CostPaidObject,
                },
                "phrase: {phrase}"
            );
            assert_eq!(rest, "", "phrase: {phrase}");
        }
    }

    /// CR 208.1 + CR 608.2k + CR 400.7j: "the power/toughness of the
    /// chosen/revealed (beheld) object" resolves the same `CostPaidObject`
    /// referent as the sacrifice/exile possessives — the additional-cost-chosen
    /// object's power read at resolution (Close Encounter, Monstrous Emergence).
    #[test]
    fn parse_quantity_ref_cost_paid_object_chosen_revealed_power() {
        for phrase in [
            "the power of the chosen creature or card",
            "power of the chosen creature or card",
            "the power of the creature you chose or the card you revealed",
        ] {
            let (rest, q) = parse_quantity_ref(phrase).unwrap();
            assert_eq!(
                q,
                QuantityRef::Power {
                    scope: crate::types::ability::ObjectScope::CostPaidObject,
                },
                "phrase: {phrase}"
            );
            assert_eq!(rest, "", "phrase: {phrase}");
        }
        // Toughness axis composes through the same combinator.
        let (rest, q) = parse_quantity_ref("the toughness of the chosen creature or card").unwrap();
        assert_eq!(
            q,
            QuantityRef::Toughness {
                scope: crate::types::ability::ObjectScope::CostPaidObject,
            }
        );
        assert_eq!(rest, "");
    }

    /// CR 506.2 + CR 402: "cards in defending player's hand" → defending-player
    /// hand size (Mr. Foxglove), reachable both bare and after "the number of".
    #[test]
    fn parse_quantity_ref_defending_player_hand() {
        for phrase in [
            "cards in defending player's hand",
            "the number of cards in defending player's hand",
        ] {
            let (rest, q) = parse_quantity_ref(phrase).unwrap();
            assert_eq!(
                q,
                QuantityRef::HandSize {
                    player: PlayerScope::DefendingPlayer,
                },
                "phrase: {phrase}"
            );
            assert_eq!(rest, "", "phrase: {phrase}");
        }
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

    /// CR 613.1: the en-Kor… no — the CDA "chosen player" cycle. "the chosen
    /// player" is the player persisted on the source via `ChosenAttribute::Player`
    /// (Skyshroud War Beast, Lost Order of Jarkeld, Entropic Specter, Sewer
    /// Nemesis). Controls-counts route through `ControllerRef::SourceChosenPlayer`;
    /// zone-counts through `CountScope::SourceChosenPlayer`.
    #[test]
    fn parse_quantity_ref_chosen_player_cda_forms() {
        let (rest, q) =
            parse_quantity_ref("the number of creatures the chosen player controls").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(tf),
            } => {
                assert_eq!(tf.controller, Some(ControllerRef::SourceChosenPlayer));
                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
            }
            other => panic!("expected ObjectCount, got {other:?}"),
        }

        for (text, zone) in [
            (
                "the number of cards in the chosen player's hand",
                ZoneRef::Hand,
            ),
            (
                "the number of cards in the chosen player's graveyard",
                ZoneRef::Graveyard,
            ),
            (
                "the number of cards in the chosen player's library",
                ZoneRef::Library,
            ),
            (
                "the number of cards in the chosen player's exile",
                ZoneRef::Exile,
            ),
        ] {
            let (rest, q) = parse_quantity_ref(text).unwrap();
            assert_eq!(rest, "");
            assert_eq!(
                q,
                QuantityRef::ZoneCardCount {
                    zone,
                    card_types: Vec::new(),
                    scope: CountScope::SourceChosenPlayer,
                    filter: None,
                }
            );
        }
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
    fn parse_player_with_most_cards_in_hand() {
        // CR 402.1: the cross-player hand-size MAX extremum.
        let (rest, q) =
            parse_player_with_extremum_cards_in_hand("the player with the most cards in hand")
                .unwrap();
        assert_eq!(
            q,
            QuantityRef::HandSize {
                player: PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Max,
                    exclude: None,
                },
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_player_with_fewest_cards_in_hand() {
        // CR 402.1: the MIN direction — proves the aggregate parameterization,
        // not just Tales' Max direction.
        let (rest, q) =
            parse_player_with_extremum_cards_in_hand("the player with the fewest cards in hand")
                .unwrap();
        assert_eq!(
            q,
            QuantityRef::HandSize {
                player: PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Min,
                    exclude: None,
                },
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_opponent_with_most_cards_in_hand() {
        // CR 402.1 + CR 102.2/102.3: opponent-scoped MAX extremum.
        let (rest, q) =
            parse_player_with_extremum_cards_in_hand("the opponent with the most cards in hand")
                .unwrap();
        assert_eq!(
            q,
            QuantityRef::HandSize {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Max,
                },
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_opponent_with_fewest_cards_in_hand() {
        // CR 402.1 + CR 102.2/102.3: opponent-scoped MIN extremum.
        let (rest, q) =
            parse_player_with_extremum_cards_in_hand("the opponent with the fewest cards in hand")
                .unwrap();
        assert_eq!(
            q,
            QuantityRef::HandSize {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Min,
                },
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_verbose_all_players_max_extremum_hand_size() {
        let (rest, q) = parse_quantity_ref(
            "the number of cards in the hand of the player with the most cards in hand",
        )
        .unwrap();
        assert_eq!(
            q,
            QuantityRef::HandSize {
                player: PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Max,
                    exclude: None,
                },
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_verbose_all_players_min_extremum_hand_size() {
        let (rest, q) = parse_quantity_ref(
            "the number of cards in the hand of the player with the fewest cards in hand",
        )
        .unwrap();
        assert_eq!(
            q,
            QuantityRef::HandSize {
                player: PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Min,
                    exclude: None,
                },
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_verbose_opponent_max_extremum_hand_size() {
        let (rest, q) = parse_quantity_ref(
            "the number of cards in the hand of the opponent with the most cards in hand",
        )
        .unwrap();
        assert_eq!(
            q,
            QuantityRef::HandSize {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Max,
                },
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_verbose_opponent_min_extremum_hand_size() {
        let (rest, q) = parse_quantity_ref(
            "the number of cards in the hand of the opponent with the fewest cards in hand",
        )
        .unwrap();
        assert_eq!(
            q,
            QuantityRef::HandSize {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Min,
                },
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn player_with_extremum_cards_in_hand_reachable_via_quantity_ref() {
        // Confirms the new combinator is registered in the shared
        // `parse_quantity_ref` `alt`, so any quantity context gains the phrase.
        let (rest, q) = parse_quantity_ref("the player with the most cards in hand").unwrap();
        assert_eq!(
            q,
            QuantityRef::HandSize {
                player: PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Max,
                    exclude: None,
                },
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
            // CR 208.3 + CR 608.2k: gendered/plural possessive pronouns are
            // interchangeable with "its" for the source's own power (Iron Fist,
            // Living Weapon — "deals damage equal to his power").
            "his power",
            "her power",
            "their power",
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

    /// CR 208.3 + CR 608.2k: gendered/plural possessive pronouns reference the
    /// source's own toughness, mirroring the power phrasings.
    #[test]
    fn test_parse_quantity_ref_self_toughness_gendered_pronouns() {
        for phrase in ["his toughness", "her toughness", "their toughness"] {
            let (rest, q) = parse_quantity_ref(phrase).unwrap();
            assert_eq!(
                q,
                QuantityRef::Toughness {
                    scope: crate::types::ability::ObjectScope::Source
                },
                "phrase: {phrase}"
            );
            assert_eq!(rest, "", "phrase: {phrase}");
        }
    }

    /// CR 122.1 + CR 608.2k: "the number of +1/+1 counters on him/her/them"
    /// counts counters on the ability's own source — the gendered/plural
    /// objective pronouns are interchangeable with "it" (Red Hulk's Enrage
    /// reflex: "damage equal to the number of +1/+1 counters on him").
    #[test]
    fn test_parse_quantity_ref_counters_on_source_gendered_pronouns() {
        for phrase in [
            "the number of +1/+1 counters on him",
            "the number of +1/+1 counters on her",
            "the number of +1/+1 counters on them",
        ] {
            let (rest, q) = parse_quantity_ref(phrase).unwrap();
            assert_eq!(
                q,
                QuantityRef::CountersOn {
                    scope: crate::types::ability::ObjectScope::Source,
                    counter_type: Some(crate::types::counter::CounterType::Plus1Plus1),
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
                filter: None,
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
                filter: None,
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
                filter: None,
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
                filter: None,
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
                                filter: None,
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
                filter: None,
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
    fn test_parse_distinct_card_types_among_cards_discarded_this_way() {
        // Occult Epiphany #3307: singular "card type" + Discarded cause.
        let (rest, q) =
            parse_distinct_card_types_among_tracked_set("card type among cards discarded this way")
                .unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::DistinctCardTypes {
                source: CardTypeSetSource::TrackedSet {
                    caused_by: Some(ThisWayCause::Discarded),
                },
            }
        );
    }

    #[test]
    fn test_parse_distinct_card_types_among_cards_exiled_this_way() {
        // Plural "card types" + Exiled cause.
        let (rest, q) =
            parse_distinct_card_types_among_tracked_set("card types among cards exiled this way")
                .unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::DistinctCardTypes {
                source: CardTypeSetSource::TrackedSet {
                    caused_by: Some(ThisWayCause::Exiled),
                },
            }
        );
    }

    #[test]
    fn test_distinct_card_types_among_tracked_set_via_parse_quantity_ref() {
        // The combinator must win over `parse_distinct_card_types_among_objects`
        // when reached through the top-level `parse_quantity_ref` alt chain.
        let (rest, q) = parse_quantity_ref("card types among cards discarded this way").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::DistinctCardTypes {
                source: CardTypeSetSource::TrackedSet {
                    caused_by: Some(ThisWayCause::Discarded),
                },
            }
        );
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

    /// CR 115.1 + CR 119.3 + CR 608.2c: the third-person "they" / "that player"
    /// life-lost anaphor must emit `PlayerScope::Target` at the leaf — the player
    /// the surrounding LoseLife affects — so a targeted clause (Blitzwing) reads
    /// the target's own loss and a per-opponent loop (Wound Reflection) can be
    /// rebound to `ScopedPlayer` by `rewrite_player_scope_refs`. Guards against
    /// the prior `Controller` mapping that drained the source's controller.
    #[test]
    fn parse_quantity_ref_third_person_life_lost_is_target_scoped() {
        // The article-only forms are the exact Wound Reflection / Archfiend /
        // Warlock / Blitzwing phrasings. The "amount of life they lost" gloss
        // (Astarion Feed) is consumed by `parse_life_lost_ref`'s leading
        // `opt("the amount of ")` strip and routed through the imperative-level
        // `parse_target_relative_life_change_this_turn` recognizer instead, which
        // also yields `Target` — so it is asserted at that layer, not here.
        for phrase in [
            "the life they lost this turn",
            "the life that player lost this turn",
        ] {
            let (rest, q) = parse_quantity_ref(phrase).unwrap();
            assert_eq!(
                q,
                QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Target
                },
                "{phrase:?} must be Target-scoped, got {q:?}"
            );
            assert_eq!(rest, "", "{phrase:?} left remainder {rest:?}");
        }
    }

    /// Over-broadening guard: the first-person "you"/"you've" arms must stay
    /// `Controller`-scoped (CR 109.5 — "you" is the controller, never a target).
    #[test]
    fn parse_quantity_ref_first_person_life_lost_stays_controller() {
        for phrase in [
            "the life you lost this turn",
            "the life you've lost this turn",
            "total life you lost this turn",
        ] {
            let (rest, q) = parse_quantity_ref(phrase).unwrap();
            assert_eq!(
                q,
                QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Controller
                },
                "{phrase:?} must stay Controller-scoped, got {q:?}"
            );
            assert_eq!(rest, "", "{phrase:?} left remainder {rest:?}");
        }
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
    fn test_parse_for_each_card_drawn_this_way() {
        let (rest, q) = parse_for_each_clause_ref("card drawn this way").unwrap();
        assert_eq!(q, QuantityRef::EventContextAmount);
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

    /// Helper: pull `qualities` out of an `ObjectCountDistinct`, panicking (so
    /// the test fails loudly) on `Fixed`/`Variable`/any other shape — the exact
    /// misparse this fix corrects.
    fn distinct_qualities(q: &QuantityRef) -> Vec<SharedQuality> {
        match q {
            QuantityRef::ObjectCountDistinct { qualities, .. } => qualities.clone(),
            other => panic!("expected ObjectCountDistinct, got {other:?}"),
        }
    }

    #[test]
    fn for_each_different_power_among_creatures_you_control() {
        // Golden Ratio: "Draw a card for each different power among creatures
        // you control." Must be a distinct-power count, not Fixed(1).
        let (rest, q) =
            parse_for_each_clause_ref("different power among creatures you control").unwrap();
        assert_eq!(rest, "");
        let (type_filters, controller, _properties, qualities) = assert_distinct_named_typed(q);
        assert!(type_filters.contains(&TypeFilter::Creature));
        assert_eq!(controller, Some(ControllerRef::You));
        assert_eq!(qualities, vec![SharedQuality::Power]);
    }

    #[test]
    fn for_each_different_powers_plural_among_creatures() {
        // Plural "powers" must parse identically (Celebrate the Harvest uses it).
        let (rest, q) =
            parse_for_each_clause_ref("different powers among creatures you control").unwrap();
        assert_eq!(rest, "");
        assert_eq!(distinct_qualities(&q), vec![SharedQuality::Power]);
    }

    #[test]
    fn for_each_different_mana_value_among_nonland_permanents() {
        // Lunar Insight: "for each different mana value among nonland permanents
        // you control."
        let (rest, q) =
            parse_for_each_clause_ref("different mana value among nonland permanents you control")
                .unwrap();
        assert_eq!(rest, "");
        assert_eq!(distinct_qualities(&q), vec![SharedQuality::ManaValue]);
    }

    #[test]
    fn for_each_different_mana_value_among_graveyard_nonland_cards() {
        // Sudden Insight: "for each different mana value among nonland cards in
        // your graveyard." The graveyard zone must survive into the filter so
        // the runtime counts graveyard cards (not the default battlefield).
        let (rest, q) =
            parse_for_each_clause_ref("different mana value among nonland cards in your graveyard")
                .unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::ObjectCountDistinct { filter, qualities } => {
                assert_eq!(qualities, vec![SharedQuality::ManaValue]);
                assert_eq!(
                    filter.extract_in_zone(),
                    Some(crate::types::zones::Zone::Graveyard),
                    "graveyard zone must survive into the filter: {filter:?}"
                );
            }
            other => panic!("expected ObjectCountDistinct, got {other:?}"),
        }
    }

    #[test]
    fn number_of_different_powers_among_creatures_celebrate_the_harvest() {
        // Celebrate the Harvest: "...where X is the number of different powers
        // among creatures you control." Routes through the "the number of" path.
        let (rest, q) =
            parse_quantity_ref("the number of different powers among creatures you control")
                .unwrap();
        assert_eq!(rest, "");
        let (type_filters, controller, _properties, qualities) = assert_distinct_named_typed(q);
        assert!(type_filters.contains(&TypeFilter::Creature));
        assert_eq!(controller, Some(ControllerRef::You));
        assert_eq!(qualities, vec![SharedQuality::Power]);
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

    /// CR 400.7 + CR 603.10a: "creature that left the battlefield under your
    /// control this turn" must parse to a destination-agnostic zone-change
    /// count (to: None) scoped to creatures you control — distinct from the
    /// graveyard-only "died" arm. Kutzil's Flanker mode 1.
    #[test]
    fn parse_for_each_creature_left_battlefield_under_your_control() {
        for phrase in [
            "creature that left the battlefield under your control this turn",
            "creature that left the battlefield under your control",
        ] {
            let (rest, q) = parse_for_each_clause_ref(phrase)
                .unwrap_or_else(|_| panic!("expected {phrase:?} to parse"));
            assert_eq!(rest, "", "{phrase:?} left unconsumed");
            let QuantityRef::ZoneChangeCountThisTurn { from, to, filter } = q else {
                panic!("expected ZoneChangeCountThisTurn for {phrase:?}, got {q:?}");
            };
            assert_eq!(from, Some(Zone::Battlefield));
            // "left the battlefield" is destination-agnostic (NOT graveyard-only).
            assert_eq!(to, None, "destination must be unconstrained");
            let TargetFilter::Typed(tf) = filter else {
                panic!("expected Typed creature filter, got {filter:?}");
            };
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
        // The graveyard-only "died" phrasing must NOT be captured by this arm.
        let (_, died) = parse_for_each_clause_ref("creature that died this turn").unwrap();
        assert!(matches!(
            died,
            QuantityRef::ZoneChangeCountThisTurn {
                to: Some(Zone::Graveyard),
                ..
            }
        ));
    }

    /// CR 700.4: plural "creatures that died this turn" must parse for both
    /// for-each and "the number of" quantity surfaces (Spymaster's Vault).
    #[test]
    fn parse_creatures_died_this_turn_plural_and_number_of() {
        for phrase in [
            "creatures that died this turn",
            "creature that died this turn",
        ] {
            let (_, for_each) = parse_for_each_clause_ref(phrase)
                .unwrap_or_else(|_| panic!("for-each {phrase:?} should parse"));
            let (_, number_of) = parse_quantity_ref(&format!("the number of {phrase}"))
                .unwrap_or_else(|_| panic!("number-of {phrase:?} should parse"));
            for q in [for_each, number_of] {
                assert!(
                    matches!(
                        q,
                        QuantityRef::ZoneChangeCountThisTurn {
                            from: Some(Zone::Battlefield),
                            to: Some(Zone::Graveyard),
                            ..
                        }
                    ),
                    "{phrase:?} got {q:?}"
                );
                // CR 700.4: unqualified "creatures that died this turn" counts
                // every player's deaths — controller must stay unscoped.
                let QuantityRef::ZoneChangeCountThisTurn {
                    filter: TargetFilter::Typed(tf),
                    ..
                } = q
                else {
                    unreachable!()
                };
                assert_eq!(tf.controller, None, "{phrase:?} must not scope controller");
            }
        }
    }

    /// CR 109.5 + #1129: "creatures that died under your control" / "put into
    /// your graveyard" forms must scope the zone-change count to the source's
    /// controller (`ControllerRef::You`) for BOTH the for-each and "the number
    /// of" surfaces, while unqualified forms leave the controller unset. Mirrors
    /// `parse_for_each_creature_left_battlefield_under_your_control`.
    #[test]
    fn parse_creatures_died_under_your_control_scopes_controller() {
        let qualified = [
            "creatures that died under your control this turn",
            "creature that died under your control",
            "creatures put into your graveyard from the battlefield this turn",
        ];
        for phrase in qualified {
            let (_, for_each) = parse_for_each_clause_ref(phrase)
                .unwrap_or_else(|_| panic!("for-each {phrase:?} should parse"));
            let (_, number_of) = parse_quantity_ref(&format!("the number of {phrase}"))
                .unwrap_or_else(|_| panic!("number-of {phrase:?} should parse"));
            for q in [for_each, number_of] {
                let QuantityRef::ZoneChangeCountThisTurn {
                    from: Some(Zone::Battlefield),
                    to: Some(Zone::Graveyard),
                    filter: TargetFilter::Typed(tf),
                } = q
                else {
                    panic!("expected graveyard ZoneChangeCountThisTurn for {phrase:?}, got {q:?}");
                };
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(
                    tf.controller,
                    Some(ControllerRef::You),
                    "{phrase:?} must scope to the controller"
                );
            }
        }

        let unqualified = [
            "creatures that died this turn",
            "creatures put into a graveyard from the battlefield",
        ];
        for phrase in unqualified {
            let (_, for_each) = parse_for_each_clause_ref(phrase)
                .unwrap_or_else(|_| panic!("for-each {phrase:?} should parse"));
            let (_, number_of) = parse_quantity_ref(&format!("the number of {phrase}"))
                .unwrap_or_else(|_| panic!("number-of {phrase:?} should parse"));
            for q in [for_each, number_of] {
                let QuantityRef::ZoneChangeCountThisTurn {
                    filter: TargetFilter::Typed(tf),
                    ..
                } = q
                else {
                    panic!("expected ZoneChangeCountThisTurn for {phrase:?}, got {q:?}");
                };
                assert_eq!(tf.controller, None, "{phrase:?} must not scope controller");
            }
        }
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
                && properties == vec![FilterProp::Attacking { defender: None }, FilterProp::Another]
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
                && properties == vec![FilterProp::Attacking { defender: None }]
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

    /// CR 700.5: the Chroma wording for devotion — "the number of <color> mana
    /// symbols in the mana costs of permanents you control" (Outrage Shaman,
    /// Primalcrux) — maps to the same `Devotion` quantity as "your devotion to
    /// <color>".
    #[test]
    fn test_parse_chroma_devotion() {
        let (rest, q) = parse_quantity_ref(
            "the number of green mana symbols in the mana costs of permanents you control",
        )
        .unwrap();
        assert_eq!(
            q,
            QuantityRef::Devotion {
                colors: DevotionColors::Fixed(vec![ManaColor::Green])
            }
        );
        assert_eq!(rest, "");

        let (_, red) = parse_quantity_ref(
            "the number of red mana symbols in the mana costs of permanents you control",
        )
        .unwrap();
        assert_eq!(
            red,
            QuantityRef::Devotion {
                colors: DevotionColors::Fixed(vec![ManaColor::Red])
            }
        );
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

    /// CR 202.1 + CR 404.2: graveyard-scope Chroma — "the number of <color> mana symbols in
    /// the mana costs of cards in your graveyard" (Umbra Stalker).
    #[test]
    fn test_parse_graveyard_chroma() {
        let (rest, q) = parse_quantity_ref(
            "the number of black mana symbols in the mana costs of cards in your graveyard",
        )
        .unwrap();
        assert_eq!(
            q,
            QuantityRef::Aggregate {
                function: AggregateFunction::Sum,
                property: ObjectProperty::ManaSymbolCount(ManaColor::Black),
                filter: TargetFilter::Typed(TypedFilter::card().properties(vec![
                    FilterProp::Owned {
                        controller: ControllerRef::You,
                    },
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ])),
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

    /// CR 202.3 + CR 115.1: "mana value of target <filter>" lowers to the
    /// object-axis `TargetObjectManaValue` (Fateful Handoff, Knollspine Dragon),
    /// carrying the parsed target filter. The bare possessive "target creature's
    /// mana value" stays `ObjectManaValue { Target }` (test below).
    #[test]
    fn test_parse_target_object_mana_value_of_form() {
        let (rest, q) =
            parse_quantity_ref("mana value of target artifact or creature you control").unwrap();
        match q {
            QuantityRef::TargetObjectManaValue { filter } => {
                assert_ne!(
                    *filter,
                    TargetFilter::Any,
                    "the carried slot filter must be the parsed 'artifact or creature you control'",
                );
            }
            other => panic!("expected TargetObjectManaValue, got {other:?}"),
        }
        assert_eq!(rest, "");
    }

    /// The bare possessive must NOT route to the of-form variant.
    #[test]
    fn test_parse_target_creature_possessive_mana_value_unchanged() {
        let (rest, q) = parse_quantity_ref("target creature's mana value").unwrap();
        assert_eq!(
            q,
            QuantityRef::ObjectManaValue {
                scope: crate::types::ability::ObjectScope::Target,
            }
        );
        assert_eq!(rest, "");
    }

    /// CR 701.9 + CR 115.1: "cards target opponent discarded this turn" lowers to
    /// the player-axis Target scope (Dream Salvage).
    #[test]
    fn test_parse_cards_target_opponent_discarded_this_turn() {
        let (rest, q) =
            parse_quantity_ref("the number of cards target opponent discarded this turn").unwrap();
        assert_eq!(
            q,
            QuantityRef::CardsDiscardedThisTurn {
                player: PlayerScope::Target,
            }
        );
        assert_eq!(rest, "");
    }

    /// Serde round-trip for the new object-axis variant.
    #[test]
    fn test_target_object_mana_value_serde_round_trip() {
        let qty = QuantityRef::TargetObjectManaValue {
            filter: Box::new(TargetFilter::Typed(TypedFilter::creature())),
        };
        let json = serde_json::to_string(&qty).expect("serialize");
        let back: QuantityRef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(qty, back);
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

    /// CR 305.6 + CR 601.2f: domain must be reachable through the `for each`
    /// clause path (not just `parse_quantity_ref`), so domain-scaled cost
    /// reducers — "costs {1} less to activate for each basic land type among
    /// lands you control" (Jodah's Codex, Wandering Treefolk, Scion of Draco) —
    /// resolve their reduction quantity instead of dropping to `Unimplemented`.
    #[test]
    fn parse_for_each_clause_ref_handles_domain() {
        let (rest, q) =
            parse_for_each_clause_ref_complete("basic land type among lands you control").unwrap();
        assert_eq!(
            q,
            QuantityRef::BasicLandTypeCount {
                controller: ControllerRef::You,
            }
        );
        assert_eq!(rest, "");

        // Inside a `for each` clause, "they control" binds to the iterating/scoped
        // player (the default `they_controller`), NOT a target player — so
        // per-player/per-opponent domain reducers count the right player's lands.
        let (_, q_they) =
            parse_for_each_clause_ref_complete("basic land type among lands they control").unwrap();
        assert_eq!(
            q_they,
            QuantityRef::BasicLandTypeCount {
                controller: ControllerRef::ScopedPlayer,
            }
        );
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
                        filter: None,
                    }
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
                filter: None,
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

    #[test]
    fn test_parse_greatest_commander_mana_value_ref() {
        // Test the greatest pattern (CR 202.3 aggregate-max)
        let phrase = "the greatest mana value of a commander you own on the battlefield or in the command zone";
        let (rest, q) = parse_quantity_ref(phrase).unwrap();
        assert_eq!(rest, "", "phrase should be fully consumed");

        // Verify it produces Aggregate with Max function
        let QuantityRef::Aggregate {
            function,
            property,
            filter,
        } = q
        else {
            panic!("Expected Aggregate, got {q:?}");
        };

        assert_eq!(function, AggregateFunction::Max);
        assert_eq!(property, ObjectProperty::ManaValue);

        // Verify the filter uses InAnyZone for multi-zone disjunction
        let TargetFilter::Typed(tf) = filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };

        assert!(tf.properties.contains(&FilterProp::IsCommander));
    }

    #[test]
    fn test_parse_commander_mana_value_ref() {
        // Test the non-greatest pattern (Stinging Study)
        let phrase =
            "the mana value of a commander you own on the battlefield or in the command zone";
        let (rest, q) = parse_quantity_ref(phrase).unwrap();
        assert_eq!(rest, "", "phrase should be fully consumed");

        // Verify it produces CommanderManaValue
        let QuantityRef::CommanderManaValue { owner } = q else {
            panic!("Expected CommanderManaValue, got {q:?}");
        };

        assert_eq!(owner, ControllerRef::You);
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

    /// Test the object-property aggregate parser for "where X is the total
    /// mana value" patterns.
    #[test]
    fn parse_object_property_aggregate_total_mana_value_basic() {
        let (rest, q) =
            parse_object_property_aggregate_ref("the total mana value of cards in your graveyard")
                .unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::Aggregate {
                function: AggregateFunction::Sum,
                property: ObjectProperty::ManaValue,
                filter,
            } => {
                assert!(matches!(filter, TargetFilter::Typed(_)));
            }
            _ => panic!("expected Aggregate with Sum and ManaValue"),
        }
    }

    /// Test parse_number_of_counters_on_object for counter count patterns.
    #[test]
    fn parse_number_of_counters_on_object_it() {
        let (rest, q) = parse_number_of_counters_on_object("charge counters on it").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::CountersOn {
                scope,
                counter_type,
            } => {
                assert_eq!(scope, ObjectScope::Source);
                assert!(counter_type.is_some());
            }
            _ => panic!("expected CountersOn"),
        }
    }

    /// Test parse_number_of_counters_on_object with "that creature".
    #[test]
    fn parse_number_of_counters_on_object_that_creature() {
        let (rest, q) =
            parse_number_of_counters_on_object("+1/+1 counters on that creature").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::CountersOn {
                scope,
                counter_type,
            } => {
                assert_eq!(scope, ObjectScope::Target);
                assert!(counter_type.is_some());
            }
            _ => panic!("expected CountersOn"),
        }
    }

    /// Test parse_equal_to_sum for two-way sum expressions.
    #[test]
    fn parse_equal_to_sum_two_way() {
        let (rest, expr) = parse_equal_to_sum(
            "the number of creatures you control and the number of artifacts you control",
        )
        .unwrap();
        assert_eq!(rest, "");
        match expr {
            QuantityExpr::Sum { exprs } => {
                assert_eq!(exprs.len(), 2);
            }
            _ => panic!("expected Sum"),
        }
    }

    /// Test parse_equal_to_sum for three-way sum expressions.
    #[test]
    fn parse_equal_to_sum_three_way() {
        let (rest, expr) = parse_equal_to_sum(
            "the number of creatures you control and the number of artifacts you control and the number of enchantments you control",
        )
        .unwrap();
        assert_eq!(rest, "");
        match expr {
            QuantityExpr::Sum { exprs } => {
                assert_eq!(exprs.len(), 3);
            }
            _ => panic!("expected Sum"),
        }
    }

    /// A single quantity must stay on the normal parse_quantity path.
    #[test]
    fn parse_equal_to_sum_rejects_single_quantity() {
        assert!(parse_equal_to_sum("the number of creatures you control").is_err());
    }

    /// Test parse_for_each_differently_named for distinct-by-name iteration.
    #[test]
    fn parse_for_each_differently_named_basic() {
        let (rest, q) = parse_for_each_differently_named("differently named basic land").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::ObjectCountDistinct { filter, qualities } => {
                assert!(matches!(filter, TargetFilter::Typed(_)));
                assert_eq!(qualities, vec![SharedQuality::Name]);
            }
            _ => panic!("expected ObjectCountDistinct"),
        }
    }

    /// Test parse_for_each_differently_named with a simple type phrase.
    #[test]
    fn parse_for_each_differently_named_creature() {
        let (rest, q) = parse_for_each_differently_named("differently named creature").unwrap();
        assert_eq!(rest, "");
        match q {
            QuantityRef::ObjectCountDistinct { filter, qualities } => {
                assert!(matches!(filter, TargetFilter::Typed(_)));
                assert_eq!(qualities, vec![SharedQuality::Name]);
            }
            _ => panic!("expected ObjectCountDistinct"),
        }
    }

    /// CR 201.2: "named <card name>" ends before the controller suffix in a
    /// controlled object-count quantity. Food Fight.
    #[test]
    fn parse_quantity_ref_controlled_named_type_keeps_controller_out_of_name() {
        let (rest, q) =
            parse_quantity_ref("the number of permanents named food fight you control").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Permanent],
                    controller: Some(ControllerRef::You),
                    properties: vec![FilterProp::Named {
                        name: "food fight".to_string(),
                    }],
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

    /// CR 120.9 + CR 115.1: "(the) damage dealt to target opponent this turn"
    /// parses to a target-player-scoped, all-damage Sum reference so the
    /// count-derived trigger target slot resolves against `ability.targets`.
    #[test]
    fn test_parse_damage_dealt_target_opponent_this_turn() {
        let (rest, q) =
            parse_damage_dealt_this_turn_ref("damage dealt to target opponent this turn").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::Any),
                target: Box::new(TargetFilter::And {
                    filters: vec![
                        TargetFilter::Player,
                        TargetFilter::Typed(
                            TypedFilter::default().controller(ControllerRef::TargetPlayer),
                        ),
                    ],
                }),
                aggregate: AggregateFunction::Sum,
                group_by: None,
                damage_kind: DamageKindFilter::Any,

                excess_only: false,
            }
        );

        // The optional "the " prefix is absorbed by the shared combinator.
        let (rest, q_the) =
            parse_damage_dealt_this_turn_ref("the damage dealt to target opponent this turn")
                .unwrap();
        assert_eq!(rest, "");
        assert_eq!(q_the, q);
    }

    /// Regression: Chandra's Incinerator phrasing still parses to the
    /// Opponent-scoped, noncombat-only Sum reference.
    #[test]
    fn test_parse_damage_dealt_chandra_noncombat_unchanged() {
        let (rest, q) = parse_damage_dealt_this_turn_ref(
            "the total amount of noncombat damage dealt to your opponents this turn",
        )
        .unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            q,
            QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::Any),
                target: Box::new(TargetFilter::And {
                    filters: vec![
                        TargetFilter::Player,
                        TargetFilter::Typed(
                            TypedFilter::default().controller(ControllerRef::Opponent),
                        ),
                    ],
                }),
                aggregate: AggregateFunction::Sum,
                group_by: None,
                damage_kind: DamageKindFilter::NoncombatOnly,

                excess_only: false,
            }
        );
    }

    #[test]
    fn parse_nontoken_creature_died_this_turn_for_each() {
        let (_, q) =
            parse_for_each_creature_died_this_turn("nontoken creature that died this turn")
                .unwrap();
        let QuantityRef::ZoneChangeCountThisTurn { filter, .. } = q else {
            panic!("expected ZoneChangeCountThisTurn, got {q:?}");
        };
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected typed filter");
        };
        assert!(tf.properties.contains(&FilterProp::NonToken));
    }
}
