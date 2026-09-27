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

use std::collections::BTreeSet;

use crate::database::CardDatabase;
use crate::game::deck_loading::{DeckEntry, PlayerDeckList};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;

pub mod ability_chain;
pub mod control;
pub mod mana_ramp;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::mana::ManaCost;

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
