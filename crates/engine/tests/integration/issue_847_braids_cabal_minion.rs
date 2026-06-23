//! Issue #847 — Braids, Cabal Minion upkeep sacrifice must fire for each player.
//!
//! https://github.com/phase-rs/phase/issues/847

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BRAIDS_CABAL_MINION: &str =
    "At the beginning of each player's upkeep, that player sacrifices an artifact, a creature, or a land.";

#[test]
fn braids_cabal_minion_upkeep_sacrifice_prompts_active_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Braids, Cabal Minion", 2, 2, BRAIDS_CABAL_MINION);
    // Sacrifice fodder so a prior upkeep does not remove Braids from the battlefield.
    scenario.add_basic_land(P0, ManaColor::Green);
    let p1_creature_a = scenario.add_creature(P1, "Sacrifice Me A", 2, 2).id();
    let p1_creature_b = scenario.add_creature(P1, "Sacrifice Me B", 2, 2).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.active_player = P1;
        state.priority_player = P1;
        state.phase = Phase::Untap;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.advance_to_upkeep();

    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::EffectZoneChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).ok();
                runner.act(GameAction::PassPriority).ok();
            }
            _ => break,
        }
    }

    assert_eq!(
        runner.state().active_player,
        P1,
        "Braids upkeep trigger should fire on the opponent's first upkeep"
    );
    assert_eq!(runner.state().phase, Phase::Upkeep);

    let chosen = match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            count,
            ..
        } => {
            assert_eq!(
                *player, P1,
                "the upkeep player must choose what to sacrifice, not Braids's controller"
            );
            assert_eq!(
                cards.len(),
                2,
                "P1 must choose among multiple eligible permanents"
            );
            assert_eq!(*count, 1);
            assert!(
                cards.contains(&p1_creature_a) && cards.contains(&p1_creature_b),
                "both P1 creatures must be eligible sacrifice choices"
            );
            p1_creature_a
        }
        other => panic!("expected a sacrifice choice at P1 upkeep, got {:?}", other),
    };

    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("sacrifice choice must succeed");
    runner.advance_until_stack_empty();

    let sacrificed = if runner.state().objects[&p1_creature_a].zone == Zone::Graveyard {
        p1_creature_a
    } else {
        p1_creature_b
    };
    let remaining = if sacrificed == p1_creature_a {
        p1_creature_b
    } else {
        p1_creature_a
    };
    assert_eq!(
        runner.state().objects[&sacrificed].zone,
        Zone::Graveyard,
        "chosen permanent must be sacrificed"
    );
    assert_eq!(
        runner.state().objects[&remaining].zone,
        Zone::Battlefield,
        "unchosen permanent must remain on the battlefield"
    );
}

#[test]
fn braids_cabal_minion_single_eligible_permanent_auto_sacrifices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Braids, Cabal Minion", 2, 2, BRAIDS_CABAL_MINION);
    scenario.add_basic_land(P0, ManaColor::Green);
    let p1_creature = scenario.add_creature(P1, "Only Target", 2, 2).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.active_player = P1;
        state.priority_player = P1;
        state.phase = Phase::Untap;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.advance_to_upkeep();

    // CR 701.21a: one eligible permanent and a mandatory sacrifice of one —
    // no choice prompt; the engine auto-sacrifices during resolution.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::EffectZoneChoice { .. }
    ) {
        runner
            .choose_first_legal_target()
            .expect("sacrifice choice must succeed");
    } else {
        for _ in 0..8 {
            if runner.state().objects[&p1_creature].zone == Zone::Graveyard {
                break;
            }
            if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                runner.act(GameAction::PassPriority).ok();
                runner.act(GameAction::PassPriority).ok();
            } else {
                runner.advance_until_stack_empty();
            }
        }
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&p1_creature].zone,
        Zone::Graveyard,
        "the only eligible permanent must be sacrificed at upkeep"
    );
}
