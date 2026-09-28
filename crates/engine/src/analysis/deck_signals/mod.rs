// allow: no_name_matching_self -- resolver identity-folds resolved face names;
// structural classifiers live in child modules and remain linted independently.
//! Uncapped structural deck signals.
//!
//! This is not [`crate::database::BracketSignals`], which is a per-card,
//! build-time curated-name-list flag set. This module is a per-deck, runtime
//! structural reading over the parsed AST. Nothing here is capped, floors a
//! tier, or produces a bracket violation.
//!
//! Commander brackets are WotC Commander Format Panel guidance, not the
//! Comprehensive Rules, so no rules annotation applies to the signal taxonomy
//! itself.
//!
//! Resolution happens once per call and the borrowed result is shared by every
//! detector. There is deliberately no engine-side cross-call memo: analysis is
//! pure, [`CardDatabase`] has no generation counter with which to invalidate a
//! cache after reload, and request-lifetime caching belongs to the caller.
//!
//! `phase-ai/src/deck_profile.rs` intentionally retains its existing average
//! mana-value calculation for now. Converging that AI input onto this module's
//! off-stack value would be a separate behavior change with its own baseline
//! review.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;

use crate::database::CardDatabase;
use crate::game::deck_loading::{DeckEntry, PlayerDeckList};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;

pub mod ability_chain;
pub mod control;
pub mod free_interaction;
pub mod mana_ramp;

/// Which uncapped structural reading this is.
///
/// This taxonomy is disjoint from capped bracket criteria by construction. It
/// has no cap table and cannot produce a bracket violation. Commander brackets
/// are format guidance rather than Comprehensive Rules, so no rules annotation
/// applies to these variants.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    strum::EnumIter,
)]
#[serde(rename_all = "snake_case")]
pub enum DeckSignalKind {
    Counterspells,
    SpotRemoval,
    Sweepers,
    CardAdvantage,
    FreeInteraction,
    ManaProducers,
    LandFetch,
    Rituals,
    ExtraLandDrops,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckSignalReading {
    pub count: u16,
    /// Distinct card names in first-seen order. Copy quantity is carried by
    /// [`Self::count`], so each resolved face appears at most once here.
    pub contributing: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckSignals {
    /// Every [`DeckSignalKind`] is always present, including zero readings.
    /// Declaration order is wire order because the key is an ordered enum.
    pub readings: BTreeMap<DeckSignalKind, DeckSignalReading>,
    pub nonland_cards: u16,
    /// Integer hundredths. `None` means the deck has no resolved nonland card.
    pub average_mana_value_centi: Option<u16>,
    pub resolved_cards: u16,
    pub unresolved_cards: u16,
}

/// One deck slot: a borrowed face, its copy count, and its rules-correct
/// off-stack mana value.
///
/// Borrowed rather than cloned because [`CardDatabase::get_face_by_name`]
/// already returns a card-face reference.
#[derive(Debug, Clone, Copy)]
pub struct DeckCard<'a> {
    pub face: &'a CardFace,
    pub count: u32,
    /// CR 202.3d + CR 709.4b: the combined off-stack value for a split card.
    /// Built from the database or deck-entry off-stack authority, never from
    /// one face's mana cost alone.
    pub mana_value: u32,
}

/// The resolved bracket-relevant sections of a name-only deck list, plus names
/// that the database could not resolve.
pub struct ResolvedDeck<'a> {
    pub cards: Vec<DeckCard<'a>>,
    pub unresolved: Vec<String>,
}

