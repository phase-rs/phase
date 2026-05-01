use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::value;
use nom::sequence::terminated;
use nom::Parser;
use nom_language::error::{VerboseError, VerboseErrorKind};

use super::oracle_nom::condition as nom_condition;
use super::oracle_nom::primitives as nom_primitives;
use crate::types::ability::{
    Comparator, ControllerRef, ParsedCondition, PlayerFilter, PlayerScope, QuantityRef,
    TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::counter::{parse_counter_type, CounterType};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::zones::Zone;

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
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("not ").parse(text) {
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
        .map(parse_condition_text)
        .collect::<Option<Vec<_>>>()
        .filter(|v| v.len() >= 2)
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

    if value(
        (),
        tag::<_, _, VerboseError<&str>>("you have the city's blessing"),
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
        });
    }
    if let Some(count) =
        parse_numeric_threshold(text, "you attacked with ", " or more creatures this turn")
    {
        return Some(ParsedCondition::YouAttackedWithAtLeast {
            count: count as u32,
        });
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
    None
}

fn parse_source_condition(text: &str) -> Option<ParsedCondition> {
    // Source conditions accept self-reference and source-state subjects:
    //   "~ <state>"          — canonical normalized self-ref (e.g., "~ is attacking")
    //   "this <noun>"        — explicit self-reference ("this creature is blocked")
    //   "enchanted <noun>"   — Aura-attached source predicate
    //   "from your <zone>"   — zone-based source predicate
    if alt((
        tag::<_, _, VerboseError<&str>>("this "),
        tag("enchanted "),
        tag("from your "),
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
    // Source state: scan for state keywords after the subject using nom at word boundaries
    if let Ok((_, condition)) = scan_source_state(text) {
        return Some(condition);
    }
    // "enchanted [type] is untapped"
    if text.contains("is untapped") {
        if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("enchanted ").parse(text) {
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
        tag::<_, _, VerboseError<&str>>("this creature doesn't have "),
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
        tag::<_, _, VerboseError<&str>>("this creature is "),
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
        tag::<_, _, VerboseError<&str>>("this creature's power is "),
        tag("~'s power is "),
    ))
    .parse(text)
    .ok()?;
    let (rest, power) = nom_primitives::parse_number(rest).ok()?;
    let (rest, _) = tag::<_, _, VerboseError<&str>>(" or greater")
        .parse(rest)
        .ok()?;
    rest.trim().is_empty().then_some(power as i32)
}

fn parse_you_control_condition(text: &str) -> Option<ParsedCondition> {
    // "you control a [subtype] or there is a [subtype] card in your graveyard"
    if text.contains(" or there is a ") && text.contains(" card in your graveyard") {
        if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("you control a ").parse(text) {
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
    if let Some((count, subtype)) = parse_you_control_subtype_count(text) {
        return Some(ParsedCondition::YouControlSubtypeCountAtLeast { subtype, count });
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
    // "you control N or more [color] permanents" / "you control N or more [core type]s"
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("you control ").parse(text) {
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
    // "you control a creature with [keyword]"
    if let Ok((keyword_text, _)) = alt((
        tag::<_, _, VerboseError<&str>>("you control a creature with "),
        tag("you control a creature that has "),
    ))
    .parse(text)
    {
        let keyword: Keyword = keyword_text.trim().parse().unwrap();
        if !matches!(keyword, Keyword::Unknown(_)) {
            return Some(ParsedCondition::YouControlCreatureWithKeyword { keyword });
        }
    }
    // "you control a/another legendary creature"
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("you control ").parse(text) {
        if rest.contains("legendary creature") {
            return Some(ParsedCondition::YouControlLegendaryCreature);
        }
        if rest.contains("colorless creature") {
            return Some(ParsedCondition::YouControlAnotherColorlessCreature);
        }
    }
    // "you control fewer creatures than each opponent"
    if tag::<_, _, VerboseError<&str>>("you control fewer creatures than")
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
    if tag::<_, _, VerboseError<&str>>("you control no creatures")
        .parse(text)
        .is_ok()
    {
        return Some(ParsedCondition::YouControlNoCreatures);
    }
    if let Ok((rest, _)) = alt((
        tag::<_, _, VerboseError<&str>>("you control an "),
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
        tag::<_, _, VerboseError<&str>>("you control an "),
        tag("you control a "),
    ))
    .parse(text)
    {
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
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("there are ").parse(text) {
        if let Some((count, after_num)) = super::oracle_util::parse_number(rest) {
            let count = count as usize;
            // "there are N or more card types among cards in your <zone>"
            if let Ok((zone_text, _)) =
                tag::<_, _, VerboseError<&str>>("or more card types among cards ").parse(after_num)
            {
                if let Some((_, zone)) = extract_zone_from_suffix(zone_text) {
                    return Some(ParsedCondition::ZoneCardTypeCountAtLeast { zone, count });
                }
            }
            // "there are N or more cards in your <zone>"
            if let Ok((zone_text, _)) =
                tag::<_, _, VerboseError<&str>>("or more cards ").parse(after_num)
            {
                if let Some((_, zone)) = extract_zone_from_suffix(zone_text) {
                    return Some(ParsedCondition::ZoneCardCountAtLeast { zone, count });
                }
            }
        }
        // "there are no <subtype> cards in your <zone>"
        if let Ok((no_rest, _)) = tag::<_, _, VerboseError<&str>>("no ").parse(rest) {
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
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("there is an ").parse(text) {
        if let Some((subtype, zone)) = parse_subtype_zone_suffix(rest, " card ") {
            return Some(ParsedCondition::ZoneSubtypeCardCountAtLeast {
                zone,
                subtype: subtype.to_string(),
                count: 1,
            });
        }
    }
    // "two or more <subtype> cards are in your <zone>"
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("two or more ").parse(text) {
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
    if tag::<_, _, VerboseError<&str>>("you have no cards")
        .parse(text)
        .is_ok()
    {
        return Some(ParsedCondition::HandSizeExact { count: 0 });
    }
    if tag::<_, _, VerboseError<&str>>("you have one or fewer cards in hand")
        .parse(text)
        .is_ok()
    {
        return Some(ParsedCondition::HandSizeOneOf { counts: vec![0, 1] });
    }
    // "you have more cards in hand than each opponent"
    if tag::<_, _, VerboseError<&str>>("you have more cards in hand than")
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
    if let Some(rest) = tag::<_, _, VerboseError<&str>>("you have exactly ")
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
    if let Ok((verb_phrase, _)) = tag::<_, _, VerboseError<&str>>("an opponent ").parse(text) {
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
            tag::<_, _, VerboseError<&str>>("you've been attacked"),
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
                tag::<_, _, VerboseError<&str>>("you cast a noncreature spell"),
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
                tag::<_, _, VerboseError<&str>>("you've cast another spell"),
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
                tag::<_, _, VerboseError<&str>>("you discarded a card"),
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
                tag::<_, _, VerboseError<&str>>("you sacrificed an artifact"),
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

fn parse_you_cast_spell_this_turn(
    text: &str,
) -> nom::IResult<&str, TargetFilter, VerboseError<&str>> {
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>("you've cast another "),
        tag("you cast another "),
        tag::<_, _, VerboseError<&str>>("you've cast an "),
        tag("you cast an "),
        tag("you've cast a "),
        tag("you cast a "),
    ))
    .parse(text)?;
    let (rest, type_text) = take_until(" spell this turn").parse(rest)?;
    let Some(filter) = nom_condition::parse_spell_history_filter(type_text) else {
        return Err(nom::Err::Error(VerboseError {
            errors: vec![(text, VerboseErrorKind::Nom(nom::error::ErrorKind::Tag))],
        }));
    };
    let (rest, _) = tag(" spell this turn").parse(rest)?;
    Ok((rest, filter))
}

/// "an opponent [verb phrase]" → typed condition
fn parse_opponent_event(
    verb_phrase: &str,
) -> nom::IResult<&str, ParsedCondition, VerboseError<&str>> {
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
fn parse_you_event_this_turn(
    text: &str,
) -> nom::IResult<&str, ParsedCondition, VerboseError<&str>> {
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

/// "[type] enter(ed) the battlefield under your control this turn"
fn parse_etb_this_turn_condition(
    text: &str,
) -> nom::IResult<&str, ParsedCondition, VerboseError<&str>> {
    alt((
        value(
            ParsedCondition::YouHadCreatureEnterThisTurn,
            alt((
                tag("a creature entered the battlefield under your control this turn"),
                tag("creature enter the battlefield under your control this turn"),
            )),
        ),
        value(
            ParsedCondition::YouHadAngelOrBerserkerEnterThisTurn,
            tag("angel or berserker enter the battlefield under your control this turn"),
        ),
        value(
            ParsedCondition::YouHadArtifactEnterThisTurn,
            alt((
                tag("an artifact entered the battlefield under your control this turn"),
                tag("artifact entered the battlefield under your control this turn"),
            )),
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
fn scan_source_state(text: &str) -> nom::IResult<&str, ParsedCondition, VerboseError<&str>> {
    // scan_at_word_boundaries returns Option<ParsedCondition> — wrap into IResult
    match super::oracle_nom::primitives::scan_at_word_boundaries(text, parse_source_state_keyword) {
        Some(condition) => Ok(("", condition)),
        None => Err(nom::Err::Error(VerboseError {
            errors: vec![(text, VerboseErrorKind::Context("no source state"))],
        })),
    }
}

/// Nom combinator: match source state keywords at the current position.
fn parse_source_state_keyword(
    input: &str,
) -> nom::IResult<&str, ParsedCondition, VerboseError<&str>> {
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
    let stats = tag::<_, _, VerboseError<&str>>("you control a ")
        .parse(text)
        .ok()
        .and_then(|(rest, _)| rest.strip_suffix(" creature"))?;
    let (power, toughness) = stats.split_once('/')?;
    Some((power.parse().ok()?, toughness.parse().ok()?))
}

fn parse_counter_requirement(text: &str) -> Option<(CounterType, u32)> {
    if let Some(counter_name) = alt((
        tag::<_, _, VerboseError<&str>>("~ has "),
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
    if let Some(counter_name) = tag::<_, _, VerboseError<&str>>("there are ")
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
    tag::<_, _, VerboseError<&str>>("there are no ")
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
        tag::<_, _, VerboseError<&str>>("you control an "),
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
    let (rest, _) = tag::<_, _, VerboseError<&str>>("you control ")
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
    use crate::types::ability::FilterProp;

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
            parse_restriction_condition("From your graveyard"),
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
    fn unrecognized_returns_none() {
        assert_eq!(
            parse_restriction_condition("something completely unknown"),
            None,
        );
    }
}
