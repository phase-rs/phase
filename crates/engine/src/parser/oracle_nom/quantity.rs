//! Quantity expression combinators for Oracle text parsing.
//!
//! Parses quantity expressions from Oracle text: fixed numbers, dynamic references
//! like "the number of creatures you control", "its power", "your life total",
//! "equal to" phrases, and "for each" phrases.

use nom::branch::alt;
use nom::bytes::complete::{tag, take_while1};
use nom::combinator::{map, opt, value};
use nom::sequence::preceded;
use nom::Parser;

use super::error::OracleResult;
use super::primitives::parse_number;
use super::target::parse_type_filter_word;
use crate::parser::oracle_target::parse_type_phrase;
use crate::types::ability::{
    AggregateFunction, ControllerRef, CountScope, FilterProp, ObjectProperty, PlayerScope,
    QuantityExpr, QuantityRef, RoundingMode, TargetFilter, TypeFilter, TypedFilter, ZoneRef,
};
use crate::types::player::PlayerCounterKind;

/// Parse a quantity expression: either a fractional expression, a dynamic reference,
/// or a fixed number. Fractional forms ("half X, rounded up/down") compose over the
/// same `parse_quantity_ref` / `parse_number` primitives used for plain quantities.
pub fn parse_quantity(input: &str) -> OracleResult<'_, QuantityExpr> {
    alt((
        parse_half_rounded,
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
pub fn parse_half_rounded(input: &str) -> OracleResult<'_, QuantityExpr> {
    let (rest, _) = tag("half ").parse(input)?;
    // "half X" and "half of X" are equivalent Oracle surface forms — the
    // "of" variant appears in phrases like "exile the top half of their
    // library, rounded down". Consume the optional connector before the inner.
    let (rest, _) = opt(tag("of ")).parse(rest)?;
    let (rest, inner) = parse_half_rounded_inner(rest)?;
    let (rest, rounding) = parse_rounding_suffix(rest)?;
    Ok((
        rest,
        QuantityExpr::HalfRounded {
            inner: Box::new(inner),
            rounding,
        },
    ))
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
/// `"they"` arm uses `ControllerRef::You` because `player_scope` iteration
/// rebinds controller to the iterating player; the `rewrite_player_scope_refs`
/// walker is NOT required for this path because the typed filter's
/// `ControllerRef::You` already resolves against the rebound controller.
fn parse_possessive_objects_they_control(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("the ").parse(input)?;
    let (rest, tf) = parse_type_filter_word(rest)?;
    let (rest, _) = alt((tag(" they control"), tag(" you control"))).parse(rest)?;
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
        parse_the_number_of,
        parse_distinct_card_types_exiled_with_source,
        parse_linked_exile_mana_value_ref,
        parse_distinct_card_types_in_zone,
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
        // CR 117.1 + CR 202.3: cost-paid object's mana value — must precede
        // `parse_event_context_refs` so the cost-paid resolver wins over the
        // generic event-source resolver for sacrificed/exiled possessives
        // (Food Chain, Burnt Offering, Metamorphosis).
        parse_cost_paid_object_mana_value_ref,
        parse_event_context_refs,
    ))
    .or(alt((
        parse_target_power_ref,
        parse_target_life_ref,
        parse_basic_land_type_count,
        // Bare suffix form — reachable when a parent combinator has already
        // consumed "there are N " (see `parse_there_are_conditions`).
        parse_basic_land_types_among_lands_you_control,
        parse_devotion_ref,
        parse_counters_among_ref,
    )))
    .parse(input)
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
        return Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::Tag),
            )],
        }));
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
    let (rest, _) = tag("the number of ").parse(input)?;
    parse_number_of_inner(rest)
}

