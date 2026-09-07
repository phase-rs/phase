//! Shared man-land (creature-land) reasoning.
//!
//! One structural detector for the "activated ability that turns this permanent
//! into a creature" shape, the animated body it produces, and whether activating
//! it would leave the source tapped. Detection is structural — an
//! [`Effect::Animate`], or a [`Effect::GenericEffect`] whose continuous
//! modifications add the Creature type — never a card-name list, so it covers the
//! whole class (Mutavault, Treetop Village, Celestial Colonnade, Raging Ravine,
//! Lumbering Falls, Shambling Vent, ...).
//!
//! Two consumers:
//! - [`crate::policies::land_animation`] — own-side timing ("should I animate
//!   this land now?").
//! - [`crate::combat_ai`] — opponent-side foresight ("an un-animated man-land the
//!   defender can still turn on is a latent blocker", CR 509.1a).

use std::collections::HashSet;

use engine::game::casting::can_pay_ability_mana_cost_after_auto_tap_excluding;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, ContinuousModification, CostCategory, Effect, PtValue,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;

/// True when activating `ability` turns its source into a creature. Structural:
/// walks the ability's full effect chain (including sub-abilities and modal
/// branches, via [`crate::cast_facts::collect_definition_effects`]).
pub(crate) fn animates_source(ability: &AbilityDefinition) -> bool {
    crate::cast_facts::collect_definition_effects(ability)
        .into_iter()
        .any(effect_animates_land)
}

fn effect_animates_land(effect: &Effect) -> bool {
    match effect {
        Effect::Animate { .. } => true,
        Effect::GenericEffect {
            static_abilities, ..
        } => static_abilities
            .iter()
            .any(|static_ability| static_ability.modifications.iter().any(adds_creature_type)),
        _ => false,
    }
}

fn adds_creature_type(modification: &ContinuousModification) -> bool {
    matches!(
        modification,
        ContinuousModification::AddType {
            core_type: CoreType::Creature
        }
    )
}

/// Mana value of `ability`'s activation cost, or `None` when the cost is absent,
/// non-mana, or dynamically priced (no current man-land has such a cost — those
/// fall through to the caller's conservative handling).
pub(crate) fn animation_mana_value(ability: &AbilityDefinition) -> Option<u32> {
    match ability.cost.as_ref()? {
        AbilityCost::Mana { cost } => Some(cost.mana_value()),
        _ => None,
    }
}

/// The base power / toughness / keywords the animation grants. Reads
/// `SetPower` / `SetToughness` / `AddKeyword` continuous modifications from a
/// `GenericEffect`, and `PtValue::Fixed` power/toughness plus `keywords` from an
/// `Effect::Animate`. A dimension the animation does not set stays `0` — matching
/// the engine's `SetPower`/`SetToughness` semantics for a land with no printed
/// P/T.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AnimatedBody {
    pub power: i32,
    pub toughness: i32,
    pub keywords: Vec<Keyword>,
}

impl AnimatedBody {
    pub(crate) fn has_keyword(&self, keyword: &Keyword) -> bool {
        self.keywords.contains(keyword)
    }
}

fn merge_keyword(list: &mut Vec<Keyword>, keyword: &Keyword) {
    if !list.contains(keyword) {
        list.push(keyword.clone());
    }
}

pub(crate) fn extract_body(ability: &AbilityDefinition) -> AnimatedBody {
    let mut body = AnimatedBody::default();
    for effect in crate::cast_facts::collect_definition_effects(ability) {
        match effect {
            Effect::GenericEffect {
                static_abilities, ..
            } => {
                for modification in static_abilities
                    .iter()
                    .flat_map(|sa| sa.modifications.iter())
                {
                    match modification {
                        ContinuousModification::SetPower { value } => body.power = *value,
                        ContinuousModification::SetToughness { value } => body.toughness = *value,
                        ContinuousModification::AddKeyword { keyword } => {
                            merge_keyword(&mut body.keywords, keyword)
                        }
                        _ => {}
                    }
                }
            }
            Effect::Animate {
                power,
                toughness,
                keywords,
                ..
            } => {
                if let Some(PtValue::Fixed(value)) = power {
                    body.power = *value;
                }
                if let Some(PtValue::Fixed(value)) = toughness {
                    body.toughness = *value;
                }
                for keyword in keywords {
                    merge_keyword(&mut body.keywords, keyword);
                }
            }
            _ => {}
        }
    }
    body
}

