use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use crate::cast_facts::{cast_facts_for_action, CastFacts};
use crate::eval::opponent_battlefield_creature_threat_value;
use crate::policies::effect_classify::{
    precombat_main_is_last_trick_window, spell_resolution_effects, CombatBinding, TimingSubject,
};
use crate::policies::hand_disruption::disruption_window_score;
#[cfg(test)]
use engine::types::game_state::CastPaymentMode;

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
            // CR 611.2a + CR 514.2: only the spell's own resolution times it as a combat trick;
            // an enters-trigger pump on a permanent cast precombat lasts through that combat. A
            // payload beside value that lands elsewhere still earns the combat window, but
            // nothing keeps the spell for it.
            let combat_binding = CombatBinding::of_resolution(&spell_resolution_effects(facts));
            let has_counter = immediate_effects
                .iter()
                .any(|effect| matches!(effect, Effect::Counter { .. }));
            let has_etb_value = !facts.immediate_etb_triggers.is_empty()
                || !facts.immediate_replacements.is_empty();

            // Removal: higher priority when opponents have high-value creatures.
            // In multiplayer, prefer targeting highest-threat opponent's best creature.
            if has_destroy || has_damage || (facts.has_direct_removal_text() && has_etb_value) {
                let max_threat = state
                    .battlefield
                    .iter()
                    .filter_map(|&id| opponent_battlefield_creature_threat_value(state, player, id))
                    .fold(0.0_f64, f64::max);

                // Scale 0.5-0.9 based on threat-weighted creature value
                return (0.5 + (max_threat / 30.0).min(0.4)).min(0.9);
            }

            // Combat tricks: highest during combat, near-zero during end/cleanup
            // (pump effects expire at cleanup — casting then has no lasting impact)
            if is_combat && combat_binding != CombatBinding::Unbound {
                return 0.9;
            }
            // CR 505.1a + CR 307.1: a sorcery-speed pump's last window before combat is not
            // mistimed.
            if combat_binding == CombatBinding::Whole
                && !precombat_main_is_last_trick_window(
                    state,
                    player,
                    TimingSubject::Spell(facts.object.id),
                )
            {
                return if matches!(state.phase, Phase::End | Phase::Cleanup) {
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
    use crate::cast_facts::collect_definition_effects;
    use crate::policies::effect_classify::effect_is_combat_trick_payload;
    use engine::game::layers::evaluate_layers;
    use engine::game::restrictions::spell_confined_to_sorcery_timing;
    use engine::game::scenario::{GameScenario, P0, P1};
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

                payment_mode: CastPaymentMode::Auto,
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

                payment_mode: CastPaymentMode::Auto,
            },
            PlayerId(0),
        );
        let threat =
            opponent_battlefield_creature_threat_value(&state, PlayerId(0), creature_id).unwrap();
        let expected = (0.5 + (threat / 30.0).min(0.4)).min(0.9);
        assert!(
            (score_with_creature - expected).abs() < f64::EPSILON,
            "removal priority must use the centralized threat value"
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
                source_rider: None,
                countered_spell_zone: None,
            })],
        );

        let score = should_play_now(
            &state,
            &GameAction::CastSpell {
                object_id: ObjectId(0),
                card_id,
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
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
                ability: Some(Box::new(ResolvedAbility::new(
                    Effect::Draw {
                        count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                        target: engine::types::ability::TargetFilter::Controller,
                    },
                    Vec::new(),
                    ObjectId(998),
                    PlayerId(1),
                ))),
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
                source_rider: None,
                countered_spell_zone: None,
            })],
        );

        let score = should_play_now(
            &state,
            &GameAction::CastSpell {
                object_id: ObjectId(0),
                card_id,
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
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
                source_rider: None,
                countered_spell_zone: None,
            })],
        );

        let score = should_play_now(
            &state,
            &GameAction::CastSpell {
                object_id: ObjectId(0),
                card_id,
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
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

                payment_mode: CastPaymentMode::Auto,
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

                payment_mode: CastPaymentMode::Auto,
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

                payment_mode: CastPaymentMode::Auto,
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

                payment_mode: CastPaymentMode::Auto,
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

                payment_mode: CastPaymentMode::Auto,
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

                payment_mode: CastPaymentMode::Auto,
            },
            PlayerId(0),
        );
        let etb_score = should_play_now(
            &state,
            &GameAction::CastSpell {
                object_id: ObjectId(0),
                card_id: etb,
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
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
                split: None,
                source_zones: vec![engine::types::zones::Zone::Library],
            })],
        );

        let score = should_play_now(
            &state,
            &GameAction::CastSpell {
                object_id: ObjectId(0),
                card_id,
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
            },
            PlayerId(0),
        );

        assert!(
            score >= 0.7,
            "expected proactive tutor priority, got {score}"
        );
    }

    // ---- D1: a non-protective until-end-of-turn GenericEffect pump is timed like a pump ----

    const COORDINATED_ASSAULT: &str = "Up to two target creatures each get +1/+0 and gain first strike until end of turn. (They deal combat damage before creatures without first strike.)";
    const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
    const UNLEASH_FURY: &str = "Double the power of target creature until end of turn.";
    const DWARVEN_SONG: &str = "One or more target creatures become red until end of turn.";
    const HEROIC_INTERVENTION: &str =
        "Permanents you control gain hexproof and indestructible until end of turn.";
    const ADAMANT_WILL: &str = "Target creature gets +2/+2 and gains indestructible until end of turn. (Damage and effects that say \"destroy\" don't destroy it.)";

    /// An instant in P0's hand built from its verbatim Oracle text; returns its cast action.
    fn oracle_instant(state: &mut GameState, name: &str, oracle_text: &str) -> GameAction {
        oracle_spell(state, name, oracle_text, CoreType::Instant)
    }

    /// An instant or sorcery in P0's hand built from its verbatim Oracle text; returns its cast
    /// action.
    fn oracle_spell(
        state: &mut GameState,
        name: &str,
        oracle_text: &str,
        core_type: CoreType,
    ) -> GameAction {
        let abilities = engine::parser::oracle::parse_oracle_text(
            oracle_text,
            name,
            &[],
            &[core_type.to_string()],
            &[],
        )
        .abilities;
        let card_id = add_spell_to_hand(state, PlayerId(0), name, core_type, abilities);
        let object_id = state
            .objects
            .values()
            .find(|object| object.card_id == card_id)
            .map(|object| object.id)
            .expect("the spell is in hand");
        GameAction::CastSpell {
            object_id,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        }
    }

    fn hint_in(phase: Phase, combat: bool, name: &str, oracle_text: &str) -> f64 {
        let mut state = make_state();
        state.phase = phase;
        if combat {
            state.combat = Some(engine::game::combat::CombatState::default());
        }
        let action = oracle_instant(&mut state, name, oracle_text);
        should_play_now(&state, &action, PlayerId(0))
    }

    /// CR 611.2a + CR 514.2: Coordinated Assault's until-end-of-turn `GenericEffect` pump and
    /// Unleash Fury's doubled power (CR 701.10b: +X/+0) both end at cleanup like Giant Growth's
    /// "+3/+3 until end of turn", so the hint times all three alike: low in the Draw step, high
    /// in combat, near zero in the End step. Dwarven Song is not a pump.
    #[test]
    fn should_play_now_times_a_generic_effect_pump_like_a_pump() {
        for (phase, combat) in [
            (Phase::Draw, false),
            (Phase::DeclareAttackers, true),
            (Phase::End, false),
        ] {
            let gg = hint_in(phase, combat, "Giant Growth", GIANT_GROWTH);
            let ca = hint_in(phase, combat, "Coordinated Assault", COORDINATED_ASSAULT);
            let uf = hint_in(phase, combat, "Unleash Fury", UNLEASH_FURY);
            assert_eq!(
                ca, gg,
                "{phase:?}: Coordinated Assault must time as Giant Growth"
            );
            assert_eq!(uf, gg, "{phase:?}: Unleash Fury must time as Giant Growth");
            if phase == Phase::DeclareAttackers {
                assert_ne!(
                    hint_in(phase, combat, "Dwarven Song", DWARVEN_SONG),
                    gg,
                    "{phase:?}: a colour change is not timed as a pump"
                );
            }
        }
    }

    /// The hint for an instant of P0's in the opponent's main phase, with its Murder on the stack
    /// aimed at a creature of P0's.
    fn hint_under_murder(name: &str, oracle_text: &str) -> f64 {
        let mut state = make_state();
        state.active_player = PlayerId(1);
        let next_id = state.next_object_id;
        let victim = create_object(
            &mut state,
            CardId(next_id),
            PlayerId(0),
            "Victim".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&victim)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let next_id = state.next_object_id;
        let murder = create_object(
            &mut state,
            CardId(next_id),
            PlayerId(1),
            "Murder".to_string(),
            Zone::Stack,
        );
        let effect = (*engine::parser::oracle::parse_oracle_text(
            "Destroy target creature.",
            "Murder",
            &[],
            &["Instant".to_string()],
            &[],
        )
        .abilities[0]
            .effect)
            .clone();
        state
            .stack
            .push_back(engine::types::game_state::StackEntry {
                id: murder,
                source_id: murder,
                controller: PlayerId(1),
                kind: engine::types::game_state::StackEntryKind::Spell {
                    ability: Some(Box::new(engine::types::ability::ResolvedAbility::new(
                        effect,
                        vec![engine::types::ability::TargetRef::Object(victim)],
                        murder,
                        PlayerId(1),
                    ))),
                    card_id: CardId(murder.0),
                    casting_variant: Default::default(),
                    actual_mana_spent: 0,
                },
            });
        let action = oracle_instant(&mut state, name, oracle_text);
        should_play_now(&state, &action, PlayerId(0))
    }

    /// CR 702.11b + CR 702.12b: hexproof and indestructible pay off against removal in any
    /// phase, so a protective grant is not timed like a pump. In the AI's End step, and in the
    /// opponent's main phase with its Murder on the stack, Heroic Intervention and Adamant Will
    /// read as Dwarven Song.
    #[test]
    fn should_play_now_does_not_time_a_protective_grant_like_a_pump() {
        let end = |name, oracle_text| hint_in(Phase::End, false, name, oracle_text);
        let ds = end("Dwarven Song", DWARVEN_SONG);
        let gg = end("Giant Growth", GIANT_GROWTH);
        assert_eq!(
            end("Coordinated Assault", COORDINATED_ASSAULT),
            gg,
            "reach: the pump timing fires in the End step"
        );
        assert_ne!(gg, ds, "reach: the pump timing differs from the default");
        assert_eq!(end("Heroic Intervention", HEROIC_INTERVENTION), ds);
        assert_eq!(end("Adamant Will", ADAMANT_WILL), ds);

        let ds = hint_under_murder("Dwarven Song", DWARVEN_SONG);
        assert_eq!(
            hint_under_murder("Heroic Intervention", HEROIC_INTERVENTION),
            ds
        );
        assert_eq!(hint_under_murder("Adamant Will", ADAMANT_WILL), ds);
    }

    const PAST_IN_FLAMES: &str = "Each instant and sorcery card in your graveyard gains flashback until end of turn. The flashback cost is equal to its mana cost.\nFlashback {4}{R} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
    const FLASHBACK: &str = "Target instant or sorcery card in your graveyard gains flashback until end of turn. The flashback cost is equal to its mana cost. (You may cast that card from your graveyard for its flashback cost. Then exile it.)";
    const UNDYING_EVIL: &str = "Target creature gains undying until end of turn. (When it dies, if it had no +1/+1 counters on it, return it to the battlefield under its owner's control with a +1/+1 counter on it.)";
    const CAULDRON_HAZE: &str = "Choose any number of target creatures. Each of those creatures gains persist until end of turn. (When it dies, if it had no -1/-1 counters on it, return it to the battlefield under its owner's control with a -1/-1 counter on it.)";
    const BLADES_OF_VELIS_VEL: &str = "Changeling (This card is every creature type.)\nUp to two target creatures each get +2/+0 and gain all creature types until end of turn.";
    const CORPOREAL_PROJECTION: &str = "Target creature you control gains myriad until end of turn. (Whenever it attacks, for each opponent other than defending player, you may create a token copy that's tapped and attacking that player or a planeswalker they control. Exile the tokens at end of combat.)\nOverload {3}{U}{U}{R}{R} (You may cast this spell for its overload cost. If you do, change \"target\" in its text to \"each.\")";

    /// CR 702.93a + CR 702.79a: undying and persist pay off when the creature dies, in whatever
    /// phase that happens, and CR 702.34a: flashback when the card is cast from the graveyard,
    /// so those grants read as Dwarven Song in every window, and under an opposing Murder.
    /// CR 702.116a: Corporeal Projection's myriad is paid out when the creature attacks, and
    /// Blades of Velis Vel still gives +2/+0, so both stay timed as Coordinated Assault is; only
    /// the sorcery-speed row is exempt, in the precombat main that is its last window.
    #[test]
    fn should_play_now_does_not_time_a_grant_whose_value_lands_outside_combat() {
        let sorcery = |phase, name, oracle_text| {
            let mut state = make_state();
            state.phase = phase;
            let action = oracle_spell(&mut state, name, oracle_text, CoreType::Sorcery);
            should_play_now(&state, &action, PlayerId(0))
        };
        for phase in [Phase::PreCombatMain, Phase::PostCombatMain, Phase::End] {
            let hint = |name, oracle_text| hint_in(phase, false, name, oracle_text);
            let ds = hint("Dwarven Song", DWARVEN_SONG);
            let pif = sorcery(phase, "Past in Flames", PAST_IN_FLAMES);
            let fb = hint("Flashback", FLASHBACK);
            let ue = hint("Undying Evil", UNDYING_EVIL);
            let ch = hint("Cauldron Haze", CAULDRON_HAZE);

            let ca = hint("Coordinated Assault", COORDINATED_ASSAULT);
            assert_ne!(ca, ds, "{phase:?}: reach: the pump timing fires");
            assert_eq!(
                hint("Blades of Velis Vel", BLADES_OF_VELIS_VEL),
                ca,
                "{phase:?}: Blades of Velis Vel's +2/+0 is a trick, whatever types it adds"
            );
            let cp = sorcery(phase, "Corporeal Projection", CORPOREAL_PROJECTION);
            if phase == Phase::PreCombatMain {
                assert_eq!(cp, ds, "{phase:?}: a sorcery's last window times nothing");
            } else {
                assert_eq!(
                    cp, ca,
                    "{phase:?}: Corporeal Projection's myriad stays timed"
                );
            }

            assert_eq!(
                pif, ds,
                "{phase:?}: Past in Flames must read as Dwarven Song"
            );
            assert_eq!(fb, ds, "{phase:?}: Flashback must read as Dwarven Song");
            assert_eq!(ue, ds, "{phase:?}: Undying Evil must read as Dwarven Song");
            assert_eq!(ch, ds, "{phase:?}: Cauldron Haze must read as Dwarven Song");
        }

        let ds = hint_under_murder("Dwarven Song", DWARVEN_SONG);
        let ue = hint_under_murder("Undying Evil", UNDYING_EVIL);
        let ch = hint_under_murder("Cauldron Haze", CAULDRON_HAZE);
        assert_ne!(
            hint_under_murder("Coordinated Assault", COORDINATED_ASSAULT),
            ds,
            "reach: the pump timing fires under Murder"
        );
        assert_eq!(ue, ds, "Undying Evil answers Murder in any phase");
        assert_eq!(ch, ds, "Cauldron Haze answers Murder in any phase");
    }

    const BLADE_TRIBE_BERSERKERS: &str = "Metalcraft — When this creature enters, if you control three or more artifacts, this creature gets +3/+3 and gains haste until end of turn.";
    const CENTAUR_HEALER: &str = "When this creature enters, you gain 3 life.";

    /// CR 608.3a + CR 603.6a + CR 611.2a: an enters trigger's pump on a permanent cast in the
    /// precombat main phase lasts through that turn's combat, so the cast is not timed as a
    /// combat trick. Blade-Tribe Berserkers' until-end-of-turn "+3/+3 and gains haste" enters
    /// trigger takes the creature hint, the one Centaur Healer's lifegain enters trigger earns on
    /// the same 3/3 body, and never falls below a vanilla 3/3's.
    #[test]
    fn should_play_now_does_not_time_an_enters_pump_creature_like_a_pump() {
        let etb = |name, oracle_text| {
            let triggers = engine::parser::oracle::parse_oracle_text(
                oracle_text,
                name,
                &[],
                &["Creature".to_string()],
                &[],
            )
            .triggers;
            assert_eq!(triggers.len(), 1, "premise: {name} has one enters trigger");
            triggers.into_iter().next().unwrap()
        };
        let berserkers_etb = etb("Blade-Tribe Berserkers", BLADE_TRIBE_BERSERKERS);
        assert!(
            berserkers_etb
                .execute
                .as_deref()
                .is_some_and(|execute| collect_definition_effects(execute)
                    .into_iter()
                    .any(effect_is_combat_trick_payload)),
            "premise: Blade-Tribe Berserkers' enters trigger carries a combat-trick payload: \
             {berserkers_etb:?}"
        );

        // The same 3/3 creature body for every row, with or without an enters trigger.
        let hint = |trigger: Option<TriggerDefinition>| {
            let mut state = make_state();
            let card_id = add_spell_to_hand(
                &mut state,
                PlayerId(0),
                "Body",
                CoreType::Creature,
                Vec::new(),
            );
            let object_id = state
                .objects
                .values()
                .find(|object| object.card_id == card_id)
                .map(|object| object.id)
                .unwrap();
            let obj = state.objects.get_mut(&object_id).unwrap();
            obj.power = Some(3);
            obj.toughness = Some(3);
            if let Some(trigger) = trigger {
                obj.trigger_definitions.push(trigger);
            }
            let action = GameAction::CastSpell {
                object_id,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            };
            let facts = cast_facts_for_action(&state, &action, PlayerId(0)).unwrap();
            let immediate_etb = !facts.immediate_etb_triggers.is_empty();
            (should_play_now(&state, &action, PlayerId(0)), immediate_etb)
        };
        let (berserkers, berserkers_reach) = hint(Some(berserkers_etb));
        let (healer, healer_reach) = hint(Some(etb("Centaur Healer", CENTAUR_HEALER)));
        let (vanilla, _) = hint(None);
        assert!(
            berserkers_reach && healer_reach,
            "reach: both enters triggers are read as the cast's immediate enters triggers"
        );
        assert_eq!(
            berserkers, healer,
            "Blade-Tribe Berserkers must take Centaur Healer's creature hint"
        );
        assert!(
            berserkers >= vanilla,
            "Blade-Tribe Berserkers {berserkers} must not fall below a vanilla 3/3's {vanilla}"
        );
    }

    const OVERRUN: &str = "Creatures you control get +3/+3 and gain trample until end of turn. (Each of those creatures can deal excess combat damage to the player or planeswalker it's attacking.)";
    const OVERCOME: &str = "Creatures you control get +2/+2 and gain trample until end of turn. (Each of those creatures can deal excess combat damage to the player or planeswalker it's attacking.)";
    const BURST_OF_SPEED: &str = "Creatures you control gain haste until end of turn. (They can attack and {T} even if they just came under your control.)";
    const ARM_THE_CATHARS: &str = "Until end of turn, target creature gets +3/+3, up to one other target creature gets +2/+2, and up to one other target creature gets +1/+1. Those creatures gain vigilance until end of turn.";
    const DEAFENING_CLARION: &str = "Choose one or both —\n• Deafening Clarion deals 3 damage to each creature.\n• Creatures you control gain lifelink until end of turn.";
    const GOBLIN_WAR_PARTY: &str = "Choose one —\n• Create three 1/1 red Goblin creature tokens.\n• Creatures you control get +1/+1 and gain haste until end of turn.\nEntwine {2}{R} (Choose both if you pay the entwine cost.)";
    const LEYLINE_OF_ANTICIPATION: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou may cast spells as though they had flash.";
    const HEROIC_REINFORCEMENTS: &str = "Create two 1/1 white Soldier creature tokens. Until end of turn, creatures you control get +1/+1 and gain haste. (They can attack and {T} this turn.)";
    const MONSTROUS_RAGE: &str = "Target creature gets +2/+0 until end of turn. Create a Monster Role token attached to it. (If you control another Role on it, put that one into the graveyard. Enchanted creature gets +1/+1 and has trample.)";
    const GERRARDS_COMMAND: &str = "Untap target creature. It gets +3/+3 until end of turn.";
    const INVIGORATED_RAMPAGE: &str = "Choose one —\n• Target creature gets +4/+0 and gains trample until end of turn.\n• Two target creatures each get +2/+0 and gain trample until end of turn.";

    /// CR 611.2a + CR 514.2: the trick hints need combat to be the spell's whole value. A spell
    /// with value elsewhere is not held back out of combat and takes its untimed arm, the
    /// default or the token arm, but its payload still earns the combat arm. A pump with only
    /// riders keeps the trick hints.
    #[test]
    fn should_play_now_times_a_candidate_only_when_combat_is_its_whole_value() {
        let sorcery = |phase, name, oracle_text| {
            let mut state = make_state();
            state.phase = phase;
            let action = oracle_spell(&mut state, name, oracle_text, CoreType::Sorcery);
            should_play_now(&state, &action, PlayerId(0))
        };
        let postcombat = Phase::PostCombatMain;

        assert_eq!(
            sorcery(postcombat, "Deafening Clarion", DEAFENING_CLARION),
            0.5,
            "Deafening Clarion takes the default arm"
        );
        for (name, oracle_text) in [
            ("Heroic Reinforcements", HEROIC_REINFORCEMENTS),
            ("Goblin War Party", GOBLIN_WAR_PARTY),
        ] {
            assert_eq!(
                sorcery(postcombat, name, oracle_text),
                0.54,
                "{name} takes the token arm"
            );
        }

        let rage = |phase, combat| hint_in(phase, combat, "Monstrous Rage", MONSTROUS_RAGE);
        assert_eq!(rage(Phase::PreCombatMain, false), 0.66, "the token arm");
        assert_eq!(
            rage(Phase::End, false),
            0.54,
            "the Role outlasts the End step"
        );
        assert_eq!(
            rage(Phase::DeclareAttackers, true),
            0.9,
            "the payload still earns the combat arm"
        );

        for (name, oracle_text) in [
            ("Gerrard's Command", GERRARDS_COMMAND),
            ("Invigorated Rampage", INVIGORATED_RAMPAGE),
        ] {
            assert_eq!(
                hint_in(Phase::PreCombatMain, false, name, oracle_text),
                0.3,
                "{name} can wait for combat"
            );
        }
        assert_eq!(
            sorcery(postcombat, "Arm the Cathars", ARM_THE_CATHARS),
            0.3,
            "postcombat, the turn's combat is behind Arm the Cathars"
        );
    }

    /// CR 505.1a + CR 307.1 + CR 307.5: a sorcery's precombat main is its last window before this
    /// turn's combat, and its until-end-of-turn payload lasts through it (CR 611.2a + CR 514.2),
    /// so the pump hint does not apply and the cast takes its untimed arm: the default, or the
    /// token arm for Goblin War Party's other mode. Instants on the same board, and Overrun in a
    /// postcombat main (CR 505.1a) or under its controller's Leyline of Anticipation (CR 601.3b),
    /// can still be timed. An opponent's Leyline opens nothing for it.
    #[test]
    fn should_play_now_does_not_time_a_sorcery_confined_pump_in_its_precombat_main() {
        let sorcery = |phase, name, oracle_text| {
            let mut state = make_state();
            state.phase = phase;
            let action = oracle_spell(&mut state, name, oracle_text, CoreType::Sorcery);
            should_play_now(&state, &action, PlayerId(0))
        };
        let main = Phase::PreCombatMain;

        assert_eq!(
            hint_in(main, false, "Giant Growth", GIANT_GROWTH),
            0.3,
            "reach: an instant pump keeps its main-phase hint"
        );
        assert_eq!(
            hint_in(main, false, "Coordinated Assault", COORDINATED_ASSAULT),
            0.3,
            "reach: a GenericEffect instant pump keeps its main-phase hint"
        );

        for (name, oracle_text) in [
            ("Overrun", OVERRUN),
            ("Overcome", OVERCOME),
            ("Burst of Speed", BURST_OF_SPEED),
            ("Arm the Cathars", ARM_THE_CATHARS),
            ("Deafening Clarion", DEAFENING_CLARION),
        ] {
            assert_eq!(
                sorcery(main, name, oracle_text),
                0.5,
                "{name} takes the default arm"
            );
        }
        assert_eq!(
            sorcery(main, "Goblin War Party", GOBLIN_WAR_PARTY),
            0.66,
            "Goblin War Party falls through to the token arm"
        );

        assert_eq!(
            sorcery(Phase::PostCombatMain, "Overrun", OVERRUN),
            0.3,
            "postcombat, the turn's combat is behind Overrun"
        );

        let with_leyline = |controller, confined| {
            let mut s = GameScenario::new_n_player(2, 42);
            s.at_phase(Phase::PreCombatMain);
            let overrun = s
                .add_spell_to_hand_from_oracle(P0, "Overrun", false, OVERRUN)
                .id();
            s.add_enchantment_from_oracle(
                controller,
                "Leyline of Anticipation",
                LEYLINE_OF_ANTICIPATION,
            );
            let mut runner = s.build();
            evaluate_layers(runner.state_mut());
            let state = runner.state();
            assert_eq!(
                spell_confined_to_sorcery_timing(state, P0, overrun),
                confined,
                "premise: the query, {controller:?}'s Leyline"
            );
            let action = GameAction::CastSpell {
                object_id: overrun,
                card_id: state.objects[&overrun].card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            };
            should_play_now(state, &action, P0)
        };
        assert_eq!(
            with_leyline(P0, false),
            0.3,
            "Overrun under its controller's Leyline can wait for combat"
        );
        assert_eq!(
            with_leyline(P1, true),
            0.5,
            "an opponent's Leyline opens nothing for Overrun"
        );
    }
}