/// Resolves the four sections bracket estimation counts, once per call.
///
/// Distinct resolved cards remain in first-seen order. Repeated spellings of
/// the same resolved face fold case-insensitively into its copy count. Unknown
/// names are deduplicated and sorted.
pub fn resolve_bracket_sections<'a>(
    deck: &PlayerDeckList,
    db: &'a CardDatabase,
) -> ResolvedDeck<'a> {
    // Keep this exhaustive destructure aligned with `game::bracket_estimate`:
    // adding a deck-list section forces an explicit counted/not-counted choice.
    let PlayerDeckList {
        commander,
        main_deck,
        companion,
        signature_spell,
        sideboard,
        attraction_deck,
        planar_deck,
        scheme_deck,
        contraption_deck,
        sticker_sheets,
        bracket_tier: _,
        combo_declaration: _,
    } = deck;
    let _ = (
        sideboard,
        attraction_deck,
        planar_deck,
        scheme_deck,
        contraption_deck,
        sticker_sheets,
    );

    let names = commander
        .iter()
        .chain(main_deck.iter())
        .chain(companion.iter())
        .chain(signature_spell.iter());
    let mut cards: Vec<DeckCard<'a>> = Vec::new();
    let mut unresolved = BTreeSet::new();
    for name in names {
        let Some(face) = db.get_face_by_name(name) else {
            unresolved.insert(name.clone());
            continue;
        };
        if let Some(card) = cards
            .iter_mut()
            .find(|card| card.face.name.eq_ignore_ascii_case(&face.name))
        {
            card.count = card.count.saturating_add(1);
        } else {
            cards.push(DeckCard {
                face,
                count: 1,
                mana_value: db.off_stack_mana_value_for_face(face),
            });
        }
    }

    ResolvedDeck {
        cards,
        unresolved: unresolved.into_iter().collect(),
    }
}

