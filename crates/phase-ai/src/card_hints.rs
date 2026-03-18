use engine::game::players;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use crate::eval::{evaluate_creature, threat_level};

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

            // Categorize by typed ability effects
            let has_destroy = obj
                .abilities
                .iter()
                .any(|a| matches!(a.effect, Effect::Destroy { .. }));
            let has_damage = obj
                .abilities
                .iter()
                .any(|a| matches!(a.effect, Effect::DealDamage { .. }));
            let has_pump = obj
                .abilities
                .iter()
                .any(|a| matches!(a.effect, Effect::Pump { .. }));
            let has_counter = obj
                .abilities
                .iter()
                .any(|a| matches!(a.effect, Effect::Counter { .. }));

            // Removal: higher priority when opponents have high-value creatures.
            // In multiplayer, prefer targeting highest-threat opponent's best creature.
            if has_destroy || has_damage {
                let opponents = players::opponents(state, player);
                let max_threat = state
                    .battlefield
                    .iter()
                    .filter_map(|&id| {
                        let o = state.objects.get(&id)?;
                        if opponents.contains(&o.controller)
                            && o.card_types.core_types.contains(&CoreType::Creature)
                        {
                            let creature_val = evaluate_creature(state, id);
                            // Weight by controller's threat level for multi-opponent focus
                            let threat_weight = threat_level(state, player, o.controller) + 0.5;
                            Some(creature_val * threat_weight)
                        } else {
                            None
                        }
                    })
                    .fold(0.0_f64, f64::max);

                // Scale 0.5-0.9 based on threat-weighted creature value
                return (0.5 + (max_threat / 30.0).min(0.4)).min(0.9);
            }

            // Combat tricks: highest during combat
            if has_pump {
                return if is_combat { 0.9 } else { 0.3 };
            }

            // Counterspells: only worth casting if there's something on the stack
            if has_counter {
                return if !is_own_turn && !state.stack.is_empty() {
                    0.8
                } else {
                    0.1
                };
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
    use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, TargetFilter};
    use engine::types::card_type::CoreType;
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;
    use engine::types::zones::Zone;

    fn make_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state
    }

    fn make_ability(effect: Effect) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Spell, effect)
    }

    fn add_spell_to_hand(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        core_type: CoreType,
        abilities: Vec<AbilityDefinition>,
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
        obj.abilities = abilities;
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
        let card_id = add_spell_to_hand(
            &mut state,
            PlayerId(0),
            "Murder",
            CoreType::Instant,
            vec![make_ability(Effect::Destroy {
                target: TargetFilter::Any,
            })],
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
        let card_id = add_spell_to_hand(
            &mut state,
            PlayerId(0),
            "Counterspell",
            CoreType::Instant,
            vec![make_ability(Effect::Counter {
                target: TargetFilter::Any,
                source_static: None,
                unless_payment: None,
            })],
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
    fn counterspells_score_high_on_opponent_turn_with_stack() {
        use engine::types::ability::ResolvedAbility;
        use engine::types::game_state::{StackEntry, StackEntryKind};
        use engine::types::identifiers::ObjectId;

        let mut state = make_state();
        state.active_player = PlayerId(1); // Opponent's turn
                                           // Put something on the stack so the counterspell has a target
        state.stack.push(StackEntry {
            id: ObjectId(999),
            source_id: ObjectId(998),
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(500),
                ability: ResolvedAbility::new(
                    Effect::Draw {
                        count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                    },
                    Vec::new(),
                    ObjectId(998),
                    PlayerId(1),
                ),
                cast_as_adventure: false,
            },
        });
        let card_id = add_spell_to_hand(
            &mut state,
            PlayerId(0),
            "Counterspell",
            CoreType::Instant,
            vec![make_ability(Effect::Counter {
                target: TargetFilter::Any,
                source_static: None,
                unless_payment: None,
            })],
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
            "Counterspell should score high on opponent turn with stack, got {score}"
        );
    }

    #[test]
    fn counterspells_score_low_with_empty_stack() {
        let mut state = make_state();
        state.active_player = PlayerId(1); // Opponent's turn, but stack is empty
        let card_id = add_spell_to_hand(
            &mut state,
            PlayerId(0),
            "Counterspell",
            CoreType::Instant,
            vec![make_ability(Effect::Counter {
                target: TargetFilter::Any,
                source_static: None,
                unless_payment: None,
            })],
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
            score <= 0.5,
            "Counterspell should score low with empty stack, got {score}"
        );
    }

    #[test]
    fn creatures_prefer_precombat_main() {
        let mut state = make_state();
        let card_id =
            add_spell_to_hand(&mut state, PlayerId(0), "Bear", CoreType::Creature, vec![]);

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
