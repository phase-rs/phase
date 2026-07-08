use std::str::FromStr;

use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::{multispace0, one_of};
use nom::combinator::{all_consuming, opt, value};
use nom::sequence::terminated;
use nom::Parser;

use super::oracle_nom::condition as nom_condition;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_nom::target as nom_target;
use super::oracle_target::parse_type_phrase;
use crate::types::ability::{
    Comparator, ControllerRef, FilterProp, ParsedCondition, PlayerFilter, PlayerScope,
    QuantityExpr, QuantityRef, StaticCondition, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::counter::{parse_counter_type, CounterType};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::zones::Zone;

fn scan_source_zone_filter(text: &str) -> Option<Zone> {
    let mut offset = 0;
    while offset <= text.len() {
        if let Ok((rest, zone)) = super::oracle_nom::filter::parse_zone_filter(&text[offset..]) {
            if rest
                .chars()
                .next()
                .is_none_or(|ch| matches!(ch, ' ' | ',' | '.'))
            {
                return Some(zone);
            }
        }
        match text[offset..].find(' ') {
            Some(i) => offset += i + 1,
            None => break,
        }
    }
    None
}

/// CR 601.3 / CR 602.5: Parse a restriction condition from Oracle text into a typed
/// `ParsedCondition`. These conditions gate whether a spell can be cast or ability activated.
/// Returns `None` for unrecognized conditions (caller treats `None` as permissive true).
/// Normalizes input: lowercase, trim, strip trailing period.
///
/// Tries compound forms first (`X and Y`, `X or Y`, `not X`) so logical composition
/// of leaf conditions composes through `ParsedCondition::And`/`Or`/`Not` per the
/// standard combinator triple shared with `AbilityCondition` and `TriggerCondition`.
pub fn parse_restriction_condition(text: &str) -> Option<ParsedCondition> {
    let lower = text.trim().trim_end_matches('.').to_lowercase();
    parse_compound_condition(&lower).or_else(|| parse_condition_text(&lower))
}

/// CR 601.3 / CR 602.5: Try logical-composition forms of restriction conditions.
/// Order matters: try `and`/`or` splits first (binary outer structure), then leading
/// `not ` (unary). Each fragment must parse as an atomic condition; if any fragment
/// fails, the whole compound parse returns `None` so the caller falls back to atomic.
fn parse_compound_condition(text: &str) -> Option<ParsedCondition> {
    if let Some(conditions) = parse_connector_split(text, " and ") {
        return Some(ParsedCondition::And { conditions });
    }
    if let Some(conditions) = parse_connector_split(text, " or ") {
        return Some(ParsedCondition::Or { conditions });
    }
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("not ").parse(text) {
        let inner = parse_condition_text(rest)?;
        return Some(ParsedCondition::Not {
            condition: Box::new(inner),
        });
    }
    None
}

/// Split `text` on `connector` and parse each fragment as an atomic condition.
/// Returns `None` if the connector is absent, only one fragment exists, or any
/// fragment fails to parse — leaving the caller to try atomic parsing on the full text.
/// This guards against false splits like "more cards in hand than each opponent" being
/// torn apart by " or " inside a single atomic phrase: each fragment must be a complete
/// atomic condition for the compound parse to succeed.
fn parse_connector_split(text: &str, connector: &str) -> Option<Vec<ParsedCondition>> {
    if !text.contains(connector) {
        return None;
    }
    let fragments: Vec<&str> = text.split(connector).map(str::trim).collect();
    if fragments.len() < 2 {
        return None;
    }
    fragments
        .into_iter()
        .map(parse_condition_fragment_text)
        .collect::<Option<Vec<_>>>()
        .filter(|v| v.len() >= 2)
}

fn parse_condition_fragment_text(text: &str) -> Option<ParsedCondition> {
    parse_condition_text(strip_leading_condition_fragment_marker(text))
}

fn strip_leading_condition_fragment_marker(text: &str) -> &str {
    let text = text.trim();
    if let Ok((rest, _)) = alt((tag::<_, _, OracleError<'_>>("if "), tag("only if "))).parse(text) {
        rest.trim()
    } else {
        text
    }
}

fn parse_condition_text(text: &str) -> Option<ParsedCondition> {
    // Counter-threshold predicates ("there are N counters on this artifact",
    // "there are no charge counters on this artifact"). Tried before
    // parse_source_condition because they have no self-ref subject prefix —
    // the "there are" lead-in is matched directly by the helpers' own gates.
    if let Some((counter_type, count)) = parse_counter_requirement(text) {
        return Some(ParsedCondition::SourceHasCounterAtLeast {
            counter_type,
            count,
        });
    }
    if let Some(counter_type) = parse_counter_absence_requirement(text) {
        return Some(ParsedCondition::SourceHasNoCounter { counter_type });
    }
    if let Some(condition) = parse_source_condition(text) {
        return Some(condition);
    }
    if let Some(condition) = parse_you_control_condition(text) {
        return Some(condition);
    }
    if let Some(condition) = parse_zone_card_condition(text) {
        return Some(condition);
    }
    if let Some(condition) = parse_hand_condition(text) {
        return Some(condition);
    }

    // Event-based conditions: structured nom matching for event phrases.
    if let Some(condition) = parse_event_condition(text) {
        return Some(condition);
    }

    // CR 601.3d + CR 608.2c: "it targets a [filter]" — gates a casting permission
    // (typically "as though it had flash") on the spell-being-cast's chosen targets.
    // The pronoun `it` here refers to the in-flight spell (Timely Ward — "you may
    // cast this spell as though it had flash if it targets a commander").
    if let Some(condition) = parse_spell_targets_filter(text) {
        return Some(condition);
    }

    if value(
        (),
        tag::<_, _, OracleError<'_>>("you have the city's blessing"),
    )
    .parse(text)
    .is_ok()
    {
        return Some(ParsedCondition::HasCityBlessing);
    }

    if let Some(count) = parse_numeric_threshold(text, "you attacked with ", " creatures this turn")
    {
        return Some(ParsedCondition::YouAttackedWithAtLeast {
            count: count as u32,
            filter: None,
        });
    }
    if let Some(count) =
        parse_numeric_threshold(text, "you attacked with ", " or more creatures this turn")
    {
        return Some(ParsedCondition::YouAttackedWithAtLeast {
            count: count as u32,
            filter: None,
        });
    }
    // CR 508.1a: "you attacked with a/an <filter> this turn" — at least one
    // attacker of the given kind. Distinct from the numeric "N creatures"
    // thresholds above (which carry no type qualifier). The trailing "this turn"
    // may already be stripped upstream (e.g. an activated-ability duration
    // parser peels it before the cost-reduction condition is reparsed), so both
    // the suffixed and bare forms are accepted. Thaumaton Torpedo: "...if you
    // attacked with a Spacecraft this turn".
    if let Some(condition) = parse_you_attacked_with_filter(text) {
        return Some(condition);
    }
    if all_consuming(alt((
        value(
            (),
            tag::<_, _, OracleError<'_>>("you've played a land this turn"),
        ),
        value((), tag("you have played a land this turn")),
        value((), tag("you played a land this turn")),
    )))
    .parse(text)
    .is_ok()
    {
        return Some(ParsedCondition::YouPlayedLandThisTurn);
    }
    if let Some(count) = parse_numeric_threshold(text, "you've cast ", " or more spells this turn")
    {
        return Some(ParsedCondition::YouCastSpellCountAtLeast {
            count: count as u32,
        });
    }
    if let Some(count) =
        parse_numeric_threshold(text, "", " or more cards left your graveyard this turn")
    {
        return Some(ParsedCondition::CardsLeftYourGraveyardThisTurnAtLeast {
            count: count as u32,
        });
    }
    if let Some(condition) = parse_quantity_restriction_condition(text) {
        return Some(condition);
    }
    None
}

/// CR 508.1a: Parse "you attacked with a/an <filter>[ this turn]" into a
/// `ParsedCondition::YouAttackedWithAtLeast { count: 1, filter }`. The `<filter>`
/// is delegated to `parse_type_phrase` so the whole class of attacker qualifiers
/// (Spacecraft, Vehicle, a specific creature type, …) is covered by the shared
/// type-phrase combinator rather than a per-card literal. Returns `None` unless
/// the entire phrase after the filter is consumed (modulo an optional " this
/// turn"), keeping unrecognized qualifiers an honest gap.
fn parse_you_attacked_with_filter(text: &str) -> Option<ParsedCondition> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("you attacked with ")
        .parse(text)
        .ok()?;
    let (filter, remainder) = parse_type_phrase(rest);
    // Reject the bare/untyped case: `parse_type_phrase` returns `Any` when no
    // type word matched, which would over-match "you attacked with three or more
    // creatures this turn" (handled by the numeric thresholds above). Require a
    // concrete typed filter here.
    if matches!(filter, TargetFilter::Any) {
        return None;
    }
    // Consume an optional trailing " this turn" and any trailing punctuation with
    // combinators (no manual string trimming), then require the phrase to be fully
    // consumed so unrecognized qualifiers stay an honest gap. The duration suffix
    // may already be stripped upstream, so it is optional.
    let (remainder, _) = multispace0::<_, OracleError<'_>>(remainder).ok()?;
    let (remainder, _) = opt(tag::<_, _, OracleError<'_>>("this turn"))
        .parse(remainder)
        .ok()?;
    let (remainder, _) = multispace0::<_, OracleError<'_>>(remainder).ok()?;
    let (remainder, _) = opt(one_of::<_, _, OracleError<'_>>(".,;"))
        .parse(remainder)
        .ok()?;
    let (remainder, _) = multispace0::<_, OracleError<'_>>(remainder).ok()?;
    if !remainder.is_empty() {
        return None;
    }
    Some(ParsedCondition::YouAttackedWithAtLeast {
        count: 1,
        filter: Some(filter),
    })
}

fn parse_quantity_restriction_condition(text: &str) -> Option<ParsedCondition> {
    let (_rest, condition) = all_consuming(nom_condition::parse_inner_condition)
        .parse(text)
        .ok()?;
    static_condition_to_restriction_condition(condition)
}

fn static_condition_to_restriction_condition(
    condition: StaticCondition,
) -> Option<ParsedCondition> {
    match condition {
        StaticCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => Some(ParsedCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        }),
        StaticCondition::And { conditions } => conditions
            .into_iter()
            .map(static_condition_to_restriction_condition)
            .collect::<Option<Vec<_>>>()
            .map(|conditions| ParsedCondition::And { conditions }),
        StaticCondition::Or { conditions } => conditions
            .into_iter()
            .map(static_condition_to_restriction_condition)
            .collect::<Option<Vec<_>>>()
            .map(|conditions| ParsedCondition::Or { conditions }),
        StaticCondition::Not { condition } => static_condition_to_restriction_condition(*condition)
            .map(|condition| ParsedCondition::Not {
                condition: Box::new(condition),
            }),
        // CR 601.3 + CR 602.5: a presence check ("a creature is attacking you",
        // "you control a [type]") is equivalent to "the count of matching
        // objects is at least one". `ParsedCondition` has no `IsPresent`
        // variant, so reuse its generic `QuantityComparison` over an
        // `ObjectCount` of the same filter — letting cast/activation
        // restrictions ("Cast this spell only if a creature is attacking you" —
        // Confront the Assault) reuse the full presence-condition vocabulary.
        StaticCondition::IsPresent {
            filter: Some(filter),
        } => Some(ParsedCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        }),
        // CR 102.1: "it's your turn" — the active player is the scoped player.
        // The `Not` recursion arm above yields `Not(IsYourTurn)` for
        // "it's not your turn".
        StaticCondition::DuringYourTurn => Some(ParsedCondition::IsYourTurn),
        _ => None,
    }
}

fn parse_source_condition(text: &str) -> Option<ParsedCondition> {
    // Source conditions accept self-reference and source-state subjects:
    //   "~ <state>"          — canonical normalized self-ref (e.g., "~ is attacking")
    //   "this <noun>"        — explicit self-reference ("this creature is blocked")
    //   "enchanted <noun>"   — Aura-attached source predicate
    //   "from your <zone>"   — zone-based source predicate
    if alt((
        tag::<_, _, OracleError<'_>>("this "),
        tag("enchanted "),
        tag("from your "),
        tag("in "),
        tag("on "),
        tag("~'s "),
        tag("~ "),
    ))
    .parse(text)
    .is_err()
    {
        return None;
    }
    // Zone-based source conditions: "from your graveyard", "[subject] in your graveyard",
    // "in exile", "from your hand", etc. Delegate to the shared zone-phrase scanner so
    // the full zone vocabulary (graveyard/hand/exile/library/battlefield) is covered
    // uniformly with word-boundary safety and the combinator-mandated parse path.
    if let Some((zone, _ctrl, _props)) = super::oracle_target::scan_zone_phrase(text) {
        return Some(ParsedCondition::SourceInZone { zone });
    }
    if let Some(zone) = scan_source_zone_filter(text) {
        return Some(ParsedCondition::SourceInZone { zone });
    }
    // Source state: scan for state keywords after the subject using nom at word boundaries
    if let Ok((_, condition)) = scan_source_state(text) {
        return Some(condition);
    }
    // "enchanted [type] is untapped"
    if text.contains("is untapped") {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("enchanted ").parse(text) {
            if let Some(type_text) = rest.strip_suffix(" is untapped") {
                if let Some(core_type) = parse_core_type_word(type_text) {
                    return Some(ParsedCondition::SourceUntappedAttachedTo {
                        required_type: core_type,
                    });
                }
            }
        }
    }
    // "this creature doesn't have [keyword]" / "~ doesn't have [keyword]"
    if let Ok((keyword_text, _)) = alt((
        tag::<_, _, OracleError<'_>>("this creature doesn't have "),
        tag("~ doesn't have "),
    ))
    .parse(text)
    {
        let keyword: Keyword = keyword_text.trim().parse().unwrap();
        if !matches!(keyword, Keyword::Unknown(_)) {
            return Some(ParsedCondition::SourceLacksKeyword { keyword });
        }
    }
    // "this creature is [color]" / "~ is [color]"
    if let Ok((color_text, _)) = alt((
        tag::<_, _, OracleError<'_>>("this creature is "),
        tag("~ is "),
    ))
    .parse(text)
    {
        if let Some(color) = parse_color_word(color_text) {
            return Some(ParsedCondition::SourceIsColor { color });
        }
    }
    // Power threshold: "this creature's power is N or greater" / "~'s power is N or greater"
    if let Some(power) = parse_source_power_threshold(text) {
        return Some(ParsedCondition::SourcePowerAtLeast { minimum: power });
    }
    if let Some((counter_type, count)) = parse_counter_requirement(text) {
        return Some(ParsedCondition::SourceHasCounterAtLeast {
            counter_type,
            count,
        });
    }
    if let Some(counter_type) = parse_counter_absence_requirement(text) {
        return Some(ParsedCondition::SourceHasNoCounter { counter_type });
    }
    None
}

fn parse_source_power_threshold(text: &str) -> Option<i32> {
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("this creature's power is "),
        tag("~'s power is "),
    ))
    .parse(text)
    .ok()?;
    let (rest, power) = nom_primitives::parse_number(rest).ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" or greater")
        .parse(rest)
        .ok()?;
    rest.trim().is_empty().then_some(power as i32)
}

