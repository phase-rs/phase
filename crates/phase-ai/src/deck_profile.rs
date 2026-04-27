use serde::{Deserialize, Serialize};

use engine::game::DeckEntry;
use engine::types::ability::Effect;
use engine::types::card_type::CoreType;

use crate::eval::EvalWeights;

/// High-level deck strategy classification derived from card composition.
/// Used to adjust evaluation weights, timing policies, and combat aggression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeckArchetype {
    Aggro,
    #[default]
    Midrange,
    Control,
    Combo,
    Ramp,
}

/// Result of deck archetype classification with confidence.
/// `Pure` = strong single-archetype match; `Hybrid` = top two within 20% of each other.
#[derive(Debug, Clone)]
pub enum ArchetypeClassification {
    /// Strong single-archetype match.
    Pure(DeckArchetype),
    /// Ambiguous — top two archetypes are within 20% of each other.
    Hybrid {
        primary: DeckArchetype,
        primary_weight: f64,
        secondary: DeckArchetype,
    },
}

/// Deck composition analysis derived from typed card data.
/// Computed once per game from `GameState.deck_pools`.
#[derive(Debug, Clone)]
pub struct DeckProfile {
    pub classification: ArchetypeClassification,
    /// Convenience: primary archetype regardless of classification.
    pub archetype: DeckArchetype,
    pub avg_mana_value: f64,
    pub creature_ratio: f64,
    pub removal_ratio: f64,
    pub draw_ratio: f64,
    pub ramp_ratio: f64,
}

impl DeckProfile {
    /// Analyze a deck list to derive its archetype and composition ratios.
    /// All detection uses typed `CardFace` data — no string matching.
    pub fn analyze(deck: &[DeckEntry]) -> Self {
        if deck.is_empty() {
            return Self::default();
        }

        let mut nonland_cards = 0u32;
        let mut total_mv = 0u32;
        let mut creatures = 0u32;
        let mut removal = 0u32;
        let mut draw = 0u32;
        let mut ramp = 0u32;

        for entry in deck {
            let card = &entry.card;
            let count = entry.count;

            let is_land = card.card_type.core_types.contains(&CoreType::Land);
            if is_land {
                continue;
            }

            nonland_cards += count;
            total_mv += card.mana_cost.mana_value() * count;

            if card.card_type.core_types.contains(&CoreType::Creature) {
                creatures += count;
            }

            for ability in card.abilities.iter() {
                if is_removal_effect(&ability.effect) {
                    removal += count;
                    break;
                }
            }
            for ability in card.abilities.iter() {
                if is_draw_effect(&ability.effect) {
                    draw += count;
                    break;
                }
            }
            for ability in card.abilities.iter() {
                if is_ramp_effect(&ability.effect) {
                    ramp += count;
                    break;
                }
            }
        }

        let nonland = nonland_cards.max(1) as f64;
        let avg_mana_value = total_mv as f64 / nonland;
        let creature_ratio = creatures as f64 / nonland;
        let removal_ratio = removal as f64 / nonland;
        let draw_ratio = draw as f64 / nonland;
        let ramp_ratio = ramp as f64 / nonland;

        let classification = classify(
            avg_mana_value,
            creature_ratio,
            removal_ratio,
            draw_ratio,
            ramp_ratio,
        );
        let archetype = match &classification {
            ArchetypeClassification::Pure(arch) => *arch,
            ArchetypeClassification::Hybrid { primary, .. } => *primary,
        };

        Self {
            classification,
            archetype,
            avg_mana_value,
            creature_ratio,
            removal_ratio,
            draw_ratio,
            ramp_ratio,
        }
    }

    /// Apply archetype-based multipliers to base evaluation weights.
    /// Returns new weights tuned for this deck's strategy.
    pub fn adjust_weights(&self, base: &EvalWeights) -> EvalWeights {
        self.adjust_weights_with(&ArchetypeMultipliers::default(), base)
    }

