//! Free-interaction detection over resolved deck cards.
//!
//! This is an uncapped structural reading, never a bracket floor. The archived
//! WotC 2025-10-21 Commander brackets update describes "free disruption" as a
//! characteristic of cards on the Game Changers list; the live bracket
//! barometers remain two-card infinite combos, extra turns, mass land denial,
//! and the Game Changers list.
//!
//! Two measured exclusions keep this reading to spells castable for no mana:
//! `AlternativeCost` options that still require mana do not qualify (Baleful
//! Mastery, Devastating Mastery, and Verdant Mastery), and a missing mana cost
//! is not a printed `{0}` (Awaken the Maelstrom, Glimpse of Tomorrow, Living
//! End, Resurgent Belief, Strike the Weak Spot, and Swallow the Hero Whole).
//! CR 202.3a explains why a card with no mana cost nevertheless has mana value
//! zero; matching the typed [`ManaCost`] variant avoids conflating that rule
//! with an actually printed `{0}`.
//!
//! Measured against `data/card-data.json` regenerated 2026-09-26 from the
//! MTGJSON 2026-09-12 vintage: 84 arm-1 faces, 7 arm-2 faces, and 31
//! free-interaction faces. The ignored measurement test below is the executable
//! provenance.

use crate::analysis::deck_signals::{control, DeckCard};
use crate::types::ability::{AbilityCost, SpellCastingOptionKind};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::mana::ManaCost;

/// Counts free interaction by copies and returns distinct contributing names
/// in first-seen order.
pub fn free_interaction(cards: &[DeckCard<'_>]) -> (u32, Vec<String>) {
    let mut count = 0_u32;
    let mut contributing = Vec::new();
    for card in cards {
        if is_free_cast(card) && is_interaction(card.face) {
            count = count.saturating_add(card.count);
            contributing.push(card.face.name.clone());
        }
    }
    (count, contributing)
}

/// A spell that can be cast for no mana.
///
/// Arm 1 accepts cast-without-paying options and alternative costs whose
/// complete typed cost contains no nonzero mana payment. Arm 2 accepts only a
/// printed `{0}` instant or sorcery; [`ManaCost::NoCost`] is deliberately not
/// equivalent to printed `{0}`.
fn is_free_cast(card: &DeckCard<'_>) -> bool {
    let face = card.face;
    let has_free_option = face.casting_options.iter().any(|option| match option.kind {
        SpellCastingOptionKind::AlternativeCost => option
            .cost
            .as_ref()
            .is_some_and(alternative_cost_is_mana_free),
        SpellCastingOptionKind::CastWithoutManaCost => true,
        SpellCastingOptionKind::AsThoughHadFlash | SpellCastingOptionKind::CastAdventure => false,
    });
    let has_printed_zero_cost = matches!(
        &face.mana_cost,
        ManaCost::Cost { shards, generic } if shards.is_empty() && *generic == 0
    );
    let is_zero_cost_nonpermanent = has_printed_zero_cost
        && face
            .card_type
            .core_types
            .iter()
            .any(|card_type| matches!(card_type, CoreType::Instant | CoreType::Sorcery))
        && !face.card_type.core_types.contains(&CoreType::Land);
    has_free_option || is_zero_cost_nonpermanent
}

/// Returns whether every component of an alternative cost is mana-free.
///
/// A literal `{0}` mana component is free. Static non-mana payments (life,
/// exile, sacrifice, discard, returning a permanent, and their peers) are also
/// free of mana. Composed costs qualify only when every branch/component does.
/// Dynamic, borrowed-keyword, and unresolved costs fail closed because the AST
/// does not prove that their payable mana amount is zero.
fn alternative_cost_is_mana_free(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Mana { cost } => matches!(
            cost,
            ManaCost::Cost { shards, generic } if shards.is_empty() && *generic == 0
        ),
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            costs.iter().all(alternative_cost_is_mana_free)
        }
        AbilityCost::PerCounter { base, .. } => alternative_cost_is_mana_free(base),
        AbilityCost::ManaDynamic { .. }
        | AbilityCost::Waterbend { .. }
        | AbilityCost::NinjutsuFamily { .. }
        | AbilityCost::KeywordCostOfCastSpell { .. }
        | AbilityCost::Unimplemented { .. } => false,
        AbilityCost::Tap
        | AbilityCost::Untap
        | AbilityCost::Loyalty { .. }
        | AbilityCost::Sacrifice(_)
        | AbilityCost::PayLife { .. }
        | AbilityCost::Discard { .. }
        | AbilityCost::Exile { .. }
        | AbilityCost::ExileMaterials { .. }
        | AbilityCost::CollectEvidence { .. }
        | AbilityCost::ExileWithAggregate { .. }
        | AbilityCost::TapCreatures { .. }
        | AbilityCost::RemoveCounter { .. }
        | AbilityCost::PayEnergy { .. }
        | AbilityCost::PaySpeed { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Unattach
        | AbilityCost::UnattachFrom { .. }
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { .. }
        | AbilityCost::Behold { .. }
        | AbilityCost::EffectCost { .. }
        | AbilityCost::GetPlayerCounters { .. } => true,
    }
}

