use engine::game::players;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use crate::cast_facts::{cast_facts_for_action, CastFacts};
use crate::eval::{evaluate_creature, threat_level};
use crate::policies::hand_disruption::disruption_window_score;

/// Returns a priority score (0.0-1.0) indicating how urgently a card should be played now.
///
/// Higher scores mean the card should be played sooner. This helps the AI
/// decide play order when multiple actions are available.
pub fn should_play_now(state: &GameState, action: &GameAction, player: PlayerId) -> f64 {
    let cast_facts = cast_facts_for_action(state, action, player);
    should_play_now_with_facts(state, action, player, cast_facts.as_ref())
}

pub(crate) fn should_play_now_with_facts(
    state: &GameState,
    action: &GameAction,
    player: PlayerId,
    cast_facts: Option<&CastFacts<'_>>,
) -> f64 {
    match action {
        GameAction::PlayLand { .. } => 1.0, // Always play lands

        GameAction::CastSpell { .. } => {
            let facts = match cast_facts {
                Some(facts) => facts,
                None => return 0.5, // Default priority
            };
            let obj = facts.object;

            let is_combat = matches!(
                state.phase,
                Phase::BeginCombat
                    | Phase::DeclareAttackers
                    | Phase::DeclareBlockers
                    | Phase::CombatDamage
            );
            let is_own_turn = engine::game::turn_control::turn_decision_maker(state) == player;
            let immediate_effects = facts.immediate_effects();
            let has_destroy = immediate_effects
                .iter()
                .any(|effect| matches!(effect, Effect::Destroy { .. }));
            let has_damage = immediate_effects
                .iter()
                .any(|effect| matches!(effect, Effect::DealDamage { .. }));
            let has_pump = immediate_effects
                .iter()
                .any(|effect| matches!(effect, Effect::Pump { .. }));
            let has_counter = immediate_effects
                .iter()
                .any(|effect| matches!(effect, Effect::Counter { .. }));
            let has_etb_value = !facts.immediate_etb_triggers.is_empty()
                || !facts.immediate_replacements.is_empty();

            // Removal: higher priority when opponents have high-value creatures.
            // In multiplayer, prefer targeting highest-threat opponent's best creature.
            if has_destroy || has_damage || (facts.has_direct_removal_text() && has_etb_value) {
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

            // Combat tricks: highest during combat, near-zero during end/cleanup
            // (pump effects expire at cleanup — casting then has no lasting impact)
            if has_pump {
                return if is_combat {
                    0.9
                } else if matches!(state.phase, Phase::End | Phase::Cleanup) {
                    0.05
                } else {
                    0.3
                };
            }

            // Counterspells: only worth casting if there's something on the stack
            if has_counter {
                return if !is_own_turn && !state.stack.is_empty() {
                    0.8
                } else {
                    0.1
                };
            }

            if facts.has_search_library() {
                let proactive = if matches!(state.phase, Phase::PreCombatMain) {
                    0.72
                } else {
                    0.58
                };
                return if is_own_turn { proactive } else { 0.45 };
            }

            if facts.has_reveal_hand_or_discard() {
                return disruption_window_score(state, player, facts)
                    .map(|window| window.hint_priority)
                    .unwrap_or(0.18);
            }

            if facts.has_draw() && facts.mana_value >= 3 {
                return if matches!(state.phase, Phase::PreCombatMain) {
                    0.68
                } else {
                    0.56
                };
            }

            // Creatures: prefer main phase 1
            if obj.card_types.core_types.contains(&CoreType::Creature) {
                let keyword_bonus = creature_keyword_bonus(obj);
                let stat_bonus = creature_stat_bonus(obj);
                let etb_bonus = if has_etb_value { 0.08 } else { 0.0 };
                return if matches!(state.phase, Phase::PreCombatMain) {
                    (0.62 + keyword_bonus + stat_bonus + etb_bonus).min(0.85)
                } else {
                    (0.48 + keyword_bonus * 0.5 + stat_bonus * 0.5 + etb_bonus * 0.5).min(0.7)
                };
            }

            if facts.is_planeswalker() || facts.is_enchantment() || facts.has_token_creation() {
                return if matches!(state.phase, Phase::PreCombatMain) {
                    0.66
                } else {
                    0.54
                };
            }

            0.5 // Default for other spells
        }

        _ => 0.5, // Non-spell actions get neutral priority
    }
}

fn creature_keyword_bonus(obj: &engine::game::game_object::GameObject) -> f64 {
    obj.keywords
        .iter()
        .map(|keyword| match keyword {
            Keyword::Flying
            | Keyword::Trample
            | Keyword::Vigilance
            | Keyword::Menace
            | Keyword::Lifelink
            | Keyword::Deathtouch
            | Keyword::FirstStrike
            | Keyword::DoubleStrike
            | Keyword::Haste => 0.03,
            _ => 0.0,
        })
        .sum::<f64>()
        .min(0.12)
}

fn creature_stat_bonus(obj: &engine::game::game_object::GameObject) -> f64 {
    let power = obj.power.unwrap_or(0).max(0) as f64;
    let toughness = obj.toughness.unwrap_or(0).max(0) as f64;
    ((power + toughness) / 20.0).min(0.1)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PtValue, TargetFilter, TriggerDefinition,
    };
    use engine::types::card_type::CoreType;
    use engine::types::identifiers::{CardId, ObjectId};
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
        obj.abilities = Arc::new(abilities);
        card_id
    }

    fn add_trigger_to_hand(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
        trigger: TriggerDefinition,
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
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.trigger_definitions.push(trigger);
        card_id
    }

    #[test]
    fn lands_always_max_priority() {
        let state = make_state();
        let score = should_play_now(
            &state,
            &GameAction::PlayLand {
                object_id: ObjectId(0),
                card_id: CardId(1),
            },
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
                cant_regenerate: false,
            })],
        );

        // No opponent creatures
        let score_empty = should_play_now(
            &state,
            &GameAction::CastSpell {
                object_id: ObjectId(0),
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
                object_id: ObjectId(0),
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
                object_id: ObjectId(0),
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

        let mut state = make_state();
        state.active_player = PlayerId(1); // Opponent's turn
                                           // Put something on the stack so the counterspell has a target
        state.stack.push_back(StackEntry {
            id: ObjectId(999),
            source_id: ObjectId(998),
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(500),
                ability: Some(ResolvedAbility::new(
                    Effect::Draw {
                        count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                        target: engine::types::ability::TargetFilter::Controller,
                    },
                    Vec::new(),
                    ObjectId(998),
                    PlayerId(1),
                )),
                casting_variant: engine::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
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
                object_id: ObjectId(0),
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
                object_id: ObjectId(0),
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
                object_id: ObjectId(0),
                card_id,
                targets: Vec::new(),
            },
            PlayerId(0),
        );

        state.phase = Phase::PostCombatMain;
        let score_post = should_play_now(
            &state,
            &GameAction::CastSpell {
                object_id: ObjectId(0),
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

    #[test]
    fn pump_spell_near_zero_during_end_step() {
        let mut state = make_state();
        state.phase = Phase::End;
        let card_id = add_spell_to_hand(
            &mut state,
            PlayerId(0),
            "Giant Growth",
            CoreType::Instant,
            vec![make_ability(Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Any,
            })],
        );

        let score = should_play_now(
            &state,
            &GameAction::CastSpell {
                object_id: ObjectId(0),
                card_id,
                targets: Vec::new(),
            },
            PlayerId(0),
        );
        assert!(
            score <= 0.1,
            "Pump spell should score near zero during End step, got {score}"
        );
    }

    #[test]
    fn pump_spell_near_zero_during_cleanup() {
        let mut state = make_state();
        state.phase = Phase::Cleanup;
        let card_id = add_spell_to_hand(
            &mut state,
            PlayerId(0),
            "Giant Growth",
            CoreType::Instant,
            vec![make_ability(Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Any,
            })],
        );

        let score = should_play_now(
            &state,
            &GameAction::CastSpell {
                object_id: ObjectId(0),
                card_id,
                targets: Vec::new(),
            },
            PlayerId(0),
        );
        assert!(
            score <= 0.1,
            "Pump spell should score near zero during Cleanup, got {score}"
        );
    }

    #[test]
    fn pump_spells_high_during_combat() {
        let mut state = make_state();
        state.phase = Phase::DeclareBlockers;
        let card_id = add_spell_to_hand(
            &mut state,
            PlayerId(0),
            "Giant Growth",
            CoreType::Instant,
            vec![make_ability(Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Any,
            })],
        );

        let score = should_play_now(
            &state,
            &GameAction::CastSpell {
                object_id: ObjectId(0),
                card_id,
                targets: Vec::new(),
            },
            PlayerId(0),
        );
        assert_eq!(score, 0.9, "Pump spell should score 0.9 during combat");
    }

    #[test]
    fn etb_creatures_gain_priority_from_trigger_value() {
        let mut state = make_state();
        let vanilla = add_spell_to_hand(
            &mut state,
            PlayerId(0),
            "Vanilla",
            CoreType::Creature,
            vec![],
        );
        let etb = add_trigger_to_hand(
            &mut state,
            PlayerId(0),
            "Harvester of Misery",
            5,
            4,
            TriggerDefinition::new(engine::types::triggers::TriggerMode::ChangesZone)
                .valid_card(TargetFilter::SelfRef)
                .destination(Zone::Battlefield)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Destroy {
                        target: TargetFilter::Any,
                        cant_regenerate: false,
                    },
                )),
        );

        let target = create_object(
            &mut state,
            CardId(800),
            PlayerId(1),
            "Target".to_string(),
            Zone::Battlefield,
        );
        let target_object = state.objects.get_mut(&target).unwrap();
        target_object.card_types.core_types.push(CoreType::Creature);
        target_object.power = Some(3);
        target_object.toughness = Some(3);

        let vanilla_score = should_play_now(
            &state,
            &GameAction::CastSpell {
                object_id: ObjectId(0),
                card_id: vanilla,
                targets: Vec::new(),
            },
            PlayerId(0),
        );
        let etb_score = should_play_now(
            &state,
            &GameAction::CastSpell {
                object_id: ObjectId(0),
                card_id: etb,
                targets: Vec::new(),
            },
            PlayerId(0),
        );

        assert!(etb_score > vanilla_score);
    }

    #[test]
    fn tutors_get_proactive_priority() {
        let mut state = make_state();
        let card_id = add_spell_to_hand(
            &mut state,
            PlayerId(0),
            "Strategic Tutor",
            CoreType::Sorcery,
            vec![make_ability(Effect::SearchLibrary {
                filter: TargetFilter::Any,
                count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: engine::types::ability::SearchSelectionConstraint::None,
            })],
        );

        let score = should_play_now(
            &state,
            &GameAction::CastSpell {
                object_id: ObjectId(0),
                card_id,
                targets: Vec::new(),
            },
            PlayerId(0),
        );

        assert!(
            score >= 0.7,
            "expected proactive tutor priority, got {score}"
        );
    }
}
