use engine::types::ability::effect_variant_name;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use crate::eval::evaluate_creature;

/// Returns a priority score (0.0-1.0) indicating how urgently a card should be played now.
///
/// Higher scores mean the card should be played sooner. This helps the AI
/// decide play order when multiple actions are available.
pub fn should_play_now(state: &GameState, action: &GameAction, player: PlayerId) -> f64 {
    match action {
        GameAction::PlayLand { .. } => 1.0, // Always play lands

        GameAction::CastSpell { card_id, .. } => {
            // Find the object in hand matching this card_id
            let p = &state.players[player.0 as usize];
            let obj = p.hand.iter().find_map(|&oid| {
                let o = state.objects.get(&oid)?;
                if o.card_id == *card_id {
                    Some(o)
                } else {
                    None
                }
            });

            let obj = match obj {
                Some(o) => o,
                None => return 0.5, // Default priority
            };

            let is_combat = matches!(
                state.phase,
                Phase::BeginCombat
                    | Phase::DeclareAttackers
                    | Phase::DeclareBlockers
                    | Phase::CombatDamage
            );
            let is_own_turn = state.active_player == player;

            // Categorize by abilities/svars
            let has_destroy = obj.svars.values().any(|v| v.contains("Destroy"))
                || obj
                    .abilities
                    .iter()
                    .any(|a| effect_variant_name(&a.effect) == "Destroy");
            let has_damage = obj.svars.values().any(|v| v.contains("DealDamage"))
                || obj
                    .abilities
                    .iter()
                    .any(|a| effect_variant_name(&a.effect) == "DealDamage");
            let has_pump = obj.svars.values().any(|v| v.contains("Pump"))
                || obj
                    .abilities
                    .iter()
                    .any(|a| effect_variant_name(&a.effect) == "Pump");
            let has_counter = obj.svars.values().any(|v| v.contains("Counter"))
                || obj
                    .abilities
                    .iter()
                    .any(|a| effect_variant_name(&a.effect) == "Counter");

            // Removal: higher priority when opponent has high-value creatures
            if has_destroy || has_damage {
                let opponent = PlayerId(1 - player.0);
                let max_threat = state
                    .battlefield
                    .iter()
                    .filter(|&&id| {
                        state
                            .objects
                            .get(&id)
                            .map(|o| {
                                o.controller == opponent
                                    && o.card_types.core_types.contains(&CoreType::Creature)
                            })
                            .unwrap_or(false)
                    })
                    .map(|&id| evaluate_creature(state, id))
                    .fold(0.0_f64, f64::max);

                // Scale 0.5-0.9 based on threat level
                return (0.5 + (max_threat / 20.0).min(0.4)).min(0.9);
            }

            // Combat tricks: highest during combat
            if has_pump {
                return if is_combat { 0.9 } else { 0.3 };
            }

            // Counterspells: hold for opponent's turn
            if has_counter {
                return if !is_own_turn { 0.8 } else { 0.1 };
            }

            // Creatures: prefer main phase 1
            if obj.card_types.core_types.contains(&CoreType::Creature) {
                return if matches!(state.phase, Phase::PreCombatMain) {
                    0.7
                } else {
                    0.5
                };
            }

            0.5 // Default for other spells
        }

        _ => 0.5, // Non-spell actions get neutral priority
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::card_type::CoreType;
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;
    use engine::types::zones::Zone;
    use std::collections::HashMap;

    fn make_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state
    }

    fn add_spell_to_hand(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        core_type: CoreType,
        svars: HashMap<String, String>,
    ) -> CardId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Hand,
        );
        let card_id = state.objects.get(&id).unwrap().card_id;
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(core_type);
        obj.mana_cost = ManaCost::zero();
        obj.svars = svars;
        card_id
    }

    #[test]
    fn lands_always_max_priority() {
        let state = make_state();
        let score = should_play_now(
            &state,
            &GameAction::PlayLand { card_id: CardId(1) },
            PlayerId(0),
        );
        assert_eq!(score, 1.0);
    }

    #[test]
    fn removal_scores_higher_with_opponent_creatures() {
        let mut state = make_state();
        let mut svars = HashMap::new();
        svars.insert("Mode".to_string(), "Destroy".to_string());
        let card_id = add_spell_to_hand(
            &mut state,
            PlayerId(0),
            "Murder",
            CoreType::Instant,
            svars.clone(),
        );

        // No opponent creatures
        let score_empty = should_play_now(
            &state,
            &GameAction::CastSpell {
                card_id,
                targets: Vec::new(),
            },
            PlayerId(0),
        );

        // Add opponent creature
        let next_id = state.next_object_id;
        let creature_id = create_object(
            &mut state,
            CardId(next_id),
            PlayerId(1),
            "Dragon".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(5);
        obj.toughness = Some(5);

        let score_with_creature = should_play_now(
            &state,
            &GameAction::CastSpell {
                card_id,
                targets: Vec::new(),
            },
            PlayerId(0),
        );

        assert!(
            score_with_creature > score_empty,
            "Removal should be higher priority when opponent has creatures"
        );
    }

    #[test]
    fn counterspells_score_low_on_own_turn() {
        let mut state = make_state();
        let mut svars = HashMap::new();
        svars.insert("Mode".to_string(), "Counter".to_string());
        let card_id = add_spell_to_hand(
            &mut state,
            PlayerId(0),
            "Counterspell",
            CoreType::Instant,
            svars,
        );

        let score = should_play_now(
            &state,
            &GameAction::CastSpell {
                card_id,
                targets: Vec::new(),
            },
            PlayerId(0),
        );
        assert!(
            score < 0.3,
            "Counterspell should score low on own turn, got {score}"
        );
    }

    #[test]
    fn counterspells_score_high_on_opponent_turn() {
        let mut state = make_state();
        state.active_player = PlayerId(1); // Opponent's turn
        let mut svars = HashMap::new();
        svars.insert("Mode".to_string(), "Counter".to_string());
        let card_id = add_spell_to_hand(
            &mut state,
            PlayerId(0),
            "Counterspell",
            CoreType::Instant,
            svars,
        );

        let score = should_play_now(
            &state,
            &GameAction::CastSpell {
                card_id,
                targets: Vec::new(),
            },
            PlayerId(0),
        );
        assert!(
            score > 0.5,
            "Counterspell should score high on opponent turn, got {score}"
        );
    }

    #[test]
    fn creatures_prefer_precombat_main() {
        let mut state = make_state();
        let card_id = add_spell_to_hand(
            &mut state,
            PlayerId(0),
            "Bear",
            CoreType::Creature,
            HashMap::new(),
        );

        let score_pre = should_play_now(
            &state,
            &GameAction::CastSpell {
                card_id,
                targets: Vec::new(),
            },
            PlayerId(0),
        );

        state.phase = Phase::PostCombatMain;
        let score_post = should_play_now(
            &state,
            &GameAction::CastSpell {
                card_id,
                targets: Vec::new(),
            },
            PlayerId(0),
        );

        assert!(
            score_pre > score_post,
            "Creatures should prefer pre-combat main"
        );
    }
}
