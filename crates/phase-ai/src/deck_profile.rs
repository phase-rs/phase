use engine::game::DeckEntry;
use engine::types::ability::Effect;
use engine::types::card_type::CoreType;

use crate::eval::EvalWeights;

/// High-level deck strategy classification derived from card composition.
/// Used to adjust evaluation weights, timing policies, and combat aggression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckArchetype {
    Aggro,
    Midrange,
    Control,
    Combo,
    Ramp,
}

/// Deck composition analysis derived from typed card data.
/// Computed once per game from `GameState.deck_pools`.
#[derive(Debug, Clone)]
pub struct DeckProfile {
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

            for ability in &card.abilities {
                if is_removal_effect(&ability.effect) {
                    removal += count;
                    break;
                }
            }
            for ability in &card.abilities {
                if is_draw_effect(&ability.effect) {
                    draw += count;
                    break;
                }
            }
            for ability in &card.abilities {
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

        let archetype = classify(
            avg_mana_value,
            creature_ratio,
            removal_ratio,
            draw_ratio,
            ramp_ratio,
        );

        Self {
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
        let (life, aggression, presence, power, toughness, hand, zone, card_adv, syn) =
            match self.archetype {
                DeckArchetype::Aggro => (0.6, 2.0, 1.5, 2.0, 0.5, 0.3, 0.15, 0.1, 0.3),
                DeckArchetype::Midrange => (1.0, 1.0, 1.5, 1.2, 1.2, 0.8, 0.3, 0.4, 0.5),
                DeckArchetype::Control => (1.5, 0.3, 0.8, 0.5, 0.8, 2.0, 0.6, 0.8, 0.4),
                DeckArchetype::Combo => (0.8, 0.5, 0.5, 0.5, 0.5, 2.5, 0.5, 0.6, 0.7),
                DeckArchetype::Ramp => (1.0, 0.7, 1.0, 1.0, 1.0, 1.0, 0.4, 0.3, 0.4),
            };

        EvalWeights {
            life: base.life * life,
            aggression: base.aggression * aggression,
            board_presence: base.board_presence * presence,
            board_power: base.board_power * power,
            board_toughness: base.board_toughness * toughness,
            hand_size: base.hand_size * hand,
            zone_quality: zone,
            card_advantage: card_adv,
            synergy: syn,
        }
    }
}

impl Default for DeckProfile {
    fn default() -> Self {
        Self {
            archetype: DeckArchetype::Midrange,
            avg_mana_value: 0.0,
            creature_ratio: 0.0,
            removal_ratio: 0.0,
            draw_ratio: 0.0,
            ramp_ratio: 0.0,
        }
    }
}

/// Score each archetype and return the best match.
fn classify(
    avg_mv: f64,
    creature_ratio: f64,
    removal_ratio: f64,
    draw_ratio: f64,
    ramp_ratio: f64,
) -> DeckArchetype {
    let aggro_score = (3.5 - avg_mv).max(0.0) + creature_ratio * 2.0 - removal_ratio;
    let control_score =
        (avg_mv - 2.5).max(0.0) + removal_ratio * 2.0 + draw_ratio * 1.5 - creature_ratio;
    let ramp_score = ramp_ratio * 3.0 + (avg_mv - 3.0).max(0.0) * 0.5;
    let combo_score = (1.0 - creature_ratio) * 1.5 + draw_ratio * 1.0 - removal_ratio * 0.5;
    // Midrange is the baseline — it wins when nothing else scores strongly.
    let midrange_score = 1.0;

    let scores = [
        (aggro_score, DeckArchetype::Aggro),
        (control_score, DeckArchetype::Control),
        (ramp_score, DeckArchetype::Ramp),
        (combo_score, DeckArchetype::Combo),
        (midrange_score, DeckArchetype::Midrange),
    ];

    scores
        .into_iter()
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, arch)| arch)
        .unwrap_or(DeckArchetype::Midrange)
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