/// Parse the inner part after "the number of".
fn parse_number_of_inner(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        parse_distinct_card_types_exiled_with_source,
        parse_distinct_card_types_in_zone,
        // CR 122.1: "[kind] counters <possessor>" must be tried BEFORE the
        // generic type-filter arm so the typed player-counter ref wins over a
        // "[typeword] you control" misread (no `TypeFilter` for counter kinds).
        parse_player_counter_ref_tail,
        // CR 700.8: "creatures in your party" must precede the generic
        // "<type> you control" arm — the trailing "in your party" is what
        // distinguishes party-size from a controlled-creature count.
        parse_creatures_in_your_party_tail,
        parse_number_of_controlled_type,
        // CR 109.4 + CR 115.7: "cards in their <zone>" / "cards in that player's <zone>"
        // must be tried BEFORE the scoped-zone combinator so the target-referring
        // possessive routes to `TargetZoneCardCount` (resolves against the player
        // target in scope) instead of falling back to a controller-less
        // `InZone` filter that counts every player's cards.
        parse_number_of_cards_in_target_zone,
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
        // CR 202.2 + CR 601.2h: "the number of colors of mana spent to cast it"
        // (Wildgrowth Archaic and the cousin-card family).
        value(
            QuantityRef::ColorsSpentOnSelf,
            tag("colors of mana spent to cast it"),
        ),
    )))
    .parse(input)
}

/// Parse "[type(s)] you control" after "the number of".
fn parse_number_of_controlled_type(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, tf) = parse_type_filter_word(input)?;
    let (rest, _) = tag(" you control").parse(rest)?;
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
    Ok((rest, QuantityRef::DistinctCardTypesInZone { zone, scope }))
}

fn parse_distinct_card_types_exiled_with_source(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("card type").parse(input)?;
    let (rest, _) = opt(tag("s")).parse(rest)?;
    let (rest, _) = tag(" among cards exiled with ").parse(rest)?;
    let (rest, _) = alt((
        tag("~"),
        preceded(
            tag("this "),
            take_while1(|c: char| c.is_ascii_alphabetic() || c == '-'),
        ),
    ))
    .parse(rest)?;
    Ok((rest, QuantityRef::DistinctCardTypesExiledBySource))
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

fn parse_card_word(input: &str) -> OracleResult<'_, ()> {
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
fn parse_type_filter_list(input: &str) -> OracleResult<'_, Vec<TypeFilter>> {
    let (mut rest, first) = parse_type_filter_word(input)?;
    let mut filters = vec![first];
    loop {
        let sep = tag::<_, _, nom_language::error::VerboseError<&str>>(" and/or ")
            .parse(rest)
            .or_else(|_| tag::<_, _, nom_language::error::VerboseError<&str>>(" and ").parse(rest))
            .or_else(|_| tag::<_, _, nom_language::error::VerboseError<&str>>(" or ").parse(rest));
        let Ok((next_rest, _)) = sep else { break };
        let Ok((after_type, next)) = parse_type_filter_word(next_rest) else {
            break;
        };
        filters.push(next);
        rest = after_type;
    }
    Ok((rest, filters))
}

fn parse_zone_ref_singular(input: &str) -> OracleResult<'_, ZoneRef> {
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
        value(QuantityRef::SelfPower, tag("its power")),
        value(QuantityRef::SelfPower, tag("~'s power")),
        value(QuantityRef::SelfPower, tag("this creature's power")),
        value(QuantityRef::SelfPower, tag("this card's power")),
    ))
    .parse(input)
}

/// Parse "its toughness" / "~'s toughness" / "this creature's toughness" /
/// "this card's toughness". See `parse_self_power_ref` for the card-vs-creature
/// rationale.
fn parse_self_toughness_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(QuantityRef::SelfToughness, tag("its toughness")),
        value(QuantityRef::SelfToughness, tag("~'s toughness")),
        value(QuantityRef::SelfToughness, tag("this creature's toughness")),
        value(QuantityRef::SelfToughness, tag("this card's toughness")),
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
            QuantityRef::LifeGainedThisTurn,
            tag("total life you gained this turn"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn,
            tag("total life you've gained this turn"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn,
            tag("the life you've gained this turn"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn,
            tag("the life you gained this turn"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn,
            tag("life you've gained this turn"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn,
            tag("life you gained this turn"),
        ),
        // Duration-stripped forms
        value(
            QuantityRef::LifeGainedThisTurn,
            tag("the life you've gained"),
        ),
        value(QuantityRef::LifeGainedThisTurn, tag("the life you gained")),
        value(QuantityRef::LifeGainedThisTurn, tag("life you've gained")),
        value(QuantityRef::LifeGainedThisTurn, tag("life you gained")),
    ))
    .parse(input)
}

/// Parse "your starting life total".
fn parse_starting_life_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    value(
        QuantityRef::StartingLifeTotal,
        tag("your starting life total"),
    )
    .parse(input)
}