/// CR 602.5b: Parse "[you / an opponent] control(s) a creature with [keyword]".
/// The controller prefix is matched with a nom `alt` so both controller scopes
/// flow through the single parameterized `ParsedCondition::ControlsCreatureWithKeyword`.
fn parse_controls_creature_with_keyword(text: &str) -> Option<(ControllerRef, Keyword)> {
    let (keyword_text, controller) = alt((
        value(
            ControllerRef::You,
            alt((
                tag::<_, _, OracleError<'_>>("you control a creature with "),
                tag("you control a creature that has "),
            )),
        ),
        value(
            ControllerRef::Opponent,
            alt((
                tag("an opponent controls a creature with "),
                tag("an opponent controls a creature that has "),
            )),
        ),
    ))
    .parse(text)
    .ok()?;
    let keyword: Keyword = keyword_text.trim().parse().ok()?;
    (!matches!(keyword, Keyword::Unknown(_))).then_some((controller, keyword))
}

fn parse_you_control_condition(text: &str) -> Option<ParsedCondition> {
    // "you control a [subtype] or there is a [subtype] card in your graveyard"
    if text.contains(" or there is a ") && text.contains(" card in your graveyard") {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you control a ").parse(text) {
            if let Some(subtype) = rest.split(" or ").next() {
                return Some(ParsedCondition::YouControlSubtypeOrGraveyardCardSubtype {
                    subtype: subtype.to_string(),
                });
            }
        }
    }
    if let Some(subtypes) = parse_you_control_land_subtypes(text) {
        return Some(ParsedCondition::YouControlLandSubtypeAny { subtypes });
    }
    if let Some(count) = parse_numeric_threshold(
        text,
        "creatures you control have total power ",
        " or greater",
    ) {
        return Some(ParsedCondition::CreaturesYouControlTotalPowerAtLeast {
            minimum: count as i32,
        });
    }
    if let Some(count) = parse_numeric_threshold(
        text,
        "you control ",
        " or more creatures with different powers",
    ) {
        return Some(ParsedCondition::YouControlDifferentPowerCreatureCountAtLeast { count });
    }
    if let Some(count) =
        parse_numeric_threshold(text, "you control ", " or more lands with the same name")
    {
        return Some(ParsedCondition::YouControlLandsWithSameNameAtLeast { count });
    }
    if let Some(count) = parse_numeric_threshold(text, "you control ", " or more snow permanents") {
        return Some(ParsedCondition::YouControlSnowPermanentCountAtLeast { count });
    }
    // CR 205.4a: A counted "you control N or more <supertype> <type>s" restriction
    // (two or more basic lands, two or more legendary creatures) must decompose the
    // supertype into a `HasSupertype` object-count the same way the singular arm
    // does — otherwise the bare-subtype catch-all below dumps "basic land" /
    // "legendary creature" into a non-existent subtype and the restriction can never
    // be satisfied. Runs before `parse_you_control_subtype_count`; the
    // `parse_supertype_prefix` gate inside the helper keeps ordinary subtype counts
    // ("two or more vampires") on the subtype path.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you control ").parse(text) {
        if let Ok((type_text, count_text)) = terminated(
            take_until::<_, _, OracleError<'_>>(" or more "),
            tag(" or more "),
        )
        .parse(rest)
        {
            if let Some(count) = parse_count_word(count_text) {
                let singular = type_text.trim().trim_end_matches('.').trim_end_matches('s');
                if let Some(cond) = parse_you_control_supertype_count(singular, count) {
                    return Some(cond);
                }
            }
        }
    }
    // The generic bare-subtype catch-all must run AFTER the specific "creatures with
    // different powers" / "lands with the same name" / "snow permanents" suffix arms
    // above. Otherwise it greedily consumes those qualifier phrases via its bare
    // `" or more "` split and dumps the whole phrase into a stringly-typed subtype
    // (e.g. subtype "creatures with different power"), shadowing the dedicated
    // YouControlDifferentPowerCreatureCountAtLeast / YouControlLandsWithSameNameAtLeast
    // variants so those cards never reach the correct parse.
    if let Some((count, subtype)) = parse_you_control_subtype_count(text) {
        return Some(ParsedCondition::YouControlSubtypeCountAtLeast { subtype, count });
    }
    // "you control N or more [color] permanents" / "you control N or more [core type]s"
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you control ").parse(text) {
        if let Some((count_text, type_text)) = rest.split_once(" or more ") {
            if let Some(count) = parse_count_word(count_text) {
                let type_text = type_text.trim().trim_end_matches('.');
                if let Some(color) = parse_color_word(type_text.trim_end_matches(" permanents")) {
                    return Some(ParsedCondition::YouControlColorPermanentCountAtLeast {
                        color,
                        count,
                    });
                }
                if let Some(core_type) = parse_core_type_word(type_text) {
                    return Some(ParsedCondition::YouControlCoreTypeCountAtLeast {
                        core_type,
                        count,
                    });
                }
            }
        }
    }
    if let Some(power) =
        parse_numeric_threshold(text, "you control a creature with power ", " or greater")
    {
        return Some(ParsedCondition::YouControlCreatureWithPowerAtLeast {
            minimum: power as i32,
        });
    }
    if let Some((power, toughness)) = parse_creature_pt_condition(text) {
        return Some(ParsedCondition::YouControlCreatureWithPt { power, toughness });
    }
    // CR 602.5b: "[you / an opponent] control(s) a creature with [keyword]"
    if let Some((controller, keyword)) = parse_controls_creature_with_keyword(text) {
        return Some(ParsedCondition::ControlsCreatureWithKeyword {
            controller,
            keyword,
        });
    }
    // "you control a/another legendary creature"
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you control ").parse(text) {
        if rest.contains("legendary creature") {
            return Some(ParsedCondition::YouControlLegendaryCreature);
        }
        if rest.contains("colorless creature") {
            return Some(ParsedCondition::YouControlAnotherColorlessCreature);
        }
    }
    // "you control fewer creatures than each opponent"
    if tag::<_, _, OracleError<'_>>("you control fewer creatures than")
        .parse(text)
        .is_ok()
    {
        return Some(ParsedCondition::QuantityVsEachOpponent {
            lhs: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
            comparator: Comparator::LT,
            rhs: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        });
    }
    // "you control no creatures"
    if tag::<_, _, OracleError<'_>>("you control no creatures")
        .parse(text)
        .is_ok()
    {
        return Some(ParsedCondition::YouControlNoCreatures);
    }
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("you control an "),
        tag("you control a "),
    ))
    .parse(text)
    {
        if let Some(name) = rest.strip_suffix(" planeswalker") {
            return Some(ParsedCondition::YouControlNamedPlaneswalker {
                name: capitalize_condition_word(name),
            });
        }
    }
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("you control an "),
        tag("you control a "),
    ))
    .parse(text)
    {
        // CR 205.4a: A supertype adjective ("snow", "basic", "legendary", "world")
        // in "you control a <supertype> <type>" must decompose into a
        // `HasSupertype` filter property plus the core type / subtype — it must NOT
        // be dumped whole into a stringly-typed `subtype`. No permanent has the
        // subtype "snow land" or "snow mountain", so the bare-subtype parse below
        // makes the restriction permanently unsatisfiable (Blizzard could never be
        // cast; Goblin Ski Patrol's ability could never activate). Reuse
        // `parse_type_phrase` — the same combinator that decomposes "basic land
        // card" into `Land` + `HasSupertype Basic` — and count matching permanents
        // you control via the generic `ObjectCount >= 1` presence check.
        if let Some(cond) = parse_you_control_supertype_count(rest, 1) {
            return Some(cond);
        }
        if let Some(core_type) = parse_core_type_word(rest) {
            return Some(ParsedCondition::YouControlCoreTypeCountAtLeast {
                core_type,
                count: 1,
            });
        }
        return Some(ParsedCondition::YouControlSubtypeCountAtLeast {
            subtype: rest.to_string(),
            count: 1,
        });
    }
    None
}

