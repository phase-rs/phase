use engine::game::players;
use engine::types::ability::Effect;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::deck_knowledge::{remaining_deck_view, RemainingDeckView};
use crate::deck_profile::{DeckArchetype, DeckProfile};
use crate::zone_eval::available_mana;

/// Categories of threats an opponent might hold. Each has a computed probability.
/// Uses a flat struct (not HashMap) for zero-alloc, no-hash, self-documenting access.
#[derive(Debug, Clone, Default)]
pub struct ThreatProbabilities {
    pub counterspell: f64,
    pub targeted_removal: f64,
    pub board_wipe: f64,
    pub combat_trick: f64,
    pub direct_damage: f64,
}

/// Per-category counts and mana costs, precomputed at build time.
/// Stores enough information to recompute mana-gated probabilities
/// at eval time without re-accessing the raw deck view.
#[derive(Debug, Clone, Default)]
pub struct ThreatCategoryPools {
    pub counterspell: CategoryPool,
    pub targeted_removal: CategoryPool,
    pub board_wipe: CategoryPool,
    pub combat_trick: CategoryPool,
    pub direct_damage: CategoryPool,
}

#[derive(Debug, Clone, Default)]
pub struct CategoryPool {
    pub count: u32,
    pub min_mana_value: u32,
    /// Counts by mana cost bucket. Index = mana value, value = count at that cost.
    /// Index 7 groups everything with mana value 7+.
    pub by_mana_cost: [u32; 8],
}

impl CategoryPool {
    fn add(&mut self, mana_value: u32, count: u32) {
        self.count += count;
        let bucket = (mana_value as usize).min(7);
        self.by_mana_cost[bucket] += count;
        if self.count == count {
            // First card added — initialize min
            self.min_mana_value = mana_value;
        } else {
            self.min_mana_value = self.min_mana_value.min(mana_value);
        }
    }

    /// Count of cards castable with at most `max_mana` available.
    fn castable_count(&self, max_mana: u32) -> u32 {
        let limit = (max_mana as usize).min(7);
        self.by_mana_cost[..=limit].iter().sum()
    }
}

/// Opponent threat profile: probabilities that the opponent's hand contains
/// at least one card of each threat category, plus precomputed pool data
/// for just-in-time mana-gated recalculation.
#[derive(Debug, Clone)]
pub struct ThreatProfile {
    /// P(at least 1 card of this category in opponent's hand).
    pub probabilities: ThreatProbabilities,
    /// Opponent's deck archetype (from their remaining pool).
    pub opponent_archetype: DeckArchetype,
    /// Per-category pool counts and mana-value distributions.
    pub category_pools: ThreatCategoryPools,
    /// Total cards remaining in opponent's pool.
    pub pool_size: u32,
    /// Opponent's current hand size.
    pub hand_size: u32,
}

/// P(at least 1 success in sample) = 1 - C(N-K, n) / C(N, n)
/// where N=pool_size, K=threat_count, n=hand_size.
///
/// Uses iterative ratio computation (not factorial) to avoid overflow:
/// C(N-K, n) / C(N, n) = product_{i=0..n-1} (N-K-i) / (N-i)
pub fn hypergeometric_at_least_one(pool_size: u32, threat_count: u32, hand_size: u32) -> f64 {
    if pool_size == 0 || hand_size == 0 {
        return 0.0;
    }
    let threat_count = threat_count.min(pool_size);
    if threat_count >= pool_size {
        return 1.0;
    }
    if hand_size >= pool_size {
        return 1.0;
    }

    // Compute P(0 successes) = product_{i=0..n-1} (N-K-i) / (N-i)
    let n = pool_size as f64;
    let k = threat_count as f64;
    let mut p_zero = 1.0;
    for i in 0..hand_size {
        let fi = i as f64;
        p_zero *= (n - k - fi) / (n - fi);
        // Early exit if probability drops to zero
        if p_zero <= 0.0 {
            return 1.0;
        }
    }

    (1.0 - p_zero).clamp(0.0, 1.0)
}

/// Classify a card face into threat categories by examining its abilities.
/// Returns a `ThreatProbabilities` with 1.0 for each matched category.
/// A card can belong to multiple categories (e.g., Lightning Bolt is both
/// targeted_removal and direct_damage).
///
/// Walks the full `sub_ability` chain on each ability to catch compound effects
/// (e.g., Destroy + Draw, exile via ChangeZone in a sub_ability).
pub fn classify_card_face(card: &CardFace) -> ThreatProbabilities {
    let mut probs = ThreatProbabilities::default();
    let is_instant = card.card_type.core_types.contains(&CoreType::Instant);

    for ability in &card.abilities {
        classify_ability(&mut probs, ability, is_instant);
    }
    probs
}