/// Reuses the control detectors verbatim; this module adds no interaction
/// vocabulary of its own.
fn is_interaction(face: &CardFace) -> bool {
    control::is_counterspell_parts(&face.abilities)
        || control::is_spot_removal_parts(&face.abilities)
        || control::is_sweeper_parts(&face.abilities)
}

#[cfg(test)]
mod measurement_tests {
    use std::path::Path;

    use super::*;
    use crate::database::CardDatabase;

    #[test]
    #[ignore = "loads the full generated card-data export"]
    fn measure_free_interaction_population() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/card-data.json");
        let db = CardDatabase::from_export(&path).expect("generated card-data export should load");
        let mut arm_1_names = Vec::new();
        let mut arm_2_names = Vec::new();
        let mut conjunction_names = Vec::new();

        for face in db.faces_in_scan_order() {
            let arm_1 = face.casting_options.iter().any(|option| match option.kind {
                SpellCastingOptionKind::AlternativeCost => option
                    .cost
                    .as_ref()
                    .is_some_and(alternative_cost_is_mana_free),
                SpellCastingOptionKind::CastWithoutManaCost => true,
                SpellCastingOptionKind::AsThoughHadFlash
                | SpellCastingOptionKind::CastAdventure => false,
            });
            let arm_2 = matches!(
                &face.mana_cost,
                ManaCost::Cost { shards, generic } if shards.is_empty() && *generic == 0
            ) && face
                .card_type
                .core_types
                .iter()
                .any(|kind| matches!(kind, CoreType::Instant | CoreType::Sorcery))
                && !face.card_type.core_types.contains(&CoreType::Land);
            let card = DeckCard {
                face,
                count: 1,
                mana_value: face.mana_cost.mana_value(),
            };

            if arm_1 {
                arm_1_names.push(face.name.clone());
            }
            if arm_2 {
                arm_2_names.push(face.name.clone());
            }
            if is_free_cast(&card) && is_interaction(face) {
                conjunction_names.push(face.name.clone());
            }
        }

        conjunction_names.sort_unstable_by_key(|name| name.to_lowercase());
        println!(
            "free-interaction measurement: arm_1={} arm_2={} conjunction={}",
            arm_1_names.len(),
            arm_2_names.len(),
            conjunction_names.len()
        );
        println!("conjunction names:");
        for name in conjunction_names {
            println!("{name}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::deck_loading::DeckEntry;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PtValue, QuantityExpr, SearchSelectionConstraint,
        SpellCastingOption, TargetFilter,
    };
    use crate::types::card::CardFace;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::mana::ManaCostShard;
    use crate::types::zones::Zone;

    fn face(name: &str, mana_cost: ManaCost) -> CardFace {
        CardFace {
            name: name.into(),
            mana_cost,
            card_type: CardType {
                core_types: vec![CoreType::Instant],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn counter_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
                countered_spell_zone: None,
            },
        )
    }

    fn sweeper_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DestroyAll {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        )
    }