fn parse_zone_card_condition(text: &str) -> Option<ParsedCondition> {
    // "there are N or more ..." forms
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("there are ").parse(text) {
        if let Some((count, after_num)) = super::oracle_util::parse_number(rest) {
            let count = count as usize;
            // "there are N or more card types among cards in your <zone>"
            if let Ok((zone_text, _)) =
                tag::<_, _, OracleError<'_>>("or more card types among cards ").parse(after_num)
            {
                if let Some((_, zone)) = extract_zone_from_suffix(zone_text) {
                    return Some(ParsedCondition::ZoneCardTypeCountAtLeast { zone, count });
                }
            }
            // "there are N or more cards in your <zone>"
            if let Ok((zone_text, _)) =
                tag::<_, _, OracleError<'_>>("or more cards ").parse(after_num)
            {
                if let Some((_, zone)) = extract_zone_from_suffix(zone_text) {
                    return Some(ParsedCondition::ZoneCardCountAtLeast { zone, count });
                }
            }
        }
        // "there are no <subtype> cards in your <zone>"
        if let Ok((no_rest, _)) = tag::<_, _, OracleError<'_>>("no ").parse(rest) {
            if let Some((subtype, zone)) = parse_subtype_zone_suffix(no_rest, " cards ") {
                return Some(ParsedCondition::ZoneSubtypeCardCountAtLeast {
                    zone,
                    subtype: subtype.trim_end_matches('s').to_string(),
                    count: 0,
                });
            }
        }
    }
    // "there is an <subtype> card in your <zone>"
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("there is an ").parse(text) {
        if let Some((subtype, zone)) = parse_subtype_zone_suffix(rest, " card ") {
            return Some(ParsedCondition::ZoneSubtypeCardCountAtLeast {
                zone,
                subtype: subtype.to_string(),
                count: 1,
            });
        }
    }
    // "two or more <subtype> cards are in your <zone>"
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("two or more ").parse(text) {
        if let Some((subtype, zone)) = parse_subtype_zone_suffix(rest, " cards are ") {
            return Some(ParsedCondition::ZoneSubtypeCardCountAtLeast {
                zone,
                subtype: subtype.trim_end_matches('s').to_string(),
                count: 2,
            });
        }
    }
    None
}

/// Extract a zone from a suffix like "in your graveyard" or "from your hand"
/// using the existing `parse_zone_suffix` combinator.
fn extract_zone_from_suffix(suffix: &str) -> Option<(usize, Zone)> {
    let (props, _ctrl, consumed) = super::oracle_target::parse_zone_suffix(suffix)?;
    props.iter().find_map(|p| match p {
        crate::types::ability::FilterProp::InZone { zone } => Some((consumed, *zone)),
        _ => None,
    })
}

/// Split text on a card-word separator (e.g. " card ", " cards are ") and extract the
/// zone from the suffix via `parse_zone_suffix`. Returns `(subtype_text, zone)`.
fn parse_subtype_zone_suffix<'a>(text: &'a str, separator: &str) -> Option<(&'a str, Zone)> {
    let pos = text.find(separator)?;
    let subtype = &text[..pos];
    let after = &text[pos + separator.len()..];
    let (_, zone) = extract_zone_from_suffix(after)?;
    Some((subtype, zone))
}