fn cost_taps_source(ability: &AbilityDefinition) -> bool {
    ability.cost.as_ref().is_some_and(|cost| {
        cost.categories()
            .into_iter()
            .any(|category| category == CostCategory::TapsSelf)
    })
}

/// CR 508.1a / CR 509.1a: returns true when activating `ability` would leave the
/// source `source_id` tapped — so the animated creature could neither attack nor
/// block this turn. Three ways this happens:
///   1. the source is already tapped,
///   2. the activation cost itself taps the source,
///   3. paying the mana cost forces auto-tapping the source (CR 605.3b): the
///      engine deprioritizes the source but taps it as a last resort, i.e.
///      exactly when the cost cannot be paid *without* it.
///
/// `player` is the source's controller. `ability_index` is the source's ability
/// index in the engine's enumerated space.
pub(crate) fn activation_leaves_source_tapped(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    ability: &AbilityDefinition,
) -> bool {
    let already_tapped = state
        .objects
        .get(&source_id)
        .map(|object| object.tapped)
        .unwrap_or(true);
    if already_tapped || cost_taps_source(ability) {
        return true;
    }

    // Only plain mana costs are assessable here; a non-mana / dynamically-priced
    // animation cost falls through as "does not force a tap".
    let Some(AbilityCost::Mana { cost }) = ability.cost.as_ref() else {
        return false;
    };
    if cost.mana_value() == 0 {
        return false;
    }

    let excluded = HashSet::from([source_id]);
    !can_pay_ability_mana_cost_after_auto_tap_excluding(
        state,
        player,
        source_id,
        Some(ability_index),
        cost,
        &excluded,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Duration, Effect, StaticDefinition,
    };
    use engine::types::mana::ManaCost;
    use engine::types::statics::StaticMode;

    fn animate_ability(generic: u32, mods: Vec<ContinuousModification>) -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GenericEffect {
                static_abilities: vec![
                    StaticDefinition::new(StaticMode::Continuous).modifications(mods)
                ],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
                end_cost: None,
            },
        );
        ability.cost = Some(AbilityCost::Mana {
            cost: ManaCost::generic(generic),
        });
        ability
    }

    #[test]
    fn detects_generic_effect_animation() {
        let ability = animate_ability(
            1,
            vec![
                ContinuousModification::SetPower { value: 2 },
                ContinuousModification::SetToughness { value: 2 },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                },
            ],
        );
        assert!(animates_source(&ability));
    }

    #[test]
    fn non_animation_ability_is_not_detected() {
        let ability = animate_ability(1, vec![ContinuousModification::SetPower { value: 5 }]);
        assert!(!animates_source(&ability));
    }

    #[test]
    fn extract_body_reads_pt_and_keywords() {
        let ability = animate_ability(
            2,
            vec![
                ContinuousModification::SetPower { value: 3 },
                ContinuousModification::SetToughness { value: 3 },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Trample,
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                },
            ],
        );
        let body = extract_body(&ability);
        assert_eq!(
            body,
            AnimatedBody {
                power: 3,
                toughness: 3,
                keywords: vec![Keyword::Trample],
            }
        );
    }

    #[test]
    fn animation_mana_value_reads_generic_cost() {
        let ability = animate_ability(3, vec![]);
        assert_eq!(animation_mana_value(&ability), Some(3));
    }
}
