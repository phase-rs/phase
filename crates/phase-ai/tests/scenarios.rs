use std::collections::HashMap;

use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
use engine::game::zones::create_object;
use engine::types::ability::TargetRef;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    GameState, TargetSelectionProgress, TargetSelectionSlot, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use phase_ai::choose_action;
use phase_ai::config::{create_config, AiDifficulty, Platform};
use rand::rngs::SmallRng;
use rand::SeedableRng;

fn make_state() -> GameState {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state
}

fn add_creature(state: &mut GameState, owner: PlayerId, power: i32, toughness: i32) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        owner,
        "Creature".to_string(),
        Zone::Battlefield,
    );
    let object = state.objects.get_mut(&id).unwrap();
    object.card_types.core_types.push(CoreType::Creature);
    object.power = Some(power);
    object.toughness = Some(toughness);
    object.entered_battlefield_turn = Some(1);
    id
}

#[test]
fn scenario_prefers_opponent_target_over_self() {
    let mut state = make_state();
    state.waiting_for = WaitingFor::TriggerTargetSelection {
        player: PlayerId(0),
        target_slots: vec![TargetSelectionSlot {
            legal_targets: vec![
                TargetRef::Player(PlayerId(0)),
                TargetRef::Player(PlayerId(1)),
            ],
            optional: false,
        }],
        target_constraints: Vec::new(),
        selection: TargetSelectionProgress {
            current_slot: 0,
            selected_slots: Vec::new(),
            current_legal_targets: vec![
                TargetRef::Player(PlayerId(0)),
                TargetRef::Player(PlayerId(1)),
            ],
        },
    };

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(11);
    let action = choose_action(&state, PlayerId(0), &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::ChooseTarget {
            target: Some(TargetRef::Player(PlayerId(1))),
        })
    );
}

#[test]
fn scenario_skips_optional_target_with_no_legal_choices() {
    let mut state = make_state();
    state.waiting_for = WaitingFor::TriggerTargetSelection {
        player: PlayerId(0),
        target_slots: vec![TargetSelectionSlot {
            legal_targets: Vec::new(),
            optional: true,
        }],
        target_constraints: Vec::new(),
        selection: Default::default(),
    };

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(12);
    let action = choose_action(&state, PlayerId(0), &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::ChooseTarget { target: None })
    );
}

#[test]
fn scenario_blocks_lethal_attack_when_a_block_exists() {
    let mut state = make_state();
    state.phase = Phase::DeclareBlockers;
    state.active_player = PlayerId(1);
    state.players[0].life = 3;

    let attacker = add_creature(&mut state, PlayerId(1), 4, 4);
    let blocker = add_creature(&mut state, PlayerId(0), 1, 1);
    state.combat = Some(CombatState {
        attackers: vec![AttackerInfo {
            object_id: attacker,
            defending_player: PlayerId(0),
        }],
        ..Default::default()
    });
    state.waiting_for = WaitingFor::DeclareBlockers {
        player: PlayerId(0),
        valid_blocker_ids: vec![blocker],
        valid_block_targets: HashMap::from([(blocker, vec![attacker])]),
    };

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(13);
    let action = choose_action(&state, PlayerId(0), &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker)],
        })
    );
}

#[test]
fn scenario_multiplayer_attacks_to_finish_exposed_player() {
    let mut state = GameState::new(engine::types::format::FormatConfig::free_for_all(), 3, 42);
    state.turn_number = 2;
    state.phase = Phase::DeclareAttackers;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.players[1].life = 4;
    state.players[2].life = 20;

    let attacker_a = add_creature(&mut state, PlayerId(0), 3, 3);
    let attacker_b = add_creature(&mut state, PlayerId(0), 2, 2);
    let _threat_creature = add_creature(&mut state, PlayerId(2), 5, 5);

    state.waiting_for = WaitingFor::DeclareAttackers {
        player: PlayerId(0),
        valid_attacker_ids: vec![attacker_a, attacker_b],
        valid_attack_targets: vec![
            AttackTarget::Player(PlayerId(1)),
            AttackTarget::Player(PlayerId(2)),
        ],
    };

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(14);
    let action = choose_action(&state, PlayerId(0), &config, &mut rng);

    let Some(engine::types::actions::GameAction::DeclareAttackers { attacks }) = action else {
        panic!("expected declare attackers action");
    };
    assert_eq!(attacks.len(), 2);
    assert!(attacks
        .iter()
        .all(|(_, target)| *target == AttackTarget::Player(PlayerId(1))));
    assert!(attacks.iter().any(|(id, _)| *id == attacker_a));
    assert!(attacks.iter().any(|(id, _)| *id == attacker_b));
}

#[test]
fn scenario_mcts_plays_available_land_deterministically() {
    let mut state = make_state();
    let land_id = create_object(
        &mut state,
        CardId(99),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&land_id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(15);
    let action = choose_action(&state, PlayerId(0), &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::PlayLand {
            card_id: CardId(99),
        })
    );
}
