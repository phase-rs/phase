use std::collections::HashMap;

use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::game_state::{
    GameState, TargetSelectionProgress, TargetSelectionSlot, WaitingFor,
};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use phase_ai::choose_action;
use phase_ai::config::{create_config, AiDifficulty, Platform};
use rand::rngs::SmallRng;
use rand::SeedableRng;

#[test]
fn scenario_prefers_opponent_target_over_self() {
    let mut runner = GameScenario::new().build();
    runner.state_mut().waiting_for = WaitingFor::TriggerTargetSelection {
        player: P0,
        target_slots: vec![TargetSelectionSlot {
            legal_targets: vec![TargetRef::Player(P0), TargetRef::Player(P1)],
            optional: false,
        }],
        target_constraints: Vec::new(),
        selection: TargetSelectionProgress {
            current_slot: 0,
            selected_slots: Vec::new(),
            current_legal_targets: vec![TargetRef::Player(P0), TargetRef::Player(P1)],
        },
    };

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(11);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
    );
}

#[test]
fn scenario_skips_optional_target_with_no_legal_choices() {
    let mut runner = GameScenario::new().build();
    runner.state_mut().waiting_for = WaitingFor::TriggerTargetSelection {
        player: P0,
        target_slots: vec![TargetSelectionSlot {
            legal_targets: Vec::new(),
            optional: true,
        }],
        target_constraints: Vec::new(),
        selection: Default::default(),
    };

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(12);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::ChooseTarget { target: None })
    );
}

#[test]
fn scenario_blocks_lethal_attack_when_a_block_exists() {
    let mut scenario = GameScenario::new();
    scenario.with_life(P0, 3);
    let attacker = scenario.add_creature(P1, "Attacker", 4, 4).id();
    let blocker = scenario.add_creature(P0, "Blocker", 1, 1).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::DeclareBlockers;
        state.active_player = P1;
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo {
                object_id: attacker,
                defending_player: P0,
            }],
            ..Default::default()
        });
        state.waiting_for = WaitingFor::DeclareBlockers {
            player: P0,
            valid_blocker_ids: vec![blocker],
            valid_block_targets: HashMap::from([(blocker, vec![attacker])]),
        };
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(13);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker)],
        })
    );
}

#[test]
fn scenario_multiplayer_attacks_to_finish_exposed_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let attacker_a = scenario.add_creature(P0, "Attacker A", 3, 3).id();
    let attacker_b = scenario.add_creature(P0, "Attacker B", 2, 2).id();
    let _threat = scenario.add_creature(PlayerId(2), "Threat", 5, 5).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.phase = Phase::DeclareAttackers;
        state.players[1].life = 4;
        state.players[2].life = 20;
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: P0,
            valid_attacker_ids: vec![attacker_a, attacker_b],
            valid_attack_targets: vec![
                AttackTarget::Player(P1),
                AttackTarget::Player(PlayerId(2)),
            ],
        };
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(14);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    let Some(engine::types::actions::GameAction::DeclareAttackers { attacks }) = action else {
        panic!("expected declare attackers action");
    };
    assert_eq!(attacks.len(), 2);
    assert!(attacks
        .iter()
        .all(|(_, target)| *target == AttackTarget::Player(P1)));
    assert!(attacks.iter().any(|(id, _)| *id == attacker_a));
    assert!(attacks.iter().any(|(id, _)| *id == attacker_b));
}

#[test]
fn scenario_mcts_plays_available_land_deterministically() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land_id = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);

    // Move the land to hand (basic land is added to battlefield; we need it in hand for PlayLand)
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&land_id).unwrap();
        obj.zone = engine::types::zones::Zone::Hand;
        state.battlefield.retain(|&id| id != land_id);
        state.players[0].hand.push(land_id);
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(15);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::PlayLand {
            card_id: runner.state().objects[&land_id].card_id,
        })
    );
}