    fn search_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SearchLibrary {
                source_zones: vec![Zone::Library],
                filter: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
            },
        )
    }

    fn token_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Token {
                name: "Giant".into(),
                power: PtValue::Fixed(4),
                toughness: PtValue::Fixed(4),
                types: vec!["Creature".into(), "Giant".into()],
                colors: Vec::new(),
                keywords: Vec::new(),
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: Vec::new(),
                static_abilities: Vec::new(),
                enter_with_counters: Vec::new(),
            },
        )
    }

    fn cards(entries: &[DeckEntry]) -> Vec<DeckCard<'_>> {
        DeckCard::from_deck_entries(entries)
    }

    #[test]
    fn free_interaction_counts_alternative_cost_and_cast_without_mana_cost() {
        let mut alternative = face("Alternative Counter", ManaCost::generic(5));
        alternative.abilities.push(counter_ability());
        alternative.casting_options.push(SpellCastingOption {
            kind: SpellCastingOptionKind::AlternativeCost,
            cost: Some(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                    AbilityCost::Exile {
                        count: 1,
                        zone: Some(Zone::Hand),
                        filter: None,
                    },
                ],
            }),
            condition: None,
        });
        let mut without_mana = face("Free Counter", ManaCost::generic(4));
        without_mana.abilities.push(counter_ability());
        without_mana
            .casting_options
            .push(SpellCastingOption::free_cast());
        let entries = vec![
            DeckEntry {
                card: alternative,
                count: 2,
            },
            DeckEntry {
                card: without_mana,
                count: 3,
            },
        ];
        let cards = cards(&entries);

        let (count, names) = free_interaction(&cards);

        assert_eq!(count, 5);
        assert_eq!(names, ["Alternative Counter", "Free Counter"]);
    }

    #[test]
    fn free_interaction_zero_cost_arm_requires_interaction() {
        let mut counter = face("Zero Counter", ManaCost::zero());
        counter.abilities.push(counter_ability());
        let mut search = face("Zero Search", ManaCost::zero());
        search.abilities.push(search_ability());
        let mut token = face("Zero Token", ManaCost::zero());
        token.abilities.push(token_ability());
        let entries = vec![
            DeckEntry {
                card: counter,
                count: 1,
            },
            DeckEntry {
                card: search,
                count: 1,
            },
            DeckEntry {
                card: token,
                count: 1,
            },
        ];
        let cards = cards(&entries);

        let (count, names) = free_interaction(&cards);

        assert_eq!(count, 1);
        assert_eq!(names, ["Zero Counter"]);
    }

    #[test]
    fn free_interaction_excludes_flash_and_adventure_options() {
        let mut flash = face("Flash Counter", ManaCost::generic(2));
        flash.abilities.push(counter_ability());
        flash
            .casting_options
            .push(SpellCastingOption::as_though_had_flash());
        let mut adventure = face("Adventure Counter", ManaCost::generic(2));
        adventure.abilities.push(counter_ability());
        adventure.casting_options.push(SpellCastingOption {
            kind: SpellCastingOptionKind::CastAdventure,
            cost: None,
            condition: None,
        });
        let entries = vec![
            DeckEntry {
                card: flash,
                count: 1,
            },
            DeckEntry {
                card: adventure,
                count: 1,
            },
        ];
        let cards = cards(&entries);

        assert_eq!(free_interaction(&cards), (0, Vec::new()));
    }

    #[test]
    fn free_interaction_excludes_a_cheaper_mana_alternative_cost() {
        let mut mastery = face("Mastery Shape", ManaCost::generic(4));
        mastery.abilities.push(counter_ability());
        mastery.casting_options.push(SpellCastingOption {
            kind: SpellCastingOptionKind::AlternativeCost,
            cost: Some(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Black],
                    generic: 1,
                },
            }),
            condition: None,
        });
        let entries = vec![DeckEntry {
            card: mastery,
            count: 1,
        }];
        let cards = cards(&entries);

        assert_eq!(free_interaction(&cards), (0, Vec::new()));
    }

    #[test]
    fn free_interaction_excludes_spells_with_no_mana_cost() {
        let mut no_cost = face("No Cost Sweeper", ManaCost::NoCost);
        no_cost.abilities.push(sweeper_ability());
        let mut printed_zero = face("Printed Zero Sweeper", ManaCost::zero());
        printed_zero.abilities.push(sweeper_ability());
        let entries = vec![
            DeckEntry {
                card: no_cost,
                count: 1,
            },
            DeckEntry {
                card: printed_zero,
                count: 1,
            },
        ];
        let cards = cards(&entries);

        assert_eq!(
            free_interaction(&cards),
            (1, vec!["Printed Zero Sweeper".into()])
        );
    }
}