/// Classify a single ability and its sub_ability chain into threat categories.
fn classify_ability(
    probs: &mut ThreatProbabilities,
    ability: &engine::types::ability::AbilityDefinition,
    is_instant: bool,
) {
    classify_effect(probs, ability.effect.as_ref(), is_instant);
    if let Some(sub) = &ability.sub_ability {
        classify_ability(probs, sub, is_instant);
    }
}

/// Classify a single effect into threat categories.
fn classify_effect(probs: &mut ThreatProbabilities, effect: &Effect, is_instant: bool) {
    match effect {
        Effect::Counter { .. } => probs.counterspell = 1.0,
        Effect::DestroyAll { .. } | Effect::DamageAll { .. } => probs.board_wipe = 1.0,
        Effect::Destroy { .. } => probs.targeted_removal = 1.0,
        Effect::DealDamage { target, .. } => {
            // DealDamage with Any target can hit creatures (removal) or players (burn)
            probs.targeted_removal = 1.0;
            if matches!(target, engine::types::ability::TargetFilter::Any) {
                probs.direct_damage = 1.0;
            }
        }
        Effect::Pump { .. } if is_instant => probs.combat_trick = 1.0,
        Effect::ChangeZone {
            destination: engine::types::zones::Zone::Exile | engine::types::zones::Zone::Graveyard,
            ..
        } => probs.targeted_removal = 1.0,
        Effect::Bounce { .. } => probs.targeted_removal = 1.0,
        _ => {}
    }
}

/// Build a full threat profile from the opponent's remaining deck view.
pub fn build_threat_profile(
    state: &GameState,
    opponent: PlayerId,
    deck_view: &RemainingDeckView,
) -> ThreatProfile {
    let pool_size: u32 = deck_view.entries.iter().map(|e| e.count).sum();
    let hand_size = state.players[opponent.0 as usize].hand.len() as u32;

    let mut pools = ThreatCategoryPools::default();

    for entry in &deck_view.entries {
        let classification = classify_card_face(&entry.card);
        let mv = entry.card.mana_cost.mana_value();
        let count = entry.count;

        if classification.counterspell > 0.0 {
            pools.counterspell.add(mv, count);
        }
        if classification.targeted_removal > 0.0 {
            pools.targeted_removal.add(mv, count);
        }
        if classification.board_wipe > 0.0 {
            pools.board_wipe.add(mv, count);
        }
        if classification.combat_trick > 0.0 {
            pools.combat_trick.add(mv, count);
        }
        if classification.direct_damage > 0.0 {
            pools.direct_damage.add(mv, count);
        }
    }

    let opponent_archetype = DeckProfile::analyze(&deck_view.entries).archetype;

    let probabilities = ThreatProbabilities {
        counterspell: hypergeometric_at_least_one(pool_size, pools.counterspell.count, hand_size),
        targeted_removal: hypergeometric_at_least_one(
            pool_size,
            pools.targeted_removal.count,
            hand_size,
        ),
        board_wipe: hypergeometric_at_least_one(pool_size, pools.board_wipe.count, hand_size),
        combat_trick: hypergeometric_at_least_one(pool_size, pools.combat_trick.count, hand_size),
        direct_damage: hypergeometric_at_least_one(pool_size, pools.direct_damage.count, hand_size),
    };

    ThreatProfile {
        probabilities,
        opponent_archetype,
        category_pools: pools,
        pool_size,
        hand_size,
    }
}

/// Compute mana-gated threat probabilities for the current game state.
/// Called during evaluation (not at profile build time) so it reflects
/// the actual available mana in the state being searched.
///
/// Uses the precomputed `ThreatCategoryPools` — no need to re-iterate the card pool.
pub fn castable_probabilities(
    profile: &ThreatProfile,
    state: &GameState,
    opponent: PlayerId,
) -> ThreatProbabilities {
    let opp_mana = available_mana(state, opponent);

    ThreatProbabilities {
        counterspell: hypergeometric_at_least_one(
            profile.pool_size,
            profile.category_pools.counterspell.castable_count(opp_mana),
            profile.hand_size,
        ),
        targeted_removal: hypergeometric_at_least_one(
            profile.pool_size,
            profile
                .category_pools
                .targeted_removal
                .castable_count(opp_mana),
            profile.hand_size,
        ),
        board_wipe: hypergeometric_at_least_one(
            profile.pool_size,
            profile.category_pools.board_wipe.castable_count(opp_mana),
            profile.hand_size,
        ),
        combat_trick: hypergeometric_at_least_one(
            profile.pool_size,
            profile.category_pools.combat_trick.castable_count(opp_mana),
            profile.hand_size,
        ),
        direct_damage: hypergeometric_at_least_one(
            profile.pool_size,
            profile
                .category_pools
                .direct_damage
                .castable_count(opp_mana),
            profile.hand_size,
        ),
    }
}

