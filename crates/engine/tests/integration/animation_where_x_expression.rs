//! Real Oracle animations: explicit quantity expressions, source references,
//! and independently announced X values, through the public action pipeline.

use engine::ai_support::legal_actions;
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SAGE: &str = "{T}: Add two mana in any combination of colors.\n{T}: Until end of turn, target land you control becomes an X/X Citizen creature with haste in addition to its other types, where X is twice the number of Gates you control. Activate only as a sorcery.\nTap an untapped Gate you control: Untap this creature.";
const OBUUN: &str = "At the beginning of combat on your turn, up to one target land you control becomes an X/X Elemental creature with trample and haste until end of turn, where X is Obuun's power. It's still a land.\nLandfall — Whenever a land you control enters, put a +1/+1 counter on target creature.";
const LAIR: &str = "If you control two or more other lands, this land enters tapped.\n{T}: Add {G}.\n{X}{G}: Until end of turn, this land becomes an X/X green Hydra creature. It's still a land. X can't be 0.";
const BOOMERANG: &str = "Return target permanent to its owner's hand.";

struct SageBoard {
    runner: GameRunner,
    sage: ObjectId,
    gates: Vec<ObjectId>,
    other_land: ObjectId,
    opposing_gate: ObjectId,
    boomerang: ObjectId,
}

fn sage_board(gate_count: usize, phase: Phase) -> SageBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    let sage = scenario
        .add_creature_from_oracle(P0, "Sage of the Maze", 1, 3, SAGE)
        .id();
    // Textless lands are population fixtures, not substitutes for tested cards.
    // CR 109.5: include both owner/controller diagonals so 'you control' cannot
    // accidentally count cards owned by the activator or the target's owner.
    // With two controlled Gates, ownership populations are 1/4 versus control 2/3.
    let gates = (0..gate_count)
        .map(|_| {
            scenario
                .add_land_from_oracle(P1, "Controlled Gate", "")
                .with_subtypes(vec!["Gate"])
                .controlled_by(P0)
                .id()
        })
        .collect();
    let opposing_gate = scenario
        .add_land_from_oracle(P0, "Opposing Gate", "")
        .with_subtypes(vec!["Gate"])
        .controlled_by(P1)
        .id();
    for _ in 0..2 {
        scenario
            .add_land_from_oracle(P1, "Other opposing Gate", "")
            .with_subtypes(vec!["Gate"]);
    }
    let other_land = scenario.add_basic_land(P0, ManaColor::Green);
    // Keep two legal recipients even with zero Gates, so the engine must
    // request a target instead of auto-selecting its sole legal assignment.
    scenario.add_basic_land(P0, ManaColor::Green);
    let boomerang = scenario
        .add_spell_to_hand_from_oracle(P0, "Boomerang", true, BOOMERANG)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    // Cleanup advances into another turn; prevent unrelated empty-library loss.
    scenario.with_library_top(P0, &["Filler", "Filler"]);
    scenario.with_library_top(P1, &["Filler", "Filler"]);
    SageBoard {
        runner: scenario.build(),
        sage,
        gates,
        other_land,
        opposing_gate,
        boomerang,
    }
}

fn animation_index(runner: &GameRunner, source: ObjectId) -> usize {
    let indices: Vec<_> = runner.state().objects[&source]
        .abilities
        .iter()
        .enumerate()
        .filter_map(|(i, a)| matches!(a.effect.as_ref(), Effect::GenericEffect { .. }).then_some(i))
        .collect();
    assert_eq!(
        indices.len(),
        1,
        "exactly one parsed animation, not a mana ability or Unimplemented"
    );
    indices[0]
}

fn offered_activation(runner: &GameRunner, source: ObjectId) -> GameAction {
    let index = animation_index(runner, source);
    legal_actions(runner.state()).into_iter().find(|a| matches!(a,
        GameAction::ActivateAbility { source_id, ability_index, .. } if *source_id == source && *ability_index == index
    )).expect("engine offers the real animation")
}

fn assert_complete(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty(),
        "stack must finish resolving"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "unanswered prompt: {:?}",
        runner.state().waiting_for
    );
}