impl<'a> DeckCard<'a> {
    /// Borrows the owned entries used by `phase-ai` without re-resolving names.
    pub fn from_deck_entries(entries: &'a [DeckEntry]) -> Vec<DeckCard<'a>> {
        entries
            .iter()
            .map(|entry| DeckCard {
                face: &entry.card,
                count: entry.count,
                mana_value: entry.off_stack_mana_value(),
            })
            .collect()
    }
}

/// Returns the quantity-weighted number of nonland cards.
pub fn nonland_card_count(cards: &[DeckCard<'_>]) -> u32 {
    cards
        .iter()
        .filter(|card| !card.face.card_type.core_types.contains(&CoreType::Land))
        .fold(0_u32, |total, card| total.saturating_add(card.count))
}

/// Quantity-weighted mean mana value of nonland cards, in integer hundredths.
/// Returns `None` when the deck has no nonland card.
///
/// Uses [`DeckCard::mana_value`], the combined off-stack value for split cards:
/// CR 202.3d defines that combined mana value off the stack, and CR 709.4b
/// defines a split card's combined mana cost. The calculation is integer-only
/// and rounds half up. `crates/phase-ai/src/deck_profile.rs` retains the known
/// `avg_mana_value` duplicate pending a separately baselined convergence.
pub fn average_mana_value_centi(cards: &[DeckCard<'_>]) -> Option<u16> {
    let (total_mana_value, nonland_cards) = cards
        .iter()
        .filter(|card| !card.face.card_type.core_types.contains(&CoreType::Land))
        .fold((0_u64, 0_u64), |(total, count), card| {
            let copies = u64::from(card.count);
            (
                total.saturating_add(u64::from(card.mana_value).saturating_mul(copies)),
                count.saturating_add(copies),
            )
        });
    if nonland_cards == 0 {
        return None;
    }
    let centi = total_mana_value
        .saturating_mul(100)
        .saturating_add(nonland_cards / 2)
        / nonland_cards;
    Some(u16::try_from(centi).unwrap_or(u16::MAX))
}

/// Resolves and classifies the deck sections used by bracket estimation.
/// Returns `None` when the deck has no commander, matching the estimator gate.
pub fn deck_signals(deck: &PlayerDeckList, db: &CardDatabase) -> Option<DeckSignals> {
    if deck.commander.is_empty() {
        return None;
    }

    let resolved = resolve_bracket_sections(deck, db);
    let control = control::detect(&resolved.cards);
    let ramp = mana_ramp::detect(&resolved.cards);
    let (free_interaction_count, free_interaction_names) =
        free_interaction::free_interaction(&resolved.cards);

    let readings = DeckSignalKind::iter()
        .map(|kind| {
            let count = match kind {
                DeckSignalKind::Counterspells => control.counterspell_count,
                DeckSignalKind::SpotRemoval => control.spot_removal_count,
                DeckSignalKind::Sweepers => control.sweeper_count,
                DeckSignalKind::CardAdvantage => control.card_draw_count,
                DeckSignalKind::FreeInteraction => free_interaction_count,
                DeckSignalKind::ManaProducers => ramp.dork_count,
                DeckSignalKind::LandFetch => ramp.land_fetch_count,
                DeckSignalKind::Rituals => ramp.ritual_count,
                DeckSignalKind::ExtraLandDrops => ramp.extra_landdrop_count,
            };
            let contributing = if kind == DeckSignalKind::FreeInteraction {
                free_interaction_names.clone()
            } else {
                resolved
                    .cards
                    .iter()
                    .filter(|card| card_matches_kind(card, kind))
                    .map(|card| card.face.name.clone())
                    .collect()
            };
            (
                kind,
                DeckSignalReading {
                    count: u16::try_from(count).unwrap_or(u16::MAX),
                    contributing,
                },
            )
        })
        .collect();

    let resolved_cards = resolved
        .cards
        .iter()
        .fold(0_u32, |total, card| total.saturating_add(card.count));

    Some(DeckSignals {
        readings,
        nonland_cards: u16::try_from(nonland_card_count(&resolved.cards)).unwrap_or(u16::MAX),
        average_mana_value_centi: average_mana_value_centi(&resolved.cards),
        resolved_cards: u16::try_from(resolved_cards).unwrap_or(u16::MAX),
        unresolved_cards: u16::try_from(resolved.unresolved.len()).unwrap_or(u16::MAX),
    })
}

fn card_matches_kind(card: &DeckCard<'_>, kind: DeckSignalKind) -> bool {
    let face = card.face;
    match kind {
        DeckSignalKind::Counterspells => control::is_counterspell_parts(&face.abilities),
        DeckSignalKind::SpotRemoval => {
            !control::is_sweeper_parts(&face.abilities)
                && control::is_spot_removal_parts(&face.abilities)
        }
        DeckSignalKind::Sweepers => control::is_sweeper_parts(&face.abilities),
        DeckSignalKind::CardAdvantage => {
            !face.card_type.core_types.contains(&CoreType::Land)
                && control::is_card_draw_parts(&face.abilities)
        }
        DeckSignalKind::FreeInteraction => {
            unreachable!("free-interaction contributors come from its single authority")
        }
        DeckSignalKind::ManaProducers => {
            mana_ramp::is_mana_dork_parts(&face.card_type.core_types, &face.abilities)
        }
        DeckSignalKind::LandFetch => {
            mana_ramp::is_land_fetch_spell_parts(&face.card_type.core_types, &face.abilities)
        }
        DeckSignalKind::Rituals => {
            mana_ramp::is_ritual_parts(&face.card_type.core_types, &face.abilities)
        }
        DeckSignalKind::ExtraLandDrops => {
            mana_ramp::is_extra_landdrop_parts(&face.static_abilities)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::deck_loading::DeckEntry;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::mana::ManaCost;
    use strum::IntoEnumIterator;

    const CARD_DATA: &str = r#"{
        "commander": {
            "name": "Commander", "mana_cost": { "type": "NoCost" },
            "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
            "power": null, "toughness": null, "loyalty": null, "defense": null,
            "oracle_text": null, "abilities": [], "triggers": [], "static_abilities": [],
            "replacements": [], "keywords": []
        },
        "island": {
            "name": "Island", "mana_cost": { "type": "NoCost" },
            "card_type": { "supertypes": ["Basic"], "core_types": ["Land"], "subtypes": ["Island"] },
            "power": null, "toughness": null, "loyalty": null, "defense": null,
            "oracle_text": null, "abilities": [], "triggers": [], "static_abilities": [],
            "replacements": [], "keywords": []
        }
    }"#;

    fn face(name: &str, mana_value: u32, core_types: Vec<CoreType>) -> CardFace {
        CardFace {
            name: name.into(),
            mana_cost: ManaCost::generic(mana_value),
            card_type: CardType {
                core_types,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn cards(entries: &[DeckEntry]) -> Vec<DeckCard<'_>> {
        DeckCard::from_deck_entries(entries)
    }

    #[test]
    fn average_mana_value_centi_excludes_lands_and_weights_by_count() {
        let entries = vec![
            DeckEntry {
                card: face("Two", 2, vec![CoreType::Creature]),
                count: 3,
            },
            DeckEntry {
                card: face("Six", 6, vec![CoreType::Sorcery]),
                count: 1,
            },
            DeckEntry {
                card: face("Land", 0, vec![CoreType::Land]),
                count: 40,
            },
        ];
        let cards = cards(&entries);

        assert_eq!(average_mana_value_centi(&cards), Some(300));
    }

    #[test]
    fn average_mana_value_centi_is_none_for_all_land_deck() {
        let entries = vec![DeckEntry {
            card: face("Land", 0, vec![CoreType::Land]),
            count: 40,
        }];
        let cards = cards(&entries);

        assert_eq!(average_mana_value_centi(&cards), None);
    }

    #[test]
    fn average_mana_value_centi_rounds_half_up() {
        let entries = vec![
            DeckEntry {
                card: face("Two", 2, vec![CoreType::Creature]),
                count: 199,
            },
            DeckEntry {
                card: face("Three", 3, vec![CoreType::Creature]),
                count: 1,
            },
        ];
        let cards = cards(&entries);

        assert_eq!(average_mana_value_centi(&cards), Some(201));
    }

    #[test]
    fn average_mana_value_centi_saturates_within_u16_for_a_100_card_deck() {
        let mut enormous = face("Enormous", 0, vec![CoreType::Creature]);
        enormous.metadata.off_stack_mana_value_override = Some(u32::MAX);
        let entries = vec![DeckEntry {
            card: enormous,
            count: 100,
        }];
        let cards = cards(&entries);

        assert_eq!(average_mana_value_centi(&cards), Some(u16::MAX));
    }

    #[test]
    fn deck_signals_emits_every_kind_even_at_zero() {
        let db = CardDatabase::from_json_str(CARD_DATA).unwrap();
        let deck = PlayerDeckList {
            commander: vec!["Commander".into()],
            ..Default::default()
        };

        let signals = deck_signals(&deck, &db).expect("commander produces signals");

        for kind in DeckSignalKind::iter() {
            assert_eq!(signals.readings[&kind].count, 0);
            assert!(signals.readings[&kind].contributing.is_empty());
        }
    }

    #[test]
    fn resolve_bracket_sections_folds_duplicates_and_preserves_first_seen_order() {
        let db = CardDatabase::from_json_str(CARD_DATA).unwrap();
        let deck = PlayerDeckList {
            commander: vec!["Commander".into()],
            main_deck: vec!["Island".into(), "COMMANDER".into()],
            companion: vec!["missing-z".into()],
            signature_spell: vec!["missing-a".into(), "missing-z".into()],
            ..Default::default()
        };

        let resolved = resolve_bracket_sections(&deck, &db);

        assert_eq!(resolved.cards.len(), 2);
        assert_eq!(resolved.cards[0].face.name, "Commander");
        assert_eq!(resolved.cards[0].count, 2);
        assert_eq!(resolved.cards[1].face.name, "Island");
        assert_eq!(resolved.unresolved, ["missing-a", "missing-z"]);
    }

    #[test]
    fn resolve_bracket_sections_reads_exactly_the_four_bracket_sections() {
        let db = CardDatabase::from_json_str(CARD_DATA).unwrap();
        let deck = PlayerDeckList {
            commander: vec!["Commander".into()],
            main_deck: vec!["Island".into()],
            companion: vec!["Island".into()],
            signature_spell: vec!["Commander".into()],
            sideboard: vec!["Island".into()],
            attraction_deck: vec!["Island".into()],
            planar_deck: vec!["Island".into()],
            scheme_deck: vec!["Island".into()],
            contraption_deck: vec!["Island".into()],
            sticker_sheets: vec!["Island".into()],
            ..Default::default()
        };

        let resolved = resolve_bracket_sections(&deck, &db);

        assert_eq!(resolved.cards[0].count, 2);
        assert_eq!(resolved.cards[1].count, 2);
    }

    #[test]
    fn deck_card_uses_entry_off_stack_mana_value_and_nonland_count_weights_copies() {
        let mut split = CardFace {
            name: "Split".into(),
            mana_cost: ManaCost::generic(2),
            card_type: CardType {
                core_types: vec![CoreType::Instant],
                ..Default::default()
            },
            ..Default::default()
        };
        split.metadata.off_stack_mana_value_override = Some(5);
        let land = CardFace {
            name: "Land".into(),
            card_type: CardType {
                core_types: vec![CoreType::Land],
                ..Default::default()
            },
            ..Default::default()
        };
        let entries = vec![
            DeckEntry {
                card: split,
                count: 3,
            },
            DeckEntry {
                card: land,
                count: 40,
            },
        ];

        let cards = DeckCard::from_deck_entries(&entries);

        assert_eq!(cards[0].mana_value, 5);
        assert_eq!(nonland_card_count(&cards), 3);
    }
}