fn parse_hand_condition(text: &str) -> Option<ParsedCondition> {
    // Quick reject: must reference "hand" somewhere
    if !text.contains("hand") {
        return None;
    }
    // "you have no cards in hand"
    if tag::<_, _, OracleError<'_>>("you have no cards")
        .parse(text)
        .is_ok()
    {
        return Some(ParsedCondition::HandSizeExact { count: 0 });
    }
    // "you have no [kind] cards in hand" — e.g. "you have no land cards in hand".
    // CR 601.3: Cast restriction — hand contains no cards of the given core type
    // or subtype. Use count: 1 + Not because count-at-least 0 is always true.
    // Verified: CR 601.3 (docs/MagicCompRules.txt:2475).
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you have no ").parse(text) {
        if let Ok((_, kind_raw)) = terminated(
            take_until::<_, _, OracleError<'_>>(" card"),
            alt((tag(" cards in hand"), tag(" card in hand"))),
        )
        .parse(rest)
        {
            let kind = kind_raw.trim();
            if let Some(core_type) = parse_core_type_word(kind) {
                return Some(ParsedCondition::Not {
                    condition: Box::new(ParsedCondition::ZoneCoreTypeCardCountAtLeast {
                        zone: Zone::Hand,
                        core_type,
                        count: 1,
                    }),
                });
            }
            if !kind.is_empty() {
                return Some(ParsedCondition::Not {
                    condition: Box::new(ParsedCondition::ZoneSubtypeCardCountAtLeast {
                        zone: Zone::Hand,
                        subtype: kind.to_string(),
                        count: 1,
                    }),
                });
            }
        }
    }
    if tag::<_, _, OracleError<'_>>("you have one or fewer cards in hand")
        .parse(text)
        .is_ok()
    {
        return Some(ParsedCondition::HandSizeOneOf { counts: vec![0, 1] });
    }
    // "you have more cards in hand than each opponent"
    if tag::<_, _, OracleError<'_>>("you have more cards in hand than")
        .parse(text)
        .is_ok()
    {
        return Some(ParsedCondition::QuantityVsEachOpponent {
            lhs: QuantityRef::HandSize {
                player: PlayerScope::Controller,
            },
            comparator: Comparator::GT,
            rhs: QuantityRef::HandSize {
                player: PlayerScope::Controller,
            },
        });
    }
    // "you have exactly N or M cards in hand"
    if let Some(rest) = tag::<_, _, OracleError<'_>>("you have exactly ")
        .parse(text)
        .ok()
        .and_then(|(rest, _)| rest.strip_suffix(" cards in hand"))
    {
        if rest.contains(" or ") {
            let counts: Vec<usize> = rest
                .split(" or ")
                .filter_map(|s| parse_count_word(s.trim()))
                .collect();
            if counts.len() >= 2 {
                return Some(ParsedCondition::HandSizeOneOf { counts });
            }
        }
        if let Some(count) = parse_count_word(rest) {
            return Some(ParsedCondition::HandSizeExact { count });
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Event condition combinators
// ---------------------------------------------------------------------------

/// Parse event-based conditions using nom combinators.
///
/// Categories:
/// - Exact phrase: `terminated(tag("prefix"), tag(" this turn"))` — precise structural matching
/// - Multi-keyword: `tag("an opponent ") + verb dispatch` — prefix dispatch with verb matching
/// - ETB tracking: `preceded()` with battlefield entry phrases
fn parse_event_condition(text: &str) -> Option<ParsedCondition> {
    // "this spell is the first spell you've cast this game" — scan for keyword co-occurrence.
    // The subject varies ("this spell is", "this is") so scan for "first spell" + suffix check.
    if scan_contains_tag(text, "first spell") && text.ends_with("cast this game") {
        return Some(ParsedCondition::FirstSpellThisGame);
    }

    // "an opponent [verb phrase]" — prefix dispatch
    if let Ok((verb_phrase, _)) = tag::<_, _, OracleError<'_>>("an opponent ").parse(text) {
        if let Some(condition) = parse_opponent_had_entered_this_turn(verb_phrase) {
            return Some(condition);
        }
        if let Ok((_, condition)) = parse_opponent_event(verb_phrase) {
            return Some(condition);
        }
        // "an opponent has N or more poison counters"
        if let Some(count) =
            parse_numeric_threshold(text, "an opponent has ", " or more poison counters")
        {
            return Some(ParsedCondition::OpponentPoisonAtLeast {
                count: count as u32,
            });
        }
    }

    // "you've been attacked this step"
    if let Ok((_, _)) = alt((
        terminated(
            tag::<_, _, OracleError<'_>>("you've been attacked"),
            tag(" this step"),
        ),
        terminated(tag("been attacked"), tag(" this step")),
    ))
    .parse(text)
    {
        return Some(ParsedCondition::BeenAttackedThisStep);
    }

    // "you [action] this turn" — exact structural matches using terminated()
    if let Ok((_, condition)) = parse_you_event_this_turn(text) {
        return Some(condition);
    }

    if let Ok((_, filter)) = parse_you_cast_spell_this_turn(text) {
        return Some(ParsedCondition::YouCastSpellThisTurn {
            filter: Some(filter),
        });
    }

    // "you/you've cast a noncreature spell this turn"
    if let Ok((_, _)) = alt((
        value(
            (),
            terminated(
                tag::<_, _, OracleError<'_>>("you cast a noncreature spell"),
                tag(" this turn"),
            ),
        ),
        value(
            (),
            terminated(tag("you've cast a noncreature spell"), tag(" this turn")),
        ),
    ))
    .parse(text)
    {
        return Some(ParsedCondition::YouCastSpellThisTurn {
            filter: Some(TargetFilter::Typed(
                TypedFilter::default().with_type(TypeFilter::Non(Box::new(TypeFilter::Creature))),
            )),
        });
    }

    // "you've cast another spell this turn" — requires at least 1 other spell cast
    if alt((
        value(
            (),
            terminated(
                tag::<_, _, OracleError<'_>>("you've cast another spell"),
                tag(" this turn"),
            ),
        ),
        value(
            (),
            terminated(tag("you cast another spell"), tag(" this turn")),
        ),
    ))
    .parse(text)
    .is_ok()
    {
        return Some(ParsedCondition::YouCastSpellCountAtLeast { count: 1 });
    }

    // "you/you've discarded a card this turn"
    if alt((
        value(
            (),
            terminated(
                tag::<_, _, OracleError<'_>>("you discarded a card"),
                tag(" this turn"),
            ),
        ),
        value(
            (),
            terminated(tag("you've discarded a card"), tag(" this turn")),
        ),
    ))
    .parse(text)
    .is_ok()
    {
        return Some(ParsedCondition::YouDiscardedCardThisTurn);
    }

    // "you/you've sacrificed an artifact this turn"
    if alt((
        value(
            (),
            terminated(
                tag::<_, _, OracleError<'_>>("you sacrificed an artifact"),
                tag(" this turn"),
            ),
        ),
        value(
            (),
            terminated(tag("you've sacrificed an artifact"), tag(" this turn")),
        ),
    ))
    .parse(text)
    .is_ok()
    {
        return Some(ParsedCondition::YouSacrificedArtifactThisTurn);
    }

    // Battlefield entry tracking: "[type] enter(ed) the battlefield under your control this turn"
    if let Ok((_, condition)) = parse_etb_this_turn_condition(text) {
        return Some(condition);
    }

    None
}

fn parse_you_cast_spell_this_turn(text: &str) -> nom::IResult<&str, TargetFilter, OracleError<'_>> {
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("you've cast another "),
        tag("you cast another "),
        tag::<_, _, OracleError<'_>>("you've cast an "),
        tag("you cast an "),
        tag("you've cast a "),
        tag("you cast a "),
    ))
    .parse(text)?;
    let (rest, type_text) = take_until(" spell this turn").parse(rest)?;
    let Some(filter) = nom_condition::parse_spell_history_filter(type_text) else {
        return Err(nom::Err::Error(nom::error::Error::new(
            text,
            nom::error::ErrorKind::Fail,
        )));
    };
    let (rest, _) = tag(" spell this turn").parse(rest)?;
    Ok((rest, filter))
}

fn parse_opponent_had_entered_this_turn(verb_phrase: &str) -> Option<ParsedCondition> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("had ")
        .parse(verb_phrase)
        .ok()?;
    parse_had_entered_this_turn(rest, ControllerRef::Opponent)
}

fn parse_had_entered_this_turn(text: &str, controller: ControllerRef) -> Option<ParsedCondition> {
    let suffix = "enter the battlefield under their control this turn";
    let (count, type_and_suffix) =
        if let Some((count, after_count)) = super::oracle_util::parse_number(text) {
            if let Ok((after_or_more, _)) =
                tag::<_, _, OracleError<'_>>("or more ").parse(after_count.trim_start())
            {
                (count, after_or_more)
            } else {
                (1, text)
            }
        } else {
            (1, text)
        };
    let (rest, type_text) = take_until::<_, _, OracleError<'_>>(suffix)
        .parse(type_and_suffix)
        .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(suffix).parse(rest).ok()?;
    if !rest.is_empty() {
        return None;
    }
    let (mut filter, _) = parse_type_phrase(type_text.trim());
    if let TargetFilter::Typed(typed) = &mut filter {
        typed.controller = Some(controller);
        typed.properties.push(FilterProp::InZone {
            zone: Zone::Battlefield,
        });
    }
    Some(ParsedCondition::BattlefieldEntriesThisTurn { filter, count })
}

/// "an opponent [verb phrase]" → typed condition
fn parse_opponent_event(verb_phrase: &str) -> nom::IResult<&str, ParsedCondition, OracleError<'_>> {
    alt((
        value(
            ParsedCondition::PlayerCountAtLeast {
                filter: PlayerFilter::OpponentLostLife,
                minimum: 1,
            },
            tag("lost life this turn"),
        ),
        value(
            ParsedCondition::PlayerCountAtLeast {
                filter: PlayerFilter::OpponentGainedLife,
                minimum: 1,
            },
            tag("gained life this turn"),
        ),
        value(
            ParsedCondition::OpponentSearchedLibraryThisTurn,
            alt((
                tag("searched their library this turn"),
                tag("searched a library this turn"),
                tag("has searched their library this turn"),
            )),
        ),
    ))
    .parse(verb_phrase)
}

/// "you [action] this turn" — exact structural matching with terminated()
fn parse_you_event_this_turn(text: &str) -> nom::IResult<&str, ParsedCondition, OracleError<'_>> {
    alt((
        value(
            ParsedCondition::YouAttackedThisTurn,
            terminated(tag("you attacked"), tag(" this turn")),
        ),
        value(
            ParsedCondition::YouGainedLifeThisTurn,
            terminated(tag("you gained life"), tag(" this turn")),
        ),
        value(
            ParsedCondition::YouCreatedTokenThisTurn,
            terminated(tag("you created a token"), tag(" this turn")),
        ),
        value(
            ParsedCondition::CreatureDiedThisTurn,
            terminated(tag("a creature died"), tag(" this turn")),
        ),
    ))
    .parse(text)
}