fn assert_animation(runner: &GameRunner, id: ObjectId, size: i32, subtype: &str) {
    let object = &runner.state().objects[&id];
    assert_eq!(object.zone, Zone::Battlefield);
    // CR 613.4b: X sets both base power and toughness.
    assert_eq!((object.power, object.toughness), (Some(size), Some(size)));
    // CR 205.1b: additive animations retain Land while adding Creature/subtype.
    assert!(object.card_types.core_types.contains(&CoreType::Land));
    assert!(object.card_types.core_types.contains(&CoreType::Creature));
    assert!(object.card_types.subtypes.iter().any(|s| s == subtype));
}

fn assert_sage_animation(board: &SageBoard, target: ObjectId, size: i32) {
    assert_animation(&board.runner, target, size, "Citizen");
    // CR 613.1f: the printed animation grants haste.
    assert!(board.runner.state().objects[&target].has_keyword(&Keyword::Haste));
    assert!(
        board.runner.state().objects[&board.sage].tapped,
        "tap cost actually paid"
    );
    assert_complete(&board.runner);
}

fn assert_sage_target_prompt(board: &SageBoard, target: ObjectId) {
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        selection,
        ..
    } = &board.runner.state().waiting_for
    else {
        panic!(
            "Sage must ask for a target, not X: {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    let slot = &target_slots[selection.current_slot];
    assert!(slot.legal_targets.contains(&TargetRef::Object(target)));
    assert!(!slot.optional);
}

fn commit_sage(board: &mut SageBoard, target: ObjectId) {
    let action = offered_activation(&board.runner, board.sage);
    board.runner.act(action).expect("activate Sage");
    assert_sage_target_prompt(board, target);
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("choose land");
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(board.runner.state().stack.len(), 1);
    let StackEntryKind::ActivatedAbility { ability, .. } = &board.runner.state().stack[0].kind
    else {
        panic!("committed activation required");
    };
    assert_eq!(ability.source_id, board.sage);
    assert_eq!(ability.controller, P0);
    assert!(board.runner.state().objects[&board.sage].tapped);
}

fn pass_cleanup(runner: &mut GameRunner, animated: &[ObjectId]) {
    runner.advance_to_combat();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { player: P0, .. }
    ));
    runner.declare_attackers(&[]).expect("declare no attackers");
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    // CR 514.2: the animation still exists in the end step, before cleanup.
    for id in animated {
        assert!(runner.state().objects[id]
            .card_types
            .core_types
            .contains(&CoreType::Creature));
    }
    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(runner.state().active_player, P1);
    assert_complete(runner);
}

