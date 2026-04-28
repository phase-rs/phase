//! mtgish `Rule::Companion(Companion)` → engine
//! `Keyword::Companion(CompanionCondition)`.
//!
//! CR 702.139: Companion is a deckbuilding-time keyword. Each of the 10
//! companion cards has a unique `CompanionCondition` enumerated in the
//! engine (Gyruda → EvenManaValues, Lurrus → MaxPermanentManaValue, etc.).
//! mtgish encodes the same condition as a structural filter expression
//! over `Cards` / `GroupFilter` / `GameNumber`.
//!
//! This module pattern-matches the structural shape of each filter and
//! emits the matching `CompanionCondition` variant. Unrecognised shapes
//! strict-fail so the report tracks any new companion that ships with a
//! different filter shape than the established 10.

use engine::types::keywords::CompanionCondition;

use crate::convert::filter::creature_type_name;
use crate::convert::result::{ConvResult, ConversionGap};
use crate::schema::types::{Cards, CheckHasable, Companion, Comparison, GameNumber, GroupFilter};

/// CR 702.139a: Decompose a `Companion` filter expression into the
/// engine's typed `CompanionCondition`. Each of the 10 known companion
/// shapes maps to exactly one variant.
pub fn convert(c: &Companion) -> ConvResult<CompanionCondition> {
    match c {
        // Gyruda / Obosh family: AllCardsPassFilter wrapping a
        // ManaValueIs comparison. Bare `Even`/`Odd` is Gyruda/Obosh
        // straight; `Or([ManaValueIs(Even|Odd), IsCardtype(Land)])` is
        // the same shape with the land exemption.
        Companion::AllCardsPassFilter(cards) => mana_value_companion(cards),

        // Kaheera / Lurrus / Zirda: EachCardPassesFilter — first arg is
        // the pre-filter (creature / permanent), second is the
        // condition the matching cards must satisfy.
        Companion::EachCardPassesFilter(prefilter, condition) => {
            each_card_companion(prefilter, condition)
        }

        // Lutri / Umori: EachCardPassesGroupFilter — pre-filter +
        // group-level constraint (DifferentNames / ShareACardType).
        Companion::EachCardPassesGroupFilter(prefilter, group) => {
            each_card_group_companion(prefilter, group)
        }

        // Yorion: IncreaseStartingDeckSize(N).
        Companion::IncreaseStartingDeckSize(g) => match &**g {
            GameNumber::Integer(n) => {
                let value = u32::try_from(*n).map_err(|_| ConversionGap::MalformedIdiom {
                    idiom: "Companion::IncreaseStartingDeckSize",
                    path: String::new(),
                    detail: format!("expected non-negative deck-size delta, got {n}"),
                })?;
                Ok(CompanionCondition::MinDeckSizeOver(value))
            }
            other => Err(ConversionGap::MalformedIdiom {
                idiom: "Companion::IncreaseStartingDeckSize",
                path: String::new(),
                detail: format!("expected Integer, got {other:?}"),
            }),
        },

        // Jegantha: NoCardPassesFilter(HasMoreThanOneOfTheSameManaSymbolInCost).
        Companion::NoCardPassesFilter(cards) => match &**cards {
            Cards::HasMoreThanOneOfTheSameManaSymbolInCost => {
                Ok(CompanionCondition::NoRepeatedManaSymbols)
            }
            other => Err(ConversionGap::MalformedIdiom {
                idiom: "Companion::NoCardPassesFilter",
                path: String::new(),
                detail: format!("unrecognised filter: {}", cards_variant_tag(other)),
            }),
        },
    }
}

/// AllCardsPassFilter is Gyruda / Obosh / Keruga. The filter is either a
/// bare `ManaValueIs` (no land exemption) or an `Or([ManaValueIs, IsCardtype(Land)])`
/// (the canonical "land cards are exempt" shape).
fn mana_value_companion(cards: &Cards) -> ConvResult<CompanionCondition> {
    match cards {
        Cards::ManaValueIs(comp) => mana_value_comparison_to_companion(comp),
        Cards::Or(parts) => {
            let (mv, has_land_exemption) = split_or_for_land_exemption(parts)?;
            if !has_land_exemption {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Companion::AllCardsPassFilter::Or",
                    path: String::new(),
                    detail: "expected a Land exemption alongside ManaValueIs".into(),
                });
            }
            mana_value_comparison_to_companion(mv)
        }
        other => Err(ConversionGap::MalformedIdiom {
            idiom: "Companion::AllCardsPassFilter",
            path: String::new(),
            detail: format!("unrecognised filter: {}", cards_variant_tag(other)),
        }),
    }
}

/// CR 702.139a: Split an `Or([ManaValueIs(_), IsCardtype(Land)])` filter
/// into the mana-value comparison and a flag for the land-exemption
/// branch. Returns `Err` if neither branch is `ManaValueIs` or if the
/// land exemption is missing.
fn split_or_for_land_exemption(parts: &[Cards]) -> ConvResult<(&Comparison, bool)> {
    let mut mana_value: Option<&Comparison> = None;
    let mut has_land = false;
    for p in parts {
        match p {
            Cards::ManaValueIs(c) => mana_value = Some(c),
            Cards::IsCardtype(crate::schema::types::CardType::Land) => has_land = true,
            _ => {}
        }
    }
    let comp = mana_value.ok_or(ConversionGap::MalformedIdiom {
        idiom: "Companion::Or split",
        path: String::new(),
        detail: "no ManaValueIs branch in Or".into(),
    })?;
    Ok((comp, has_land))
}