/// CR 603.6a: modern enters templating is written "When [this object] enters"
/// (the canonical form elides "the battlefield"), so "[type] entered under your
/// control this turn" is equivalent to the full form "[type] entered the
/// battlefield under your control this turn". Matches the optional
/// " the battlefield" then the mandatory control/this-turn suffix.
fn entered_under_your_control_suffix(text: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    value(
        (),
        (
            opt(tag(" the battlefield")),
            tag(" under your control this turn"),
        ),
    )
    .parse(text)
}

/// "[type] enter(ed) [the battlefield] under your control this turn"
fn parse_etb_this_turn_condition(
    text: &str,
) -> nom::IResult<&str, ParsedCondition, OracleError<'_>> {
    alt((
        value(
            ParsedCondition::YouHadCreatureEnterThisTurn,
            (
                alt((tag("a creature entered"), tag("creature enter"))),
                entered_under_your_control_suffix,
            ),
        ),
        value(
            ParsedCondition::YouHadAngelOrBerserkerEnterThisTurn,
            (
                tag("angel or berserker enter"),
                entered_under_your_control_suffix,
            ),
        ),
        value(
            ParsedCondition::YouHadArtifactEnterThisTurn,
            (
                alt((tag("an artifact entered"), tag("artifact entered"))),
                entered_under_your_control_suffix,
            ),
        ),
    ))
    .parse(text)
}

/// Delegates to the shared word-boundary scanning primitive in `oracle_nom::primitives`.
fn scan_contains_tag(text: &str, phrase: &str) -> bool {
    super::oracle_nom::primitives::scan_contains(text, phrase)
}

/// Scan source condition text for state keywords at word boundaries using nom.
/// Matches "[subject] is attacking", "[subject] is blocked", "[subject] suspended", etc.
fn scan_source_state(text: &str) -> nom::IResult<&str, ParsedCondition, OracleError<'_>> {
    // scan_at_word_boundaries returns Option<ParsedCondition> — wrap into IResult
    match super::oracle_nom::primitives::scan_at_word_boundaries(text, parse_source_state_keyword) {
        Some(condition) => Ok(("", condition)),
        None => Err(nom::Err::Error(nom::error::Error::new(
            text,
            nom::error::ErrorKind::Fail,
        ))),
    }
}

/// Nom combinator: match source state keywords at the current position.
fn parse_source_state_keyword(input: &str) -> nom::IResult<&str, ParsedCondition, OracleError<'_>> {
    alt((
        value(
            ParsedCondition::SourceIsAttackingOrBlocking,
            tag("attacking or blocking"),
        ),
        value(ParsedCondition::SourceIsAttacking, tag("is attacking")),
        value(ParsedCondition::SourceIsBlocked, tag("is blocked")),
        value(ParsedCondition::SourceIsCreature, tag("is a creature")),
        value(
            ParsedCondition::SourceEnteredThisTurn,
            tag("entered this turn"),
        ),
        value(
            ParsedCondition::SourceInZone { zone: Zone::Exile },
            tag("suspended"),
        ),
    ))
    .parse(input)
}

// ---------------------------------------------------------------------------
// Helpers (moved from restrictions.rs)
// ---------------------------------------------------------------------------

fn parse_numeric_threshold(text: &str, prefix: &str, suffix: &str) -> Option<usize> {
    let middle = text.strip_prefix(prefix)?.strip_suffix(suffix)?.trim();
    parse_count_word(middle)
}

/// Parse a count word using nom combinator for digit/English number matching.
fn parse_count_word(text: &str) -> Option<usize> {
    let trimmed = text.trim();
    if trimmed == "zero" {
        return Some(0);
    }
    // Delegate to nom combinator for number parsing (handles digits and English words).
    let lower = trimmed.to_lowercase();
    nom_primitives::parse_number
        .parse(&lower)
        .ok()
        .and_then(|(rest, n)| rest.is_empty().then_some(n as usize))
}

fn parse_core_type_word(text: &str) -> Option<CoreType> {
    CoreType::from_str(&capitalize_condition_word(
        text.trim().trim_end_matches('s'),
    ))
    .ok()
}

fn parse_color_word(text: &str) -> Option<ManaColor> {
    ManaColor::from_str(&capitalize_condition_word(
        text.trim().trim_end_matches('s'),
    ))
    .ok()
}

fn parse_creature_pt_condition(text: &str) -> Option<(i32, i32)> {
    let stats = tag::<_, _, OracleError<'_>>("you control a ")
        .parse(text)
        .ok()
        .and_then(|(rest, _)| rest.strip_suffix(" creature"))?;
    let (power, toughness) = stats.split_once('/')?;
    Some((power.parse().ok()?, toughness.parse().ok()?))
}

fn parse_counter_requirement(text: &str) -> Option<(CounterType, u32)> {
    if let Some(counter_name) = alt((
        tag::<_, _, OracleError<'_>>("~ has "),
        tag("this artifact has "),
        tag("this enchantment has "),
    ))
    .parse(text)
    .ok()
    .and_then(|(rest, _)| rest.strip_suffix(" counters on it"))
    {
        let (count_text, counter_name) = counter_name.split_once(" or more ")?;
        return Some((
            parse_counter_type(counter_name),
            parse_count_word(count_text)? as u32,
        ));
    }
    // "there are <N or more> <type> counters on <self-ref>" where self-ref is
    // the canonical normalized "~" token (the upstream parser rewrites
    // self-noun phrases like "this artifact" to "~" before reaching here) or
    // the un-normalized "this artifact" / "this enchantment" form.
    if let Some(counter_name) = tag::<_, _, OracleError<'_>>("there are ")
        .parse(text)
        .ok()
        .and_then(|(rest, _)| {
            rest.strip_suffix(" counters on ~") // allow-noncombinator: structural suffix on tokenized condition text
                .or_else(|| rest.strip_suffix(" counters on this artifact")) // allow-noncombinator: structural suffix on tokenized condition text
                .or_else(|| rest.strip_suffix(" counters on this enchantment")) // allow-noncombinator: structural suffix on tokenized condition text
        })
    {
        let (count_text, counter_name) = counter_name.split_once(" or more ")?;
        return Some((
            parse_counter_type(counter_name),
            parse_count_word(count_text)? as u32,
        ));
    }
    None
}

fn parse_counter_absence_requirement(text: &str) -> Option<CounterType> {
    tag::<_, _, OracleError<'_>>("there are no ")
        .parse(text)
        .ok()
        .and_then(|(rest, _)| {
            rest.strip_suffix(" counters on ~") // allow-noncombinator: structural suffix on tokenized condition text
                .or_else(|| rest.strip_suffix(" counters on this artifact")) // allow-noncombinator: structural suffix on tokenized condition text
                .or_else(|| rest.strip_suffix(" counters on this enchantment")) // allow-noncombinator: structural suffix on tokenized condition text
        })
        .map(parse_counter_type)
}

fn parse_you_control_land_subtypes(text: &str) -> Option<Vec<String>> {
    let rest = alt((
        tag::<_, _, OracleError<'_>>("you control an "),
        tag("you control a "),
    ))
    .parse(text)
    .ok()
    .map(|(rest, _)| rest)?;
    if !rest.contains(" or ") {
        return None;
    }
    let subtypes = rest
        .split(" or ")
        .map(|piece| {
            piece
                .trim()
                .trim_start_matches("a ")
                .trim_start_matches("an ")
                .to_string()
        })
        .collect::<Vec<_>>();
    if subtypes.len() < 2 {
        return None;
    }
    if !subtypes.iter().all(|subtype| {
        matches!(
            subtype.as_str(),
            "plains" | "island" | "swamp" | "mountain" | "forest" | "desert"
        )
    }) {
        return None;
    }
    Some(subtypes)
}

fn parse_you_control_subtype_count(text: &str) -> Option<(usize, String)> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("you control ")
        .parse(text)
        .ok()?;
    let (minimum_text, subtype_text) = rest.split_once(" or more ")?;
    let minimum = parse_count_word(minimum_text)?;

    let normalized = subtype_text.trim();
    if parse_core_type_word(normalized).is_some()
        || normalized.ends_with(" permanents")
        || normalized == "snow permanents"
    {
        return None;
    }

    let subtype = normalized.trim_end_matches('s').trim().to_string();
    Some((minimum, subtype))
}

/// CR 205.4a + CR 205.4g: Decompose a `<supertype> <type>` phrase (snow land,
/// basic land, legendary creature, …) into a `HasSupertype` object-count
/// presence condition — count the matching permanents you control and compare
/// `>= count`. Shared by the singular "you control a snow land" arm and the
/// plural "you control N or more basic lands" arm so a supertype adjective never
/// falls through to the stringly-typed subtype dump (which yields a non-existent
/// subtype like "basic land", leaving the restriction permanently unsatisfiable).
/// Gated on `parse_supertype_prefix` so genuine bare-subtype counts
/// ("two or more vampires") are untouched and still reach the subtype path.
fn parse_you_control_supertype_count(type_phrase: &str, count: usize) -> Option<ParsedCondition> {
    nom_target::parse_supertype_prefix(type_phrase).ok()?;
    let (filter, remainder) = parse_type_phrase(type_phrase);
    if !remainder.trim().is_empty() {
        return None;
    }
    let TargetFilter::Typed(typed) = filter else {
        return None;
    };
    Some(ParsedCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(typed.controller(ControllerRef::You)),
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed {
            value: count as i32,
        },
    })
}