/// CR 117.1 + CR 202.3: Cost-paid object's mana value.
///
/// Composes the prefix grammar
/// `[the] (sacrificed|exiled) (creature|card|permanent|artifact)'s (mana value|converted mana cost)`
/// into a single typed combinator. Each axis is a single `alt()` over
/// independent variants — adding a new participle (e.g. "discarded"), a new
/// noun, or the British spelling of "mana value" extends one alt branch
/// rather than adding a new top-level arm.
///
/// Used by Food Chain ("1 plus the exiled creature's mana value"),
/// Burnt Offering / Metamorphosis ("the sacrificed creature's mana value"),
/// and the broader cost-paid-by-property class.
fn parse_cost_paid_object_mana_value_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = opt(tag("the ")).parse(input)?;
    let (rest, _) = alt((tag("sacrificed "), tag("exiled "), tag("discarded "))).parse(rest)?;
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
    let (rest, _) = alt((tag("'s mana value"), tag("'s converted mana cost"))).parse(rest)?;
    Ok((rest, QuantityRef::CostPaidObjectManaValue))
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
        value(
            QuantityRef::EventContextSourcePower,
            tag("that creature's power"),
        ),
        value(
            QuantityRef::EventContextSourceToughness,
            tag("that creature's toughness"),
        ),
        // "Whenever you cast an enchantment spell, ... equal to that spell's
        // mana value" (Dusty Parlor) — the SpellCast event's source object is
        // the spell itself, so CMC reads cleanly off it.
        value(
            QuantityRef::EventContextSourceManaValue,
            tag("that spell's mana value"),
        ),
    ))
    .parse(input)
}

/// Parse target-creature power refs:
///   - Saxon-genitive: "target creature's power" / "the target creature's power"
///   - Of-form: "the power of target creature [you control|an opponent controls]?"
///
/// All variants resolve to the same `QuantityRef::TargetPower`. CR 107.1.
/// Longest-first ordering: the controller-qualified of-form variants must come
/// before the bare of-form so `alt`'s short-circuit doesn't strand the
/// "you control" / "an opponent controls" suffix as un-consumed remainder
/// (which would cause `parse_quantity_ref`'s `rest.is_empty()` check to fail).
/// Soul's Majesty, Predator's Rapport, and similar.
fn parse_target_power_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(QuantityRef::TargetPower, tag("target creature's power")),
        value(QuantityRef::TargetPower, tag("the target creature's power")),
        value(
            QuantityRef::TargetPower,
            tag("the power of target creature you control"),
        ),
        value(
            QuantityRef::TargetPower,
            tag("the power of target creature an opponent controls"),
        ),
        value(
            QuantityRef::TargetPower,
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

/// Parse the bare domain suffix: "basic land types among lands you control".
///
/// Factored out so both the full "the number of ..." form (Domain quantity) and
/// the "there are N ..." condition form (see `parse_there_are_conditions` in
/// `oracle_nom/condition.rs`) share a single tag authority.
fn parse_basic_land_types_among_lands_you_control(input: &str) -> OracleResult<'_, QuantityRef> {
    value(
        QuantityRef::BasicLandTypeCount,
        tag("basic land types among lands you control"),
    )
    .parse(input)
}

/// Parse "the number of basic land types among lands you control" (Domain).
fn parse_basic_land_type_count(input: &str) -> OracleResult<'_, QuantityRef> {
    preceded(
        tag("the number of "),
        parse_basic_land_types_among_lands_you_control,
    )
    .parse(input)
}