#[test]
fn sage_counts_controlled_gates_snapshots_and_expires() {
    let mut board = sage_board(2, Phase::PreCombatMain);
    let target = board.gates[0];
    let index = animation_index(&board.runner, board.sage);
    assert_eq!(index, 1, "the second printed ability is the animation");
    offered_activation(&board.runner, board.sage);
    board
        .runner
        .activate(board.sage, index)
        .target_object(target)
        .resolve();
    // CR 107.3c + CR 109.5: two controlled Gates yield 4/4, without announcing X.
    assert_sage_animation(&board, target, 4);
    assert!(board.runner.state().objects[&target]
        .card_types
        .subtypes
        .contains(&"Gate".into()));
    assert!(!board.runner.state().objects[&board.other_land]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert!(!board.runner.state().objects[&board.opposing_gate]
        .card_types
        .core_types
        .contains(&CoreType::Creature));

    board
        .runner
        .cast(board.boomerang)
        .target_object(board.gates[1])
        .resolve();
    assert_eq!(
        board.runner.state().objects[&board.gates[1]].zone,
        Zone::Hand
    );
    // CR 608.2h: removing a counted Gate AFTER resolution does not resize it.
    assert_sage_animation(&board, target, 4);
    pass_cleanup(&mut board.runner, &[target]);
    let land = &board.runner.state().objects[&target];
    // CR 514.2: all until-end-of-turn modifications expire at cleanup.
    assert!(land.card_types.core_types.contains(&CoreType::Land));
    assert!(land.card_types.subtypes.contains(&"Gate".into()));
    assert!(!land.card_types.core_types.contains(&CoreType::Creature));
    assert!(!land.card_types.subtypes.contains(&"Citizen".into()));
    assert!(!land.has_keyword(&Keyword::Haste));
    assert_eq!((land.power, land.toughness), (None, None));
}

#[test]
fn sage_reads_gates_at_resolution_and_survives_source_departure() {
    for bounce_source in [false, true] {
        let mut board = sage_board(2, Phase::PreCombatMain);
        let target = board.other_land;
        commit_sage(&mut board, target);
        let bounced = if bounce_source {
            board.sage
        } else {
            board.gates[1]
        };
        board
            .runner
            .cast(board.boomerang)
            .target_object(bounced)
            .resolve();
        assert_eq!(board.runner.state().objects[&bounced].zone, Zone::Hand);
        // CR 608.2h + CR 113.7a: use the Gate count at resolution even after
        // the independently existing stack ability's source has left.
        assert_animation(
            &board.runner,
            target,
            if bounce_source { 4 } else { 2 },
            "Citizen",
        );
        assert!(board.runner.state().objects[&target].has_keyword(&Keyword::Haste));
        assert_complete(&board.runner);
    }
}

#[test]
fn sage_preserves_timing_and_target_restrictions() {
    let mut board = sage_board(2, Phase::PreCombatMain);
    let action = offered_activation(&board.runner, board.sage);
    board.runner.act(action).unwrap();
    assert_sage_target_prompt(&board, board.other_land);
    // CR 109.5: a controlled nonland and an opponent-controlled land are not
    // 'target land you control'; a legal land is offered in this same prompt.
    for invalid in [board.opposing_gate, board.sage] {
        assert!(board
            .runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(invalid))
            })
            .is_err());
        assert_sage_target_prompt(&board, board.other_land);
    }
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(board.other_land)),
        })
        .unwrap();
    board.runner.advance_until_stack_empty();
    assert_sage_animation(&board, board.other_land, 4);

    // CR 602.5d + CR 307.1: the identical parsed ability cannot be activated
    // outside a main phase or with a spell already on the stack.
    let mut combat = sage_board(2, Phase::BeginCombat);
    let index = animation_index(&combat.runner, combat.sage);
    assert!(combat.runner.state().objects[&combat.sage].abilities[index].is_sorcery_speed());
    assert!(matches!(combat.runner.act(GameAction::ActivateAbility {
        source_id: combat.sage,
        ability_index: index,
        payment_mode: CastPaymentMode::Auto,
    }), Err(EngineError::ActionNotAllowed(message)) if message.contains("Activation restriction not satisfied") && message.contains("AsSorcery")));
    assert!(!combat.runner.state().objects[&combat.sage].tapped);
    assert_complete(&combat.runner);

    let mut stacked = sage_board(2, Phase::PreCombatMain);
    let action = offered_activation(&stacked.runner, stacked.sage);
    let mut response = stacked
        .runner
        .cast(stacked.boomerang)
        .target_object(stacked.opposing_gate)
        .commit();
    assert_eq!(response.state().stack.len(), 1);
    assert!(matches!(
        response.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(
        matches!(response.act(action), Err(EngineError::ActionNotAllowed(message))
        if message.contains("Activation restriction not satisfied") && message.contains("AsSorcery"))
    );
    response.resolve();
    assert_eq!(
        stacked.runner.state().objects[&stacked.opposing_gate].zone,
        Zone::Hand
    );
    assert_complete(&stacked.runner);
}

#[test]
fn sage_zero_gates_kills_the_animated_land_and_invalid_target_does_not_redirect() {
    // Each negative includes the same successful activation path as a control.
    for count in [2, 0] {
        let mut board = sage_board(count, Phase::PreCombatMain);
        let target = board.other_land;
        commit_sage(&mut board, target);
        board.runner.advance_until_stack_empty();
        assert_complete(&board.runner);
        assert!(board.runner.state().objects[&board.sage].tapped);
        if count == 0 {
            // CR 704.5f: an animated 0/0 is put in its owner's graveyard.
            assert_eq!(board.runner.state().objects[&target].zone, Zone::Graveyard);
            assert!(board.runner.state().players[0].graveyard.contains(&target));
        } else {
            assert_sage_animation(&board, target, 4);
        }
    }
    for invalidate in [false, true] {
        let mut board = sage_board(2, Phase::PreCombatMain);
        let target = board.other_land;
        commit_sage(&mut board, target);
        if invalidate {
            board
                .runner
                .cast(board.boomerang)
                .target_object(target)
                .resolve();
            // CR 608.2b + CR 400.7: the moved target is illegal; no replacement
            // recipient is chosen among the remaining lands.
            assert_eq!(board.runner.state().objects[&target].zone, Zone::Hand);
            for gate in &board.gates {
                assert!(!board.runner.state().objects[gate]
                    .card_types
                    .core_types
                    .contains(&CoreType::Creature));
            }
        } else {
            board.runner.advance_until_stack_empty();
            assert_sage_animation(&board, target, 4);
        }
        assert_complete(&board.runner);
    }
}

#[test]
fn obuun_source_power_reference_accepts_and_declines_optional_target() {
    for choose_land in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let obuun = scenario
            .add_creature_from_oracle(P0, "Obuun, Mul Daya Ancestor", 3, 3, OBUUN)
            .id();
        scenario.add_vanilla(P0, 7, 7);
        let land = scenario.add_basic_land(P0, ManaColor::Green);
        let mut runner = scenario.build();
        runner.pass_both_players();
        assert_eq!(runner.state().phase, Phase::BeginCombat);
        let WaitingFor::TriggerTargetSelection {
            player,
            source_id,
            target_slots,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "combat must produce Obuun's target prompt: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(*source_id, Some(obuun));
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(land)));
        assert!(target_slots[0].optional);
        runner
            .act(GameAction::SelectTargets {
                targets: if choose_land {
                    vec![TargetRef::Object(land)]
                } else {
                    vec![]
                },
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_complete(&runner);
        if choose_land {
            // CR 608.2h: X is the triggering source's power, not another 7/7.
            assert_animation(&runner, land, 3, "Elemental");
            assert!(runner.state().objects[&land].has_keyword(&Keyword::Trample));
            assert!(runner.state().objects[&land].has_keyword(&Keyword::Haste));
        } else {
            assert!(!runner.state().objects[&land]
                .card_types
                .core_types
                .contains(&CoreType::Creature));
        }
    }
}

#[test]
fn lair_pending_activations_keep_independent_x_and_reject_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_land_from_oracle(P0, "Lair of the Hydra", LAIR)
        .id();
    let second = scenario
        .add_land_from_oracle(P0, "Lair of the Hydra", LAIR)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..10)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    scenario.with_library_top(P1, &["Filler", "Filler"]);
    let mut runner = scenario.build();
    for (source, x, expected_stack, remaining_mana) in [(first, 3, 1, 6), (second, 5, 2, 0)] {
        let action = offered_activation(&runner, source);
        runner.act(action).unwrap();
        let WaitingFor::ChooseXValue {
            player, min, max, ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "Lair must ask for announced X: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(*min, 1);
        assert!(*max >= x);
        // CR 107.3a: announce X through the actual public prompt. Lair's
        // printed restriction rejects zero, then accepts this activation's X.
        assert!(runner.act(GameAction::ChooseX { value: 0 }).is_err());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseXValue { player: P0, .. }
        ));
        runner.act(GameAction::ChooseX { value: x }).unwrap();
        // Auto payment may finish directly; otherwise complete the offered
        // mana-payment window. No target or other prompt is silently skipped.
        if let WaitingFor::ManaPayment { player, .. } = runner.state().waiting_for {
            assert_eq!(player, P0);
            runner
                .act(GameAction::PassPriority)
                .expect("pay the funded mana cost");
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        assert_eq!(runner.state().stack.len(), expected_stack);
        assert_eq!(runner.state().players[0].mana_pool.total(), remaining_mana);
    }
    // CR 107.3k: both activations are pending simultaneously; a shared last-X
    // value of 5 would incorrectly make the first land 5/5 as well.
    assert_eq!(runner.state().stack.len(), 2);
    runner.advance_until_stack_empty();
    assert_complete(&runner);
    assert_animation(&runner, first, 3, "Hydra");
    assert_animation(&runner, second, 5, "Hydra");
    for land in [first, second] {
        assert!(runner.state().objects[&land]
            .color
            .contains(&ManaColor::Green));
    }
    pass_cleanup(&mut runner, &[first, second]);
    for land in [first, second] {
        // CR 514.2: the announced-X animations also expire at cleanup.
        assert!(runner.state().objects[&land]
            .card_types
            .core_types
            .contains(&CoreType::Land));
        assert!(!runner.state().objects[&land]
            .card_types
            .core_types
            .contains(&CoreType::Creature));
    }
}