/// CR 601.3d + CR 608.2c: Parse `"it targets a <type_phrase>"` (or `"it targets <type_phrase>"`)
/// into a `ParsedCondition::SpellTargetsFilter` whose filter is derived from
/// `parse_type_phrase`. The pronoun `it` refers to the spell being cast — this
/// condition gates target-dependent casting permissions ("you may cast this spell
/// as though it had flash if it targets a commander" — Timely Ward). The trailing
/// remainder returned by `parse_type_phrase` must be empty for the parse to
/// succeed; otherwise we'd silently truncate qualifying clauses that the filter
/// layer hasn't absorbed.
pub(crate) fn parse_spell_targets_filter(text: &str) -> Option<ParsedCondition> {
    let rest = alt((
        tag::<_, _, OracleError<'_>>("it targets a "),
        tag("it targets an "),
        tag("it targets "),
    ))
    .parse(text)
    .ok()?
    .0;
    // CR 903.3: Bare "commander" / "commanders" without a possessive or
    // controller suffix is not lifted by `parse_type_phrase` (which expects
    // type words) or by the possessive arms of `parse_target` (which require
    // "your" / "their" / a trailing controller-suffix). Recognize it here
    // explicitly so "it targets a commander" maps to the `IsCommander`
    // FilterProp without forcing a controller scope. Timely Ward, Skullbriar's
    // sponsors, etc., all reach this arm.
    if let Ok((after, _)) =
        alt((tag::<_, _, OracleError<'_>>("commanders"), tag("commander"))).parse(rest)
    {
        if after.trim().is_empty() {
            return Some(ParsedCondition::SpellTargetsFilter {
                filter: TargetFilter::Typed(TypedFilter {
                    properties: vec![FilterProp::IsCommander],
                    ..Default::default()
                }),
            });
        }
    }
    // CR 115.1: "it targets a permanent or player" — proliferate-style pool
    // (Shiko and Narset, Unified Flurry gate). Matched before `parse_type_phrase`
    // so the "or player" half is not dropped.
    if rest.trim() == "permanent or player" {
        return Some(ParsedCondition::SpellTargetsFilter {
            filter: TargetFilter::Or {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::permanent()),
                    TargetFilter::Player,
                ],
            },
        });
    }
    // CR 115.9b: "one or more" is redundant with .any() semantics (Orvar — "if it
    // targets one or more other permanents you control").
    let (rest, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("one or more "),
        tag("one or more"),
    )))
    .parse(rest)
    .ok()?;
    let (filter, remainder) = parse_type_phrase(rest);
    if !remainder.trim().is_empty() {
        return None;
    }
    // `parse_type_phrase` falls back to `TargetFilter::Any` when no type word
    // matched. A bare "it targets a frob" must not silently widen the gate to
    // "any target"; refuse the parse instead so the casting permission is not
    // emitted (strictly safe — the spell stays sorcery-speed until the
    // predicate is recognized).
    if matches!(filter, TargetFilter::Any | TargetFilter::None) {
        return None;
    }
    Some(ParsedCondition::SpellTargetsFilter { filter })
}