/// Build threat profile for the highest-threat opponent in multiplayer.
/// "Highest threat" = most untapped mana AND most remaining cards.
pub fn build_threat_profile_multiplayer(
    state: &GameState,
    ai_player: PlayerId,
) -> Option<ThreatProfile> {
    let opponents = players::opponents(state, ai_player);
    if opponents.is_empty() {
        return None;
    }

    // Pick the opponent with the most untapped mana, breaking ties by hand size.
    let primary = opponents.iter().copied().max_by_key(|&opp| {
        let mana = available_mana(state, opp);
        let hand = state.players[opp.0 as usize].hand.len() as u32;
        (mana, hand)
    })?;

    let deck_view = remaining_deck_view(state, primary);
    if deck_view.entries.is_empty() {
        return None;
    }
    Some(build_threat_profile(state, primary, &deck_view))
}

/// Base threat probabilities by opponent archetype, used for Medium difficulty
/// where per-card analysis is skipped. Values are conservative estimates.
pub struct ArchetypeBaseProbabilities;

impl ArchetypeBaseProbabilities {
    pub fn for_archetype(archetype: DeckArchetype) -> ThreatProbabilities {
        match archetype {
            DeckArchetype::Aggro => ThreatProbabilities {
                counterspell: 0.0,
                targeted_removal: 0.2,
                board_wipe: 0.0,
                combat_trick: 0.3,
                direct_damage: 0.3,
            },
            DeckArchetype::Control => ThreatProbabilities {
                counterspell: 0.3,
                targeted_removal: 0.3,
                board_wipe: 0.2,
                combat_trick: 0.0,
                direct_damage: 0.1,
            },
            DeckArchetype::Midrange => ThreatProbabilities {
                counterspell: 0.1,
                targeted_removal: 0.3,
                board_wipe: 0.1,
                combat_trick: 0.15,
                direct_damage: 0.1,
            },
            DeckArchetype::Combo => ThreatProbabilities {
                counterspell: 0.2,
                targeted_removal: 0.1,
                board_wipe: 0.1,
                combat_trick: 0.0,
                direct_damage: 0.0,
            },
            DeckArchetype::Ramp => ThreatProbabilities {
                counterspell: 0.0,
                targeted_removal: 0.2,
                board_wipe: 0.2,
                combat_trick: 0.0,
                direct_damage: 0.1,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr, TargetFilter};
    use engine::types::card::CardFace;
    use engine::types::card_type::CardType;

    fn make_ability(effect: Effect) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Spell, effect)
    }

    // ── Hypergeometric math ──────────────────────────────────────────────

    #[test]
    fn hypergeometric_basic() {
        // 4 counters in 30-card pool, hand of 5 → ~0.52
        let p = hypergeometric_at_least_one(30, 4, 5);
        assert!((0.48..=0.56).contains(&p), "Expected ~0.52, got {p}");
    }

    #[test]
    fn hypergeometric_empty_pool() {
        assert_eq!(hypergeometric_at_least_one(0, 4, 5), 0.0);
    }

    #[test]
    fn hypergeometric_zero_hand() {
        assert_eq!(hypergeometric_at_least_one(30, 4, 0), 0.0);
    }

    #[test]
    fn hypergeometric_pool_smaller_than_hand() {
        // All cards are in hand
        assert_eq!(hypergeometric_at_least_one(3, 1, 5), 1.0);
    }

    #[test]
    fn hypergeometric_threat_exceeds_pool() {
        // Clamp: threat_count > pool_size → 1.0
        assert_eq!(hypergeometric_at_least_one(5, 10, 3), 1.0);
    }

    #[test]
    fn hypergeometric_all_threats() {
        // Every card is a threat → guaranteed hit
        assert_eq!(hypergeometric_at_least_one(10, 10, 3), 1.0);
    }

    #[test]
    fn hypergeometric_no_threats() {
        assert_eq!(hypergeometric_at_least_one(30, 0, 5), 0.0);
    }

    #[test]
    fn hypergeometric_large_pool_no_overflow() {
        // 60-card pool, 8 threats, 7-card hand — should compute without overflow
        let p = hypergeometric_at_least_one(60, 8, 7);
        assert!(p > 0.0 && p < 1.0, "Should be a valid probability, got {p}");
        // Rough expected: ~0.62
        assert!(
            (0.55..=0.70).contains(&p),
            "Expected ~0.62 for 8/60 pool with 7-card hand, got {p}"
        );
    }

    #[test]
    fn hypergeometric_single_threat_single_draw() {
        // 1 threat in 10 cards, draw 1 → exactly 0.1
        let p = hypergeometric_at_least_one(10, 1, 1);
        assert!((p - 0.1).abs() < 0.001, "Expected 0.1, got {p}");
    }