/// Map the mana-value `Comparison` to a `CompanionCondition`. `Even`
/// → Gyruda (EvenManaValues); `Odd` → Obosh (OddManaValues);
/// `GreaterThanOrEqualTo(N)` → Keruga (MinManaValue).
fn mana_value_comparison_to_companion(comp: &Comparison) -> ConvResult<CompanionCondition> {
    match comp {
        Comparison::Even => Ok(CompanionCondition::EvenManaValues),
        Comparison::Odd => Ok(CompanionCondition::OddManaValues),
        Comparison::GreaterThanOrEqualTo(g) => match &**g {
            GameNumber::Integer(n) => {
                let value = u32::try_from(*n).map_err(|_| ConversionGap::MalformedIdiom {
                    idiom: "Companion::MinManaValue",
                    path: String::new(),
                    detail: format!("expected non-negative threshold, got {n}"),
                })?;
                Ok(CompanionCondition::MinManaValue(value))
            }
            other => Err(ConversionGap::MalformedIdiom {
                idiom: "Companion::MinManaValue",
                path: String::new(),
                detail: format!("expected Integer threshold, got {other:?}"),
            }),
        },
        Comparison::LessThanOrEqualTo(g) => match &**g {
            GameNumber::Integer(n) => {
                let value = u32::try_from(*n).map_err(|_| ConversionGap::MalformedIdiom {
                    idiom: "Companion::MaxPermanentManaValue",
                    path: String::new(),
                    detail: format!("expected non-negative threshold, got {n}"),
                })?;
                Ok(CompanionCondition::MaxPermanentManaValue(value))
            }
            other => Err(ConversionGap::MalformedIdiom {
                idiom: "Companion::MaxPermanentManaValue",
                path: String::new(),
                detail: format!("expected Integer threshold, got {other:?}"),
            }),
        },
        other => Err(ConversionGap::MalformedIdiom {
            idiom: "Companion::ManaValueIs",
            path: String::new(),
            detail: format!("unrecognised comparison: {other:?}"),
        }),
    }
}

/// EachCardPassesFilter is Kaheera / Lurrus / Zirda. The first arg is
/// the pre-filter (Creature / Permanent), the second arg is the
/// condition every matching card must satisfy.
fn each_card_companion(prefilter: &Cards, condition: &Cards) -> ConvResult<CompanionCondition> {
    match (prefilter, condition) {
        // Kaheera: prefilter `IsCardtype(Creature)`, condition is an
        // `Or` of `IsCreatureType(<one of N>)` branches.
        (Cards::IsCardtype(crate::schema::types::CardType::Creature), Cards::Or(parts)) => {
            let mut types = Vec::with_capacity(parts.len());
            for p in parts {
                match p {
                    Cards::IsCreatureType(ct) => types.push(creature_type_name(ct)),
                    other => {
                        return Err(ConversionGap::MalformedIdiom {
                            idiom: "Companion::CreatureTypeRestriction",
                            path: String::new(),
                            detail: format!(
                                "expected IsCreatureType branch, got {}",
                                cards_variant_tag(other)
                            ),
                        });
                    }
                }
            }
            Ok(CompanionCondition::CreatureTypeRestriction(types))
        }
        // Lurrus: prefilter `IsPermanent`, condition is `ManaValueIs(<=N)`.
        (Cards::IsPermanent, Cards::ManaValueIs(c)) => mana_value_comparison_to_companion(c),
        // Zirda: prefilter `IsPermanent`, condition is `HasAbility(ActivatedAbility)`.
        (Cards::IsPermanent, Cards::HasAbility(CheckHasable::ActivatedAbility)) => {
            Ok(CompanionCondition::PermanentsHaveActivatedAbilities)
        }
        _ => Err(ConversionGap::MalformedIdiom {
            idiom: "Companion::EachCardPassesFilter",
            path: String::new(),
            detail: format!(
                "unrecognised pair: ({}, {})",
                cards_variant_tag(prefilter),
                cards_variant_tag(condition)
            ),
        }),
    }
}

/// EachCardPassesGroupFilter is Lutri / Umori. Pre-filter is
/// `IsNonCardtype(Land)`; group filter is the deck-wide constraint.
fn each_card_group_companion(
    prefilter: &Cards,
    group: &GroupFilter,
) -> ConvResult<CompanionCondition> {
    match (prefilter, group) {
        (
            Cards::IsNonCardtype(crate::schema::types::CardType::Land),
            GroupFilter::DifferentNames,
        ) => Ok(CompanionCondition::Singleton),
        (
            Cards::IsNonCardtype(crate::schema::types::CardType::Land),
            GroupFilter::ShareACardType,
        ) => Ok(CompanionCondition::SharedCardType),
        _ => Err(ConversionGap::MalformedIdiom {
            idiom: "Companion::EachCardPassesGroupFilter",
            path: String::new(),
            detail: format!(
                "unrecognised group filter: ({}, {:?})",
                cards_variant_tag(prefilter),
                group
            ),
        }),
    }
}

fn cards_variant_tag(c: &Cards) -> String {
    serde_json::to_value(c)
        .ok()
        .and_then(|v| v.get("_Cards").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string())
}