    /// Apply custom archetype multipliers to base evaluation weights.
    pub fn adjust_weights_with(
        &self,
        multipliers: &ArchetypeMultipliers,
        base: &EvalWeights,
    ) -> EvalWeights {
        let m = multipliers.for_archetype(self.archetype);
        EvalWeights {
            life: base.life * m[0],
            aggression: base.aggression * m[1],
            board_presence: base.board_presence * m[2],
            board_power: base.board_power * m[3],
            board_toughness: base.board_toughness * m[4],
            hand_size: base.hand_size * m[5],
            zone_quality: base.zone_quality * m[6],
            card_advantage: base.card_advantage * m[7],
            synergy: base.synergy * m[8],
        }
    }
}

impl Default for DeckProfile {
    fn default() -> Self {
        Self {
            classification: ArchetypeClassification::Pure(DeckArchetype::Midrange),
            archetype: DeckArchetype::Midrange,
            avg_mana_value: 0.0,
            creature_ratio: 0.0,
            removal_ratio: 0.0,
            draw_ratio: 0.0,
            ramp_ratio: 0.0,
        }
    }
}

/// Per-archetype weight multipliers (9 values each: life, aggression, presence,
/// power, toughness, hand, zone_quality, card_advantage, synergy).
/// These scale the base EvalWeights to reflect how each archetype values
/// different board dimensions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchetypeMultipliers {
    pub aggro: [f64; 9],
    pub midrange: [f64; 9],
    pub control: [f64; 9],
    pub combo: [f64; 9],
    pub ramp: [f64; 9],
}

impl Default for ArchetypeMultipliers {
    fn default() -> Self {
        Self {
            aggro: [0.6, 2.0, 1.5, 2.0, 0.5, 0.3, 0.15, 0.1, 0.3],
            midrange: [1.0, 1.0, 1.5, 1.2, 1.2, 0.8, 0.3, 0.4, 0.5],
            control: [1.5, 0.3, 0.8, 0.5, 0.8, 2.0, 0.6, 0.8, 0.4],
            combo: [0.8, 0.5, 0.5, 0.5, 0.5, 2.5, 0.5, 0.6, 0.7],
            ramp: [1.0, 0.7, 1.0, 1.0, 1.0, 1.0, 0.4, 0.3, 0.4],
        }
    }
}

impl ArchetypeMultipliers {
    /// Get the 9-element multiplier array for a given archetype.
    pub fn for_archetype(&self, archetype: DeckArchetype) -> &[f64; 9] {
        match archetype {
            DeckArchetype::Aggro => &self.aggro,
            DeckArchetype::Midrange => &self.midrange,
            DeckArchetype::Control => &self.control,
            DeckArchetype::Combo => &self.combo,
            DeckArchetype::Ramp => &self.ramp,
        }
    }
}

/// Score each archetype and return the best match.
/// Returns `Hybrid` when the top two archetypes are within 20% of each other,
/// indicating an ambiguous classification where blending is appropriate.
fn classify(
    avg_mv: f64,
    creature_ratio: f64,
    removal_ratio: f64,
    draw_ratio: f64,
    ramp_ratio: f64,
) -> ArchetypeClassification {
    let aggro_score = (3.5 - avg_mv).max(0.0) + creature_ratio * 2.0 - removal_ratio;
    let control_score =
        (avg_mv - 2.5).max(0.0) + removal_ratio * 2.0 + draw_ratio * 1.5 - creature_ratio;
    let ramp_score = ramp_ratio * 3.0 + (avg_mv - 3.0).max(0.0) * 0.5;
    let combo_score = (1.0 - creature_ratio) * 1.5 + draw_ratio * 1.0 - removal_ratio * 0.5;
    // Midrange is the baseline — it wins when nothing else scores strongly.
    let midrange_score = 1.0;

    let mut scores = [
        (aggro_score, DeckArchetype::Aggro),
        (control_score, DeckArchetype::Control),
        (ramp_score, DeckArchetype::Ramp),
        (combo_score, DeckArchetype::Combo),
        (midrange_score, DeckArchetype::Midrange),
    ];

    scores.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let (best_score, best_arch) = scores[0];
    let (second_score, second_arch) = scores[1];

    // If top two scores are within 20% of each other, classify as hybrid.
    // Guard against zero/negative best_score to avoid division issues.
    if best_score > 0.0 && (best_score - second_score) / best_score < 0.2 {
        let total = best_score + second_score;
        ArchetypeClassification::Hybrid {
            primary: best_arch,
            primary_weight: best_score / total,
            secondary: second_arch,
        }
    } else {
        ArchetypeClassification::Pure(best_arch)
    }
}