/// Parse devotion references.
fn parse_devotion_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("your devotion to ").parse(input)?;
    let (rest, color) = super::primitives::parse_color(rest)?;
    // Check for " and [color]" for multi-color devotion
    if let Ok((rest2, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>(" and ").parse(rest)
    {
        if let Ok((rest3, color2)) = super::primitives::parse_color(rest2) {
            return Ok((
                rest3,
                QuantityRef::Devotion {
                    colors: vec![color, color2],
                },
            ));
        }
    }
    Ok((
        rest,
        QuantityRef::Devotion {
            colors: vec![color],
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
    alt((
        parse_distinct_card_types_in_zone,
        parse_zone_card_count,
        parse_for_each_attached_to_source,
        // CR 700.8: "creature in your party" must precede the generic
        // "<type> you control" arm — same reason as in
        // `parse_number_of_inner`.
        parse_creature_in_party_for_each,
        parse_for_each_controlled_type,
    ))
    .parse(input)
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
    while let Ok((after_and, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>(" and ").parse(rest)
    {
        let (after_type, next) = parse_type_filter_word(after_and)?;
        types.push(next);
        rest = after_type;
    }
    // CR 301.5 + CR 303.4 + CR 613.4c: Two referents share the "<type>
    // [and <type>]* attached to <referent>" shape. The static parser already
    // normalizes the source's printed name to `~`, so a literal `~` referent
    // means "attached to the static's source object" (Kellan, the
    // Fae-Blooded — `AttachedToSource`). The pronoun `it` is anaphoric on the
    // affected subject of the surrounding continuous modification — for
    // "Enchanted creature gets +N/+M for each Aura and Equipment attached to
    // it", "it" refers to the enchanted creature, the per-recipient host of
    // the layer-evaluated boost (`AttachedToRecipient`). Both literals are
    // single-token leaves of the same combinator, so we dispatch with `alt`
    // and select the matching `FilterProp` from a typed pair.
    let (rest, prop) = alt((
        value(FilterProp::AttachedToSource, tag(" attached to ~")),
        value(FilterProp::AttachedToRecipient, tag(" attached to it")),
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

fn parse_for_each_controlled_type(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, tf) = parse_type_filter_word(input)?;
    let (rest, _) = tag(" you control").parse(rest)?;
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

/// Parse "your speed".
fn parse_speed_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    value(QuantityRef::Speed, tag("your speed")).parse(input)
}

/// CR 122.1: Parse "[kind] counters <possessor>" → `QuantityRef::PlayerCounter`.
///
/// Reached after `parse_the_number_of` consumes the leading `"the number of "`.
/// Composes a typed kind alt and a typed possessor alt — no string matching
/// downstream and no permutation-enumerated tag lists.
fn parse_player_counter_ref_tail(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, kind) = parse_player_counter_kind(input)?;
    let (rest, _) = tag(" counters ").parse(rest)?;
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
/// `PlayerCounterKind` variant directly (no intermediate string).
fn parse_player_counter_kind(input: &str) -> OracleResult<'_, PlayerCounterKind> {
    alt((
        value(PlayerCounterKind::Experience, tag("experience")),
        value(PlayerCounterKind::Poison, tag("poison")),
        value(PlayerCounterKind::Rad, tag("rad")),
        value(PlayerCounterKind::Ticket, tag("ticket")),
    ))
    .parse(input)
}

/// CR 122.1 + CR 109.5: Typed possessor alt mapping to `CountScope`. Each arm
/// emits the scope variant directly. Targeted-player phrasings ("target
/// opponent has", "that player has") are intentionally not represented
/// because no current card requires them; extending here is a typed
/// addition, not a string-match retrofit.
fn parse_player_counter_possessor(input: &str) -> OracleResult<'_, CountScope> {
    alt((
        value(CountScope::Controller, tag("you have")),
        value(CountScope::Opponents, tag("each opponent has")),
        value(CountScope::All, tag("each player has")),
    ))
    .parse(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AggregateFunction, FilterProp, ObjectProperty, TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::mana::ManaColor;

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
        assert_eq!(q, QuantityRef::SelfPower);
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
            assert_eq!(q, QuantityRef::SelfPower, "phrase: {phrase}");
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
    fn test_parse_distinct_card_types_in_exile() {
        let (rest, q) =
            parse_quantity_ref("the number of card types among cards in exile").unwrap();
        assert_eq!(
            q,
            QuantityRef::DistinctCardTypesInZone {
                zone: ZoneRef::Exile,
                scope: CountScope::All,
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_distinct_card_types_exiled_with_source() {
        let (rest, q) =
            parse_quantity_ref("the number of card types among cards exiled with ~").unwrap();
        assert_eq!(q, QuantityRef::DistinctCardTypesExiledBySource);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_distinct_card_types_exiled_with_this_creature() {
        let (rest, q) =
            parse_quantity_ref("the number of card types among cards exiled with this creature")
                .unwrap();
        assert_eq!(q, QuantityRef::DistinctCardTypesExiledBySource);
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
        assert_eq!(q, QuantityRef::LifeGainedThisTurn);
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
    /// resolves to `QuantityRef::ColorsSpentOnSelf`. Used by Wildgrowth Archaic
    /// and the cousin-card family for ETB-counter quantity expressions.
    #[test]
    fn parses_colors_spent_to_cast_it() {
        let (rest, q) =
            parse_quantity_ref("the number of colors of mana spent to cast it").unwrap();
        assert_eq!(q, QuantityRef::ColorsSpentOnSelf);
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

        let (rest2, q2) = parse_quantity_ref("that creature's power").unwrap();
        assert_eq!(q2, QuantityRef::EventContextSourcePower);
        assert_eq!(rest2, "");
    }

    /// CR 603.7c: Dusty Parlor — the SpellCast event's source object is the
    /// spell, so "that spell's mana value" reads its CMC via the shared
    /// `EventContextSourceManaValue` resolution path.
    #[test]
    fn test_parse_that_spells_mana_value() {
        let (rest, q) = parse_quantity_ref("that spell's mana value").unwrap();
        assert_eq!(q, QuantityRef::EventContextSourceManaValue);
        assert_eq!(rest, "");
    }

    /// CR 117.1 + CR 202.3: Food Chain — "the exiled creature's mana value"
    /// resolves to the cost-paid object snapshot (NOT the trigger-event
    /// source), so the parser must emit `CostPaidObjectManaValue`.
    #[test]
    fn test_parse_exiled_creatures_mana_value() {
        let (rest, q) = parse_quantity_ref("the exiled creature's mana value").unwrap();
        assert_eq!(q, QuantityRef::CostPaidObjectManaValue);
        assert_eq!(rest, "");
    }

    /// CR 117.1 + CR 202.3: Burnt Offering / Metamorphosis — additional
    /// sacrifice cost referenced as "the sacrificed creature's mana value".
    #[test]
    fn test_parse_sacrificed_creatures_mana_value() {
        let (rest, q) = parse_quantity_ref("the sacrificed creature's mana value").unwrap();
        assert_eq!(q, QuantityRef::CostPaidObjectManaValue);
        assert_eq!(rest, "");
    }

    /// Parser must accept the legacy "converted mana cost" phrasing.
    #[test]
    fn test_parse_sacrificed_creatures_converted_mana_cost() {
        let (rest, q) =
            parse_quantity_ref("the sacrificed creature's converted mana cost").unwrap();
        assert_eq!(q, QuantityRef::CostPaidObjectManaValue);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_equal_to() {
        let (rest, q) = parse_equal_to("equal to its power").unwrap();
        assert_eq!(
            q,
            QuantityExpr::Ref {
                qty: QuantityRef::SelfPower
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

    #[test]
    fn test_parse_devotion() {
        let (rest, q) = parse_quantity_ref("your devotion to red").unwrap();
        assert_eq!(
            q,
            QuantityRef::Devotion {
                colors: vec![ManaColor::Red]
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
                colors: vec![ManaColor::White, ManaColor::Black]
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_target_power() {
        let (rest, q) = parse_quantity_ref("target creature's power").unwrap();
        assert_eq!(q, QuantityRef::TargetPower);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_basic_land_type_count() {
        let (rest, q) =
            parse_quantity_ref("the number of basic land types among lands you control").unwrap();
        assert_eq!(q, QuantityRef::BasicLandTypeCount);
        assert_eq!(rest, "");
    }

    // --- Half-rounded fractional expressions (CR 107.1a) ---

    #[test]
    fn test_parse_half_their_library_rounded_down() {
        let (rest, q) = parse_quantity("half their library, rounded down").unwrap();
        assert_eq!(
            q,
            QuantityExpr::HalfRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::TargetZoneCardCount {
                        zone: ZoneRef::Library,
                    },
                }),
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
            QuantityExpr::HalfRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Target
                    },
                }),
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
            QuantityExpr::HalfRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Target
                    },
                }),
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
            QuantityExpr::HalfRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::SelfPower,
                }),
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
            QuantityExpr::HalfRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Controller
                    },
                }),
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
            QuantityExpr::HalfRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Target
                    },
                }),
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
            QuantityExpr::HalfRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::TargetZoneCardCount {
                        zone: ZoneRef::Library,
                    },
                }),
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
            QuantityExpr::HalfRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Target
                    },
                }),
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
        assert!(matches!(q, QuantityExpr::HalfRounded { .. }));
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
}
