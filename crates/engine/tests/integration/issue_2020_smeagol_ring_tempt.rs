//! Issue #2020 — Sméagol, Helpful Guide RingTemptsYou trigger must target an
//! opponent so the RevealUntil land-steal effect reveals the chosen library.
//!
//! https://github.com/phase-rs/phase/issues/2020

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const SMEAGOL_RING_TRIGGER: &str = "Whenever the Ring tempts you, target opponent reveals cards from the top of their library until they reveal a land card. Put that card onto the battlefield tapped under your control and the rest into their graveyard.";
const RING_TEMPT_ORACLE: &str = "The Ring tempts you.";

#[test]
fn smeagol_ring_tempt_reveal_steals_opponent_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let smeagol = scenario
        .add_creature_from_oracle(P0, "Sméagol, Helpful Guide", 4, 2, SMEAGOL_RING_TRIGGER)
        .id();
    let forest = scenario.add_card_to_library_top(P1, "Forest");
    let island = scenario.add_card_to_library_top(P1, "Island");

    let spell = scenario
        .add_spell_to_hand(P0, "Tempt Spell", false)
        .from_oracle_text(RING_TEMPT_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();

    {
        let forest_obj = runner.state_mut().objects.get_mut(&forest).unwrap();
        forest_obj.card_types.core_types.push(CoreType::Land);
        forest_obj.base_card_types = forest_obj.card_types.clone();
    }
    {
        let island_obj = runner.state_mut().objects.get_mut(&island).unwrap();
        island_obj.card_types.core_types.push(CoreType::Instant);
        island_obj.base_card_types = island_obj.card_types.clone();
    }

    let trigger = &runner.state().objects[&smeagol].trigger_definitions[0];
    assert_eq!(trigger.mode, TriggerMode::RingTemptsYou);
    assert_eq!(
        trigger.valid_target,
        Some(TargetFilter::Player),
        "parser must surface opponent player target for RevealUntil"
    );

    runner.cast(spell).resolve();

    let mut resolved = false;
    for _ in 0..96 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P1)],
                    })
                    .expect("target opponent for RingTemptsYou reveal");
            }
            WaitingFor::ChooseRingBearer { candidates, .. } => {
                runner
                    .act(GameAction::ChooseRingBearer {
                        target: candidates[0],
                    })
                    .expect("choose ring bearer");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                resolved = true;
                break;
            }
            _ => {
                runner.act(GameAction::PassPriority).ok();
            }
        }
    }

    assert!(resolved, "ring tempt chain must resolve to priority");
    assert_eq!(
        runner.state().ring_level.get(&P0).copied(),
        Some(1),
        "ring must have tempted the controller"
    );
    let island_in_gy = runner.state().players[1]
        .graveyard
        .iter()
        .any(|id| runner.state().objects[id].name == "Island");
    assert!(
        island_in_gy,
        "reveal must mill opponent Island into graveyard before hitting Forest; gy={:?}",
        runner.state().players[1]
            .graveyard
            .iter()
            .map(|id| &runner.state().objects[id].name)
            .collect::<Vec<_>>()
    );
    assert!(
        runner.state().objects.values().any(|obj| {
            obj.zone == Zone::Battlefield
                && obj.controller == P0
                && obj.name == "Forest"
                && obj.tapped
        }),
        "opponent's revealed Forest must enter tapped under Sméagol's controller; waiting_for={:?}",
        runner.state().waiting_for
    );
}