fn capitalize_condition_word(text: &str) -> String {
    let mut out = String::new();
    for (index, piece) in text.split_whitespace().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let mut chars = piece.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.extend(chars);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        ControllerRef, CountScope, FilterProp, QuantityExpr, TargetFilter, TypeFilter,
    };
    use crate::types::card_type::Supertype;

    /// CR 508.1a: "you attacked with a/an <filter>[ this turn]" parses to a
    /// filtered `YouAttackedWithAtLeast { count: 1 }`, both with and without the
    /// trailing "this turn" (the latter is the shape reaching the parser after an
    /// upstream duration strip). The unfiltered numeric thresholds remain
    /// `filter: None`, and the bare-creature numeric form must NOT be captured by
    /// the typed arm.
    #[test]
    fn attacked_with_filter_condition_parses_typed_and_preserves_numeric() {
        for text in [
            "you attacked with a spacecraft this turn",
            "you attacked with a spacecraft",
        ] {
            match parse_restriction_condition(text) {
                Some(ParsedCondition::YouAttackedWithAtLeast {
                    count: 1,
                    filter: Some(TargetFilter::Typed(tf)),
                }) => assert!(
                    tf.type_filters
                        .iter()
                        .any(|f| matches!(f, TypeFilter::Subtype(s) if s == "Spacecraft")),
                    "expected Spacecraft subtype, got {:?} for {text}",
                    tf.type_filters
                ),
                other => panic!("expected filtered attacked-with for {text}, got {other:?}"),
            }
        }
        // Numeric thresholds stay unfiltered.
        assert_eq!(
            parse_restriction_condition("you attacked with 3 or more creatures this turn"),
            Some(ParsedCondition::YouAttackedWithAtLeast {
                count: 3,
                filter: None,
            }),
            "numeric attacker threshold must stay filter: None"
        );
    }

    /// CR 207.2c: The "creatures with different powers" (Coven) and "lands with the
    /// same name" qualifier phrases have dedicated typed condition variants. The
    /// generic bare-subtype count catch-all must not shadow them by swallowing the
    /// whole phrase into a stringly-typed subtype. This is a fail-on-revert guard:
    /// reordering the generic arm back above the specific arms makes these produce
    /// `YouControlSubtypeCountAtLeast { subtype: "creatures with different power"/… }`.
    #[test]
    fn qualifier_phrase_conditions_beat_generic_subtype_count() {
        // Coven activation/cast restriction (Dawnhart Mentor, Ambitious Farmhand,
        // Candletrap, Sungold Sentinel).
        assert_eq!(
            parse_restriction_condition(
                "you control three or more creatures with different powers"
            ),
            Some(ParsedCondition::YouControlDifferentPowerCreatureCountAtLeast { count: 3 }),
            "Coven 'different powers' must map to the dedicated variant, not a subtype dump"
        );
        // Endless Atlas / Sceptre of Eternal Glory.
        assert_eq!(
            parse_restriction_condition("you control three or more lands with the same name"),
            Some(ParsedCondition::YouControlLandsWithSameNameAtLeast { count: 3 }),
            "'lands with the same name' must map to the dedicated variant"
        );
        // Regression guard: a genuine bare subtype still flows to the generic arm.
        assert_eq!(
            parse_restriction_condition("you control five or more vampires"),
            Some(ParsedCondition::YouControlSubtypeCountAtLeast {
                subtype: "vampire".to_string(),
                count: 5,
            }),
            "bare subtype threshold must still reach the generic subtype-count arm"
        );
        // Regression guard: snow permanents keep their own variant.
        assert_eq!(
            parse_restriction_condition("you control two or more snow permanents"),
            Some(ParsedCondition::YouControlSnowPermanentCountAtLeast { count: 2 }),
            "snow-permanent threshold must keep its dedicated variant"
        );
    }

    /// CR 508.1 + CR 601.3: a presence-style restriction condition ("Cast this
    /// spell only if a creature is attacking you" — Confront the Assault)
    /// bridges StaticCondition::IsPresent into ParsedCondition::QuantityComparison
    /// over an ObjectCount of the same filter.
    #[test]
    fn restriction_presence_condition_bridges_to_object_count() {
        match parse_restriction_condition("a creature is attacking you") {
            Some(ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }) => assert!(
                matches!(&filter, TargetFilter::Typed(tf) if tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Attacking { defender: Some(ControllerRef::You) }
                ))),
                "filter should be a creature attacking you, got {filter:?}"
            ),
            other => panic!("expected QuantityComparison(ObjectCount >= 1), got {other:?}"),
        }
    }

    /// CR 508.1 + CR 118.9: Lethargy Trap — "three or more creatures are attacking"
    /// bridges to ObjectCount(creature + Attacking) >= N.
    #[test]
    fn restriction_attacking_creatures_count_ge() {
        match parse_restriction_condition("three or more creatures are attacking") {
            Some(ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            }) => assert!(
                matches!(&filter, TargetFilter::Typed(tf) if tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Attacking { defender: None }
                ))),
                "filter should be attacking creatures, got {filter:?}"
            ),
            other => panic!("expected QuantityComparison(ObjectCount >= 3), got {other:?}"),
        }
    }

    /// CR 205.4a + CR 205.4g: A supertype adjective in a "you control a
    /// <supertype> <type>" restriction (Blizzard — "Cast this spell only if you
    /// control a snow land"; Goblin Ski Patrol — "only if you control a snow
    /// Mountain") must decompose into a `HasSupertype` filter property plus the
    /// core type / subtype and count via `ObjectCount >= 1`. It must NOT dump
    /// "snow land" / "snow mountain" into a stringly-typed
    /// `YouControlSubtypeCountAtLeast` — no permanent has such a subtype, so that
    /// parse leaves the restriction permanently unsatisfiable.
    #[test]
    fn you_control_supertype_permanent_decomposes_to_filter() {
        use crate::types::card_type::Supertype;

        fn assert_supertype_filter(
            text: &str,
            expect_type: TypeFilter,
            expect_supertype: Supertype,
            expect_count: i32,
        ) {
            match parse_restriction_condition(text) {
                Some(ParsedCondition::QuantityComparison {
                    lhs:
                        QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount { filter },
                        },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value },
                }) => {
                    assert_eq!(value, expect_count, "{text}: count must be {expect_count}");
                    match filter {
                        TargetFilter::Typed(tf) => {
                            assert_eq!(
                                tf.controller,
                                Some(ControllerRef::You),
                                "{text}: must be scoped to permanents you control"
                            );
                            assert!(
                                tf.properties.iter().any(|p| matches!(
                                    p,
                                    FilterProp::HasSupertype { value } if *value == expect_supertype
                                )),
                                "{text}: must carry HasSupertype({expect_supertype:?}), got {:?}",
                                tf.properties
                            );
                            assert!(
                                tf.type_filters.contains(&expect_type),
                                "{text}: type_filters {:?} must contain {expect_type:?}",
                                tf.type_filters
                            );
                        }
                        other => panic!("{text}: expected a Typed filter, got {other:?}"),
                    }
                }
                other => {
                    panic!("{text}: expected QuantityComparison(ObjectCount >= N), got {other:?}")
                }
            }
        }

        // Singular "you control a <supertype> <type>" (count 1):
        // Blizzard — supertype Snow + core type Land.
        assert_supertype_filter(
            "you control a snow land",
            TypeFilter::Land,
            Supertype::Snow,
            1,
        );
        // Goblin Ski Patrol — supertype Snow + subtype Mountain (basic land type).
        assert_supertype_filter(
            "you control a snow mountain",
            TypeFilter::Subtype("Mountain".to_string()),
            Supertype::Snow,
            1,
        );

        // Plural/count "you control N or more <supertype> <type>s" (Matt's sibling
        // gap): must decompose the same way with the parsed count, NOT dump
        // "basic land" / "legendary creature" into a stringly subtype.
        assert_supertype_filter(
            "you control two or more basic lands",
            TypeFilter::Land,
            Supertype::Basic,
            2,
        );
        assert_supertype_filter(
            "you control two or more legendary creatures",
            TypeFilter::Creature,
            Supertype::Legendary,
            2,
        );

        // Regression guard: an ordinary bare subtype count keeps the subtype path
        // (no supertype prefix → must NOT become an ObjectCount comparison).
        assert_eq!(
            parse_restriction_condition("you control two or more vampires"),
            Some(ParsedCondition::YouControlSubtypeCountAtLeast {
                subtype: "vampire".to_string(),
                count: 2,
            }),
            "bare subtype count must stay on the subtype path"
        );
    }

    #[test]
    fn parses_source_conditions() {
        assert_eq!(
            parse_restriction_condition("~ is attacking"),
            Some(ParsedCondition::SourceIsAttacking),
        );
        assert_eq!(
            parse_restriction_condition("this creature is attacking"),
            Some(ParsedCondition::SourceIsAttacking),
        );
        assert_eq!(
            parse_restriction_condition("~'s power is 4 or greater"),
            Some(ParsedCondition::SourcePowerAtLeast { minimum: 4 }),
        );
        assert_eq!(
            parse_restriction_condition("this card is in your graveyard"),
            Some(ParsedCondition::SourceInZone {
                zone: Zone::Graveyard
            }),
        );
        assert_eq!(
            parse_restriction_condition("~ is on the stack"),
            Some(ParsedCondition::SourceInZone { zone: Zone::Stack }),
        );
        assert_eq!(
            parse_restriction_condition("From your graveyard"),
            Some(ParsedCondition::SourceInZone {
                zone: Zone::Graveyard
            }),
        );
        assert_eq!(
            parse_restriction_condition("in your graveyard"),
            Some(ParsedCondition::SourceInZone {
                zone: Zone::Graveyard
            }),
        );
    }

    #[test]
    fn parses_counter_threshold_conditions() {
        // Both the canonical ~ form (post-self-noun-normalization) and the
        // un-normalized "this artifact" form must parse to the same shape.
        // Production input arrives as ~ after the upstream rewrite.
        for input in [
            "there are three or more brick counters on ~",
            "there are three or more brick counters on this artifact",
        ] {
            let result = parse_restriction_condition(input);
            assert!(
                matches!(
                    result,
                    Some(ParsedCondition::SourceHasCounterAtLeast { count: 3, .. })
                ),
                "input={input:?}, got: {result:?}",
            );
        }
        for input in [
            "there are no charge counters on ~",
            "there are no charge counters on this artifact",
        ] {
            let result = parse_restriction_condition(input);
            assert!(
                matches!(result, Some(ParsedCondition::SourceHasNoCounter { .. })),
                "input={input:?}, got: {result:?}",
            );
        }
    }

    #[test]
    fn parses_you_control_conditions() {
        assert!(matches!(
            parse_restriction_condition("you control two or more vampires"),
            Some(ParsedCondition::YouControlSubtypeCountAtLeast { count: 2, .. })
        ));
        assert!(matches!(
            parse_restriction_condition("you control a legendary creature"),
            Some(ParsedCondition::YouControlLegendaryCreature)
        ));
    }

    #[test]
    fn parses_controls_creature_with_keyword_both_scopes() {
        // CR 602.5b: Groundling Pouncer — "an opponent controls a creature with flying".
        assert_eq!(
            parse_restriction_condition("an opponent controls a creature with flying"),
            Some(ParsedCondition::ControlsCreatureWithKeyword {
                controller: ControllerRef::Opponent,
                keyword: Keyword::Flying,
            }),
        );
        // Building-block proof: the same condition with controller = You still parses.
        assert_eq!(
            parse_restriction_condition("you control a creature with flying"),
            Some(ParsedCondition::ControlsCreatureWithKeyword {
                controller: ControllerRef::You,
                keyword: Keyword::Flying,
            }),
        );
        // "that has" phrasing flows through the same combinator.
        assert_eq!(
            parse_restriction_condition("an opponent controls a creature that has flying"),
            Some(ParsedCondition::ControlsCreatureWithKeyword {
                controller: ControllerRef::Opponent,
                keyword: Keyword::Flying,
            }),
        );
    }

    #[test]
    fn parses_land_played_this_turn_conditions() {
        for input in [
            "you've played a land this turn",
            "you have played a land this turn",
            "you played a land this turn",
        ] {
            assert_eq!(
                parse_restriction_condition(input),
                Some(ParsedCondition::YouPlayedLandThisTurn),
                "input={input:?}"
            );
        }
    }

    #[test]
    fn parses_zone_card_conditions() {
        assert!(matches!(
            parse_restriction_condition(
                "there are four or more card types among cards in your graveyard"
            ),
            Some(ParsedCondition::ZoneCardTypeCountAtLeast {
                zone: Zone::Graveyard,
                count: 4
            })
        ));
        assert!(matches!(
            parse_restriction_condition("there are seven or more cards in your graveyard"),
            Some(ParsedCondition::ZoneCardCountAtLeast {
                zone: Zone::Graveyard,
                count: 7
            })
        ));
    }

    #[test]
    fn parses_hand_conditions() {
        assert_eq!(
            parse_restriction_condition("you have exactly seven cards in hand"),
            Some(ParsedCondition::HandSizeExact { count: 7 }),
        );
        assert_eq!(
            parse_restriction_condition("you have exactly zero or seven cards in hand"),
            Some(ParsedCondition::HandSizeOneOf { counts: vec![0, 7] }),
        );
    }

    #[test]
    fn parses_quantity_vs_opponent() {
        assert!(matches!(
            parse_restriction_condition("you have more cards in hand than each opponent"),
            Some(ParsedCondition::QuantityVsEachOpponent {
                lhs: QuantityRef::HandSize {
                    player: PlayerScope::Controller
                },
                comparator: Comparator::GT,
                rhs: QuantityRef::HandSize {
                    player: PlayerScope::Controller
                },
            })
        ));
    }

    #[test]
    fn parses_event_conditions() {
        assert_eq!(
            parse_restriction_condition("you attacked this turn"),
            Some(ParsedCondition::YouAttackedThisTurn),
        );
        assert_eq!(
            parse_restriction_condition("you gained life this turn"),
            Some(ParsedCondition::YouGainedLifeThisTurn),
        );
        assert!(matches!(
            parse_restriction_condition("you've cast an instant or sorcery spell this turn"),
            Some(ParsedCondition::YouCastSpellThisTurn {
                filter: Some(TargetFilter::Or { filters })
            }) if filters.iter().any(|filter| matches!(
                filter,
                TargetFilter::Typed(TypedFilter { type_filters, .. })
                    if type_filters == &vec![TypeFilter::Instant]
            )) && filters.iter().any(|filter| matches!(
                filter,
                TargetFilter::Typed(TypedFilter { type_filters, .. })
                    if type_filters == &vec![TypeFilter::Sorcery]
            ))
        ));
        assert!(matches!(
            parse_restriction_condition("you've cast another green spell this turn"),
            Some(ParsedCondition::YouCastSpellThisTurn {
                filter: Some(TargetFilter::Typed(TypedFilter {
                    properties,
                    ..
                }))
            }) if properties == vec![FilterProp::HasColor {
                color: ManaColor::Green
            }]
        ));
        assert_eq!(
            parse_restriction_condition("a creature died this turn"),
            Some(ParsedCondition::CreatureDiedThisTurn),
        );
    }

    #[test]
    fn parses_quantity_restriction_conditions() {
        assert!(matches!(
            parse_restriction_condition(
                "you've cast three or more instant and/or sorcery spells this turn"
            ),
            Some(ParsedCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Controller,
                        filter: Some(TargetFilter::Or { .. }),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            })
        ));
        assert!(matches!(
            parse_restriction_condition(
                "a non-skeleton creature died under your control this turn"
            ),
            Some(ParsedCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ZoneChangeCountThisTurn { .. },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
        ));
    }

    #[test]
    fn parses_opponent_event_conditions() {
        // CR 602.5b: "an opponent [action] this turn" maps to PlayerCountAtLeast
        // with the matching PlayerFilter. Tests cover the full class, not a single card.
        assert_eq!(
            parse_restriction_condition("an opponent lost life this turn"),
            Some(ParsedCondition::PlayerCountAtLeast {
                filter: PlayerFilter::OpponentLostLife,
                minimum: 1,
            }),
        );
        assert_eq!(
            parse_restriction_condition("an opponent gained life this turn"),
            Some(ParsedCondition::PlayerCountAtLeast {
                filter: PlayerFilter::OpponentGainedLife,
                minimum: 1,
            }),
        );
    }

    #[test]
    fn parses_city_blessing_restriction() {
        assert_eq!(
            parse_restriction_condition("you have the city's blessing"),
            Some(ParsedCondition::HasCityBlessing),
        );
    }

    #[test]
    fn parses_you_control_core_type_count() {
        assert!(matches!(
            parse_restriction_condition("you control three or more artifacts"),
            Some(ParsedCondition::YouControlCoreTypeCountAtLeast {
                core_type: CoreType::Artifact,
                count: 3,
            })
        ));
        assert!(matches!(
            parse_restriction_condition("you control two or more enchantments"),
            Some(ParsedCondition::YouControlCoreTypeCountAtLeast {
                core_type: CoreType::Enchantment,
                count: 2,
            })
        ));
    }

    #[test]
    fn parses_you_control_color_permanent_count() {
        assert!(matches!(
            parse_restriction_condition("you control two or more white permanents"),
            Some(ParsedCondition::YouControlColorPermanentCountAtLeast {
                color: ManaColor::White,
                count: 2,
            })
        ));
    }

    #[test]
    fn parses_compound_and() {
        // Two atomic conditions joined by "and" form a ParsedCondition::And.
        let parsed =
            parse_restriction_condition("you attacked this turn and you gained life this turn");
        assert!(matches!(
            parsed,
            Some(ParsedCondition::And { ref conditions })
                if conditions.len() == 2
                    && matches!(conditions[0], ParsedCondition::YouAttackedThisTurn)
                    && matches!(conditions[1], ParsedCondition::YouGainedLifeThisTurn)
        ));
    }

    #[test]
    fn parses_compound_or() {
        let parsed =
            parse_restriction_condition("you attacked this turn or you gained life this turn");
        assert!(matches!(
            parsed,
            Some(ParsedCondition::Or { ref conditions })
                if conditions.len() == 2
                    && matches!(conditions[0], ParsedCondition::YouAttackedThisTurn)
                    && matches!(conditions[1], ParsedCondition::YouGainedLifeThisTurn)
        ));
    }

    #[test]
    fn parses_compound_or_if_restriction_fragments() {
        let parsed =
            parse_restriction_condition("~ entered this turn or if you control a basic land");
        let Some(ParsedCondition::Or { conditions }) = parsed else {
            panic!("expected compound Or, got {parsed:?}");
        };
        assert_eq!(conditions.len(), 2);
        assert!(matches!(
            conditions[0],
            ParsedCondition::SourceEnteredThisTurn
        ));
        match &conditions[1] {
            ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ObjectCount {
                                filter: TargetFilter::Typed(tf),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert!(tf
                    .properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::HasSupertype { value } if *value == Supertype::Basic)));
            }
            other => panic!("expected ObjectCount basic land condition, got {other:?}"),
        }
    }

    #[test]
    fn parses_compound_source_zone_shorthand() {
        let parsed = parse_restriction_condition("~ is on the battlefield or in your graveyard");
        assert!(matches!(
            parsed,
            Some(ParsedCondition::Or { ref conditions })
                if conditions.len() == 2
                    && matches!(conditions[0], ParsedCondition::SourceInZone { zone: Zone::Battlefield })
                    && matches!(conditions[1], ParsedCondition::SourceInZone { zone: Zone::Graveyard })
        ));
    }

    #[test]
    fn parses_compound_not() {
        let parsed = parse_restriction_condition("not you attacked this turn");
        assert!(matches!(
            parsed,
            Some(ParsedCondition::Not { ref condition })
                if matches!(**condition, ParsedCondition::YouAttackedThisTurn)
        ));
    }

    #[test]
    fn compound_falls_back_when_fragment_unparseable() {
        // " or " inside an atomic phrase must not tear it apart — when a fragment
        // fails, the compound parse returns None and the caller tries atomic parsing.
        let parsed = parse_restriction_condition("you have more cards in hand than each opponent");
        // Atomic parse succeeds (QuantityVsEachOpponent); compound must not interfere.
        assert!(matches!(
            parsed,
            Some(ParsedCondition::QuantityVsEachOpponent { .. })
        ));
    }

    #[test]
    fn parses_opponent_controls_more_than_you_activation_restrictions() {
        use crate::types::ability::{
            Comparator, PlayerFilter, PlayerRelation, QuantityExpr, QuantityRef, TypeFilter,
        };

        // Issue #859 / #2908: Weathered Wayfarer — existential "more lands than you".
        let parsed = parse_restriction_condition("an opponent controls more lands than you")
            .expect("Weathered Wayfarer activation restriction must parse");
        match parsed {
            ParsedCondition::QuantityComparison {
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
            other => panic!("expected existential opponent ControlsCount GT, got {other:?}"),
        }

        // Issue #2908: Isolated Watchtower — "at least two more lands than you".
        let parsed =
            parse_restriction_condition("an opponent controls at least two more lands than you")
                .expect("Isolated Watchtower activation restriction must parse");
        match parsed {
            ParsedCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::PlayerCount {
                                filter:
                                    PlayerFilter::ControlsCount {
                                        relation: PlayerRelation::Opponent,
                                        comparator: Comparator::GE,
                                        count,
                                        ..
                                    },
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => match count.as_ref() {
                QuantityExpr::Offset { offset: 2, .. } => {}
                other => panic!("expected Offset(+2) count threshold, got {other:?}"),
            },
            other => {
                panic!("expected existential opponent ControlsCount GE (you+2), got {other:?}")
            }
        }

        // Building-block proof: creatures variant shares the same combinator axis.
        let parsed = parse_restriction_condition("an opponent controls more creatures than you")
            .expect("creature comparison activation restriction must parse");
        match parsed {
            ParsedCondition::QuantityComparison {
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
                ..
            } => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
            }
            other => panic!("expected creature ControlsCount GT, got {other:?}"),
        }
    }

    #[test]
    fn unrecognized_returns_none() {
        assert_eq!(
            parse_restriction_condition("something completely unknown"),
            None,
        );
    }

    #[test]
    fn it_targets_a_commander_parses_to_spell_targets_filter() {
        // CR 601.3d + CR 903.3: Timely Ward — "if it targets a commander" gates
        // the flash permission against the spell-being-cast's chosen targets.
        let parsed = parse_restriction_condition("it targets a commander")
            .expect("should parse the target-commander predicate");
        match parsed {
            ParsedCondition::SpellTargetsFilter {
                filter: TargetFilter::Typed(filter),
            } => {
                assert!(filter.properties.contains(&FilterProp::IsCommander));
                assert!(
                    filter.controller.is_none(),
                    "bare 'commander' has no controller scope, got {:?}",
                    filter.controller
                );
            }
            other => panic!("expected SpellTargetsFilter(IsCommander), got {other:?}"),
        }
    }

    #[test]
    fn it_targets_a_permanent_or_player_parses_to_spell_targets_filter() {
        let parsed = parse_restriction_condition("it targets a permanent or player")
            .expect("should parse the permanent-or-player predicate");
        match parsed {
            ParsedCondition::SpellTargetsFilter {
                filter: TargetFilter::Or { filters },
            } => {
                assert!(filters.iter().any(|f| matches!(
                    f,
                    TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Permanent)
                )));
                assert!(filters.contains(&TargetFilter::Player));
            }
            other => panic!("expected SpellTargetsFilter(Or), got {other:?}"),
        }
    }

    #[test]
    fn it_targets_a_creature_parses_to_spell_targets_filter() {
        // CR 601.3d + CR 608.2c: hypothetical "as though it had flash if it targets
        // a creature" — verifies the helper composes with `parse_type_phrase` for
        // ordinary core types, not just the commander special case.
        let parsed = parse_restriction_condition("it targets a creature")
            .expect("should parse the target-creature predicate");
        match parsed {
            ParsedCondition::SpellTargetsFilter {
                filter: TargetFilter::Typed(filter),
            } => {
                assert!(filter.type_filters.contains(&TypeFilter::Creature));
            }
            other => panic!("expected SpellTargetsFilter(Creature), got {other:?}"),
        }
    }

    #[test]
    fn it_targets_one_or_more_other_permanents_you_control() {
        let parsed =
            parse_restriction_condition("it targets one or more other permanents you control")
                .expect("Orvar intervening-if should parse");
        match parsed {
            ParsedCondition::SpellTargetsFilter {
                filter: TargetFilter::Typed(tf),
            } => {
                assert!(tf.type_filters.contains(&TypeFilter::Permanent));
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::Another));
            }
            other => panic!("expected SpellTargetsFilter(Typed permanent), got {other:?}"),
        }
    }

    #[test]
    fn it_targets_unknown_returns_none() {
        // CR 601.3d: predicate that doesn't lift to a typed filter must not be
        // emitted — fail-loud (return None) so the caller leaves the casting
        // permission off rather than fail-silent with `TargetFilter::Any`.
        assert_eq!(
            parse_restriction_condition("it targets a frob the wobble"),
            None,
        );
    }
}