    // ── Card classification ──────────────────────────────────────────────

    #[test]
    fn classify_counter_spell() {
        let card = CardFace {
            card_type: CardType {
                core_types: vec![CoreType::Instant],
                ..Default::default()
            },
            abilities: vec![make_ability(Effect::Counter {
                target: TargetFilter::Any,
                source_static: None,
                unless_payment: None,
            })],
            ..Default::default()
        };
        let probs = classify_card_face(&card);
        assert_eq!(probs.counterspell, 1.0);
    }

    #[test]
    fn classify_board_wipe() {
        let card = CardFace {
            card_type: CardType {
                core_types: vec![CoreType::Sorcery],
                ..Default::default()
            },
            abilities: vec![make_ability(Effect::DestroyAll {
                target: TargetFilter::Any,
                cant_regenerate: false,
            })],
            ..Default::default()
        };
        let probs = classify_card_face(&card);
        assert_eq!(probs.board_wipe, 1.0);
    }

    #[test]
    fn classify_burn_spell() {
        let card = CardFace {
            card_type: CardType {
                core_types: vec![CoreType::Instant],
                ..Default::default()
            },
            abilities: vec![make_ability(Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            })],
            ..Default::default()
        };
        let probs = classify_card_face(&card);
        assert_eq!(probs.targeted_removal, 1.0);
        assert_eq!(probs.direct_damage, 1.0);
    }

    #[test]
    fn classify_instant_pump_is_combat_trick() {
        let card = CardFace {
            card_type: CardType {
                core_types: vec![CoreType::Instant],
                ..Default::default()
            },
            abilities: vec![make_ability(Effect::Pump {
                power: engine::types::ability::PtValue::Fixed(3),
                toughness: engine::types::ability::PtValue::Fixed(3),
                target: TargetFilter::Any,
            })],
            ..Default::default()
        };
        let probs = classify_card_face(&card);
        assert_eq!(probs.combat_trick, 1.0);
    }

    #[test]
    fn classify_sorcery_pump_not_combat_trick() {
        let card = CardFace {
            card_type: CardType {
                core_types: vec![CoreType::Sorcery],
                ..Default::default()
            },
            abilities: vec![make_ability(Effect::Pump {
                power: engine::types::ability::PtValue::Fixed(3),
                toughness: engine::types::ability::PtValue::Fixed(3),
                target: TargetFilter::Any,
            })],
            ..Default::default()
        };
        let probs = classify_card_face(&card);
        assert_eq!(probs.combat_trick, 0.0);
    }

    // ── Castable gating ──────────────────────────────────────────────────

    #[test]
    fn castable_gating_filters_by_mana() {
        let mut pools = ThreatCategoryPools::default();
        // 4 board wipes at mana value 4
        pools.board_wipe.add(4, 4);

        let profile = ThreatProfile {
            probabilities: ThreatProbabilities::default(),
            opponent_archetype: DeckArchetype::Control,
            category_pools: pools,
            pool_size: 30,
            hand_size: 5,
        };

        // With only 3 mana: no castable wipes → probability 0
        assert_eq!(profile.category_pools.board_wipe.castable_count(3), 0);

        // With 4 mana: all 4 wipes castable
        assert_eq!(profile.category_pools.board_wipe.castable_count(4), 4);

        // With 10 mana: still 4
        assert_eq!(profile.category_pools.board_wipe.castable_count(10), 4);
    }

    // ── ArchetypeOnly mode ───────────────────────────────────────────────

    #[test]
    fn archetype_only_all_variants() {
        let archetypes = [
            DeckArchetype::Aggro,
            DeckArchetype::Control,
            DeckArchetype::Midrange,
            DeckArchetype::Combo,
            DeckArchetype::Ramp,
        ];
        for arch in &archetypes {
            let probs = ArchetypeBaseProbabilities::for_archetype(*arch);
            // All probabilities should be in [0, 1]
            assert!((0.0..=1.0).contains(&probs.counterspell));
            assert!((0.0..=1.0).contains(&probs.targeted_removal));
            assert!((0.0..=1.0).contains(&probs.board_wipe));
            assert!((0.0..=1.0).contains(&probs.combat_trick));
            assert!((0.0..=1.0).contains(&probs.direct_damage));
        }

        // Control should have counterspells, aggro should not
        let control = ArchetypeBaseProbabilities::for_archetype(DeckArchetype::Control);
        let aggro = ArchetypeBaseProbabilities::for_archetype(DeckArchetype::Aggro);
        assert!(control.counterspell > aggro.counterspell);
        assert!(aggro.combat_trick > control.combat_trick);
    }
}
