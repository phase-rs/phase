use std::str::FromStr;

use crate::game::game_object::{parse_counter_type, CounterType};
use crate::parser::oracle_util::parse_number;
use crate::types::ability::{
    Comparator, ControllerRef, ParsedCondition, QuantityRef, TargetFilter, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::zones::Zone;

/// CR 601.3 / CR 602.5: Parse a restriction condition from Oracle text into a typed
/// `ParsedCondition`. These conditions gate whether a spell can be cast or ability activated.
/// Returns `None` for unrecognized conditions (caller treats `None` as permissive true).
/// Normalizes input: lowercase, trim, strip trailing period.
pub fn parse_restriction_condition(text: &str) -> Option<ParsedCondition> {
    let lower = text.trim().trim_end_matches('.').to_lowercase();
    parse_condition_text(&lower)
}

fn parse_condition_text(text: &str) -> Option<ParsedCondition> {
    if let Some(condition) = parse_source_condition(text) {
        return Some(condition);
    }
    if let Some(condition) = parse_you_control_condition(text) {
        return Some(condition);
    }
    if let Some(condition) = parse_graveyard_condition(text) {
        return Some(condition);
    }
    if let Some(condition) = parse_hand_condition(text) {
        return Some(condition);
    }

    // Event-based conditions: decompose using prefix/keyword matching where feasible.
    if text.contains("first spell") && text.contains("cast this game") {
        return Some(ParsedCondition::FirstSpellThisGame);
    }
    if text.starts_with("an opponent") && text.contains("searched") && text.contains("library") {
        return Some(ParsedCondition::OpponentSearchedLibraryThisTurn);
    }
    if text.contains("been attacked") && text.contains("this step") {
        return Some(ParsedCondition::BeenAttackedThisStep);
    }
    if text.starts_with("an opponent") && text.contains("poison counters") {
        if let Some(count) =
            parse_numeric_threshold(text, "an opponent has ", " or more poison counters")
        {
            return Some(ParsedCondition::OpponentPoisonAtLeast {
                count: count as u32,
            });
        }
    }
    if text.starts_with("you attacked") && text.ends_with("this turn") && !text.contains("with") {
        return Some(ParsedCondition::YouAttackedThisTurn);
    }
    if text.starts_with("you gained life") && text.ends_with("this turn") {
        return Some(ParsedCondition::YouGainedLifeThisTurn);
    }
    if text.starts_with("you created a token") && text.ends_with("this turn") {
        return Some(ParsedCondition::YouCreatedTokenThisTurn);
    }
    if text.starts_with("a creature died") && text.ends_with("this turn") {
        return Some(ParsedCondition::CreatureDiedThisTurn);
    }
    if text.contains("cast a noncreature spell") && text.ends_with("this turn") {
        return Some(ParsedCondition::YouCastNoncreatureSpellThisTurn);
    }
    if text.contains("discarded a card") && text.ends_with("this turn") {
        return Some(ParsedCondition::YouDiscardedCardThisTurn);
    }
    if text.contains("sacrificed an artifact") && text.ends_with("this turn") {
        return Some(ParsedCondition::YouSacrificedArtifactThisTurn);
    }
    if text.contains("creature enter the battlefield under your control this turn") {
        return Some(ParsedCondition::YouHadCreatureEnterThisTurn);
    }
    if text.contains("angel or berserker enter the battlefield under your control this turn") {
        return Some(ParsedCondition::YouHadAngelOrBerserkerEnterThisTurn);
    }
    if text.contains("artifact entered the battlefield under your control this turn") {
        return Some(ParsedCondition::YouHadArtifactEnterThisTurn);
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
    // Source conditions require "this creature/permanent/card/land" or "enchanted" or "from your"
    // as a prefix — reject bare "~" or unrecognized subjects.
    let is_source_ref = text.starts_with("this ")
        || text.starts_with("enchanted ")
        || text.starts_with("from your ");
    if !is_source_ref {
        return None;
    }
    // Zone-based source conditions
    if text.contains("graveyard")
        && (text.starts_with("from your") || text.contains("in your graveyard"))
    {
        return Some(ParsedCondition::SourceInZone {
            zone: Zone::Graveyard,
        });
    }
    if text.contains("suspended") {
        return Some(ParsedCondition::SourceInZone { zone: Zone::Exile });
    }
    // Combat state conditions
    if text.contains("attacking or blocking") {
        return Some(ParsedCondition::SourceIsAttackingOrBlocking);
    }
    if text.contains("is attacking") {
        return Some(ParsedCondition::SourceIsAttacking);
    }
    if text.contains("is blocked") {
        return Some(ParsedCondition::SourceIsBlocked);
    }
    // Type/state checks
    if text.contains("is a creature") {
        return Some(ParsedCondition::SourceIsCreature);
    }
    if text.contains("entered this turn") {
        return Some(ParsedCondition::SourceEnteredThisTurn);
    }
    // "enchanted [type] is untapped"
    if text.contains("is untapped") {
        if let Some(rest) = text.strip_prefix("enchanted ") {
            if let Some(type_text) = rest.strip_suffix(" is untapped") {
                if let Some(core_type) = parse_core_type_word(type_text) {
                    return Some(ParsedCondition::SourceUntappedAttachedTo {
                        required_type: core_type,
                    });
                }
            }
        }
    }
    // "this creature doesn't have [keyword]"
    if let Some(keyword_text) = text.strip_prefix("this creature doesn't have ") {
        let keyword: Keyword = keyword_text.trim().parse().unwrap();
        if !matches!(keyword, Keyword::Unknown(_)) {
            return Some(ParsedCondition::SourceLacksKeyword { keyword });
        }
    }
    // "this creature is [color]"
    if let Some(color_text) = text.strip_prefix("this creature is ") {
        if let Some(color) = parse_color_word(color_text) {
            return Some(ParsedCondition::SourceIsColor { color });
        }
    }
    // Power threshold: "this creature's power is N or greater"
    if let Some(power) = parse_numeric_threshold(text, "this creature's power is ", " or greater") {
        return Some(ParsedCondition::SourcePowerAtLeast {
            minimum: power as i32,
        });
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

fn parse_you_control_condition(text: &str) -> Option<ParsedCondition> {
    // "you control a [subtype] or there is a [subtype] card in your graveyard"
    if text.contains(" or there is a ") && text.contains(" card in your graveyard") {
        if let Some(rest) = text.strip_prefix("you control a ") {
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
    if let Some(rest) = text.strip_prefix("you control ") {
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
    if let Some(keyword_text) = text
        .strip_prefix("you control a creature with ")
        .or_else(|| text.strip_prefix("you control a creature that has "))
    {
        let keyword: Keyword = keyword_text.trim().parse().unwrap();
        if !matches!(keyword, Keyword::Unknown(_)) {
            return Some(ParsedCondition::YouControlCreatureWithKeyword { keyword });
        }
    }
    // "you control a legendary creature"
    if text.starts_with("you control") && text.contains("legendary creature") {
        return Some(ParsedCondition::YouControlLegendaryCreature);
    }
    // "you control another colorless creature"
    if text.starts_with("you control") && text.contains("colorless creature") {
        return Some(ParsedCondition::YouControlAnotherColorlessCreature);
    }
    // "you control fewer creatures than each opponent"
    if text.starts_with("you control fewer creatures than") {
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
    if text.starts_with("you control no creatures") {
        return Some(ParsedCondition::YouControlNoCreatures);
    }
    if let Some(name) = text
        .strip_prefix("you control an ")
        .or_else(|| text.strip_prefix("you control a "))
        .and_then(|rest| rest.strip_suffix(" planeswalker"))
    {
        return Some(ParsedCondition::YouControlNamedPlaneswalker {
            name: capitalize_condition_word(name),
        });
    }
    if let Some(rest) = text
        .strip_prefix("you control an ")
        .or_else(|| text.strip_prefix("you control a "))
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

fn parse_graveyard_condition(text: &str) -> Option<ParsedCondition> {
    if let Some(count) =
        parse_numeric_threshold(text, "there are ", " or more cards in your graveyard")
    {
        return Some(ParsedCondition::GraveyardCardCountAtLeast { count });
    }
    if let Some(count) = parse_numeric_threshold(
        text,
        "there are ",
        " or more card types among cards in your graveyard",
    ) {
        return Some(ParsedCondition::GraveyardCardTypeCountAtLeast { count });
    }
    if let Some(subtype) = text
        .strip_prefix("there is an ")
        .and_then(|rest| rest.strip_suffix(" card in your graveyard"))
    {
        return Some(ParsedCondition::GraveyardSubtypeCardCountAtLeast {
            subtype: subtype.to_string(),
            count: 1,
        });
    }
    if let Some(subtype) = text
        .strip_prefix("two or more ")
        .and_then(|rest| rest.strip_suffix(" cards are in your graveyard"))
    {
        return Some(ParsedCondition::GraveyardSubtypeCardCountAtLeast {
            subtype: subtype.trim_end_matches('s').to_string(),
            count: 2,
        });
    }
    None
}

fn parse_hand_condition(text: &str) -> Option<ParsedCondition> {
    if !text.contains("cards in hand") && !text.contains("hand") {
        return None;
    }
    // "you have no cards in hand"
    if text.starts_with("you have no cards") {
        return Some(ParsedCondition::HandSizeExact { count: 0 });
    }
    // "you have more cards in hand than each opponent"
    if text.contains("more cards in hand than") {
        return Some(ParsedCondition::QuantityVsEachOpponent {
            lhs: QuantityRef::HandSize,
            comparator: Comparator::GT,
            rhs: QuantityRef::HandSize,
        });
    }
    // "you have exactly N or M cards in hand"
    if let Some(rest) = text
        .strip_prefix("you have exactly ")
        .and_then(|r| r.strip_suffix(" cards in hand"))
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
// Helpers (moved from restrictions.rs)
// ---------------------------------------------------------------------------

fn parse_numeric_threshold(text: &str, prefix: &str, suffix: &str) -> Option<usize> {
    let middle = text.strip_prefix(prefix)?.strip_suffix(suffix)?.trim();
    parse_count_word(middle)
}

fn parse_count_word(text: &str) -> Option<usize> {
    let trimmed = text.trim();
    if trimmed == "zero" {
        return Some(0);
    }
    parse_number(trimmed).and_then(|(count, rest)| rest.is_empty().then_some(count as usize))
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
    let stats = text
        .strip_prefix("you control a ")
        .and_then(|rest| rest.strip_suffix(" creature"))?;
    let (power, toughness) = stats.split_once('/')?;
    Some((power.parse().ok()?, toughness.parse().ok()?))
}

fn parse_counter_requirement(text: &str) -> Option<(CounterType, u32)> {
    if let Some(counter_name) = text
        .strip_prefix("this artifact has ")
        .or_else(|| text.strip_prefix("this enchantment has "))
        .and_then(|rest| rest.strip_suffix(" counters on it"))
    {
        let (count_text, counter_name) = counter_name.split_once(" or more ")?;
        return Some((
            parse_counter_type(counter_name),
            parse_count_word(count_text)? as u32,
        ));
    }
    if let Some(counter_name) = text
        .strip_prefix("there are ")
        .and_then(|rest| rest.strip_suffix(" counters on this artifact"))
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
    text.strip_prefix("there are no ")
        .and_then(|rest| rest.strip_suffix(" counters on this artifact"))
        .map(parse_counter_type)
}

fn parse_you_control_land_subtypes(text: &str) -> Option<Vec<String>> {
    if !text.starts_with("you control an ") && !text.starts_with("you control a ") {
        return None;
    }
    let rest = text
        .strip_prefix("you control an ")
        .or_else(|| text.strip_prefix("you control a "))?;
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
    let prefix = "you control ";
    let rest = text.strip_prefix(prefix)?;
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

    #[test]
    fn parses_source_conditions() {
        assert_eq!(
            parse_restriction_condition("~ is attacking"),
            None, // self-ref not in condition text form
        );
        assert_eq!(
            parse_restriction_condition("this creature is attacking"),
            Some(ParsedCondition::SourceIsAttacking),
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
    fn parses_graveyard_conditions() {
        assert!(matches!(
            parse_restriction_condition(
                "there are four or more card types among cards in your graveyard"
            ),
            Some(ParsedCondition::GraveyardCardTypeCountAtLeast { count: 4 })
        ));
        assert!(matches!(
            parse_restriction_condition("there are seven or more cards in your graveyard"),
            Some(ParsedCondition::GraveyardCardCountAtLeast { count: 7 })
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
                lhs: QuantityRef::HandSize,
                comparator: Comparator::GT,
                rhs: QuantityRef::HandSize,
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
        assert_eq!(
            parse_restriction_condition("a creature died this turn"),
            Some(ParsedCondition::CreatureDiedThisTurn),
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
    fn unrecognized_returns_none() {
        assert_eq!(
            parse_restriction_condition("something completely unknown"),
            None,
        );
    }
}
