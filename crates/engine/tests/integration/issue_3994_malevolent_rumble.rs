//! Regression for issue #3994: Malevolent Rumble must put unkept revealed cards
//! into the graveyard even when a SequentialSibling token sub-ability follows.
//!
//! https://github.com/phase-rs/phase/issues/3994

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn add_green_mana(runner: &mut engine::game::scenario::GameRunner, count: usize) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]));
    }
}

/// CR 701.20: Reveal four, optionally keep one permanent, rest to graveyard,
/// then create an Eldrazi Spawn token via the chained sub-ability.
#[test]
fn malevolent_rumble_puts_unkept_cards_in_graveyard() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Rest D", "Rest C", "Rest B", "Rest A"] {
        scenario.add_card_to_library_top(P0, name);
    }
    let rumble = scenario.add_real_card(P0, "Malevolent Rumble", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_green_mana(&mut runner, 2);

    let card_id = runner.state().objects[&rumble].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: rumble,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Malevolent Rumble");

    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::DigChoice { .. } => {
                runner
                    .act(GameAction::SelectCards { cards: vec![] })
                    .expect("decline optional keep");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                let _ = runner.act(GameAction::PassPriority);
            }
        }
    }

    let gy = &runner.state().players[P0.0 as usize].graveyard;
    for name in ["Rest A", "Rest B", "Rest C", "Rest D"] {
        let id = runner
            .state()
            .objects
            .values()
            .find(|o| o.name == name)
            .map(|o| o.id)
            .unwrap_or_else(|| panic!("missing library card {name}"));
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Graveyard,
            "{name} must be in graveyard after Malevolent Rumble resolves"
        );
        assert!(gy.contains(&id), "{name} must be listed in graveyard zone");
    }

    assert!(
        runner
            .state()
            .objects
            .values()
            .any(|o| o.zone == Zone::Battlefield && o.name == "Eldrazi Spawn"),
        "Malevolent Rumble must still create its Eldrazi Spawn token"
    );
}