fn is_removal_effect(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Destroy { .. } | Effect::DealDamage { .. } | Effect::DestroyAll { .. }
    )
}

fn is_draw_effect(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Draw { .. } | Effect::Scry { .. } | Effect::Surveil { .. }
    )
}

fn is_ramp_effect(effect: &Effect) -> bool {
    matches!(effect, Effect::Mana { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr, TargetFilter};
    use engine::types::card::CardFace;
    use engine::types::card_type::CardType;
    use engine::types::mana::ManaCost;

    fn make_ability(effect: Effect) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Spell, effect)
    }

    fn creature_entry(mv: u32) -> DeckEntry {
        DeckEntry {
            card: CardFace {
                card_type: CardType {
                    core_types: vec![CoreType::Creature],
                    ..Default::default()
                },
                mana_cost: ManaCost::generic(mv),
                ..Default::default()
            },
            count: 4,
        }
    }

    fn removal_entry(mv: u32) -> DeckEntry {
        DeckEntry {
            card: CardFace {
                card_type: CardType {
                    core_types: vec![CoreType::Instant],
                    ..Default::default()
                },
                mana_cost: ManaCost::generic(mv),
                abilities: vec![make_ability(Effect::Destroy {
                    target: TargetFilter::Any,
                    cant_regenerate: false,
                })],
                ..Default::default()
            },
            count: 4,
        }
    }

    fn draw_entry(mv: u32) -> DeckEntry {
        DeckEntry {
            card: CardFace {
                card_type: CardType {
                    core_types: vec![CoreType::Sorcery],
                    ..Default::default()
                },
                mana_cost: ManaCost::generic(mv),
                abilities: vec![make_ability(Effect::Draw {
                    count: QuantityExpr::Fixed { value: 2 },
                    target: engine::types::ability::TargetFilter::Controller,
                })],
                ..Default::default()
            },
            count: 4,
        }
    }

    #[test]
    fn classify_aggro_deck() {
        // 20 cheap creatures + 4 burn = aggro
        let deck = vec![
            creature_entry(1), // 4x 1-drops
            creature_entry(1), // 4x 1-drops
            creature_entry(2), // 4x 2-drops
            creature_entry(2), // 4x 2-drops
            creature_entry(3), // 4x 3-drops
        ];
        let profile = DeckProfile::analyze(&deck);
        assert_eq!(profile.archetype, DeckArchetype::Aggro);
        assert!(profile.avg_mana_value < 2.5);
        assert!(profile.creature_ratio > 0.8);
    }

    #[test]
    fn classify_control_deck() {
        // 8 removal + 8 draw + high curve = control
        let deck = vec![
            removal_entry(2),
            removal_entry(3),
            draw_entry(3),
            draw_entry(4),
            creature_entry(5),
        ];
        let profile = DeckProfile::analyze(&deck);
        assert_eq!(profile.archetype, DeckArchetype::Control);
        assert!(profile.removal_ratio > 0.3);
        assert!(profile.draw_ratio > 0.3);
    }

    #[test]
    fn classify_empty_deck() {
        let profile = DeckProfile::analyze(&[]);
        assert_eq!(profile.archetype, DeckArchetype::Midrange);
    }

    #[test]
    fn adjust_weights_aggro_doubles_aggression() {
        let base = EvalWeights::default();
        let profile = DeckProfile {
            archetype: DeckArchetype::Aggro,
            ..Default::default()
        };
        let adjusted = profile.adjust_weights(&base);
        assert!((adjusted.aggression - base.aggression * 2.0).abs() < f64::EPSILON);
        assert!(adjusted.hand_size < base.hand_size);
    }

    #[test]
    fn adjust_weights_control_doubles_hand_size() {
        let base = EvalWeights::default();
        let profile = DeckProfile {
            archetype: DeckArchetype::Control,
            ..Default::default()
        };
        let adjusted = profile.adjust_weights(&base);
        assert!((adjusted.hand_size - base.hand_size * 2.0).abs() < f64::EPSILON);
        assert!(adjusted.aggression < base.aggression);
    }
}
