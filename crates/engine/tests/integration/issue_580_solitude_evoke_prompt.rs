//! Regression for issue #580: Solitude must offer Evoke as an alternative cast
//! cost and prompt exile of a white card from hand when the player opts in.
//!
//! https://github.com/phase-rs/phase/issues/580

use engine::game::rehydrate_game_from_card_db;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::AbilityCost;
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::game_state::{AlternativeCastKeyword, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn add_white_mana(runner: &mut engine::game::scenario::GameRunner, amount: u32) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(ManaType::White, dummy, false, vec![]));
    }
}

#[test]
fn solitude_fixture_carries_non_mana_evoke_keyword() {
    let Some(db) = load_db() else {
        eprintln!("skipping: integration card fixture not available");
        return;
    };
    let face = db
        .get_face_by_name("Solitude")
        .expect("Solitude must be in integration fixture");
    let evoke = face
        .keywords
        .iter()
        .find_map(|k| match k {
            engine::types::keywords::Keyword::Evoke(cost) => Some(cost),
            _ => None,
        })
        .expect("Solitude must carry Keyword::Evoke");
    assert!(
        matches!(
            evoke,
            engine::types::keywords::EvokeCost::NonMana(AbilityCost::Exile { .. })
        ),
        "Solitude evoke must be NonMana(Exile), got {evoke:?}"
    );
}

#[test]
fn solitude_cast_offers_evoke_when_both_costs_affordable() {
    let Some(db) = load_db() else {
        eprintln!("skipping: integration card fixture not available");
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let solitude = scenario.add_real_card(P0, "Solitude", Zone::Hand, db);
    let _white_filler = scenario.add_real_card(P0, "Doomed Traveler", Zone::Hand, db);
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    add_white_mana(&mut runner, 5);

    let card_id = runner.state().objects[&solitude].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: solitude,
            card_id,
            targets: vec![],
        })
        .expect("cast Solitude");

    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::AlternativeCastChoice {
                keyword: AlternativeCastKeyword::Evoke,
                alternative_additional_cost: Some(AbilityCost::Exile { .. }),
                ..
            }
        ),
        "expected AlternativeCastChoice(Evoke) with exile sub-cost, got {:?}",
        result.waiting_for
    );
}

#[test]
fn solitude_evoke_choice_prompts_exile_white_card_from_hand() {
    let Some(db) = load_db() else {
        eprintln!("skipping: integration card fixture not available");
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let solitude = scenario.add_real_card(P0, "Solitude", Zone::Hand, db);
    let white_filler = scenario.add_real_card(P0, "Doomed Traveler", Zone::Hand, db);
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    add_white_mana(&mut runner, 5);

    let card_id = runner.state().objects[&solitude].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: solitude,
            card_id,
            targets: vec![],
        })
        .expect("cast Solitude");
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("opt into evoke");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: engine::types::game_state::PayCostKind::ExileFromZone {
                    zone: engine::types::zones::ExileCostSourceZone::Hand,
                },
                ..
            }
        ),
        "evoke must prompt exile from hand, got {:?}",
        runner.state().waiting_for
    );

    let WaitingFor::PayCost { choices, .. } = &runner.state().waiting_for else {
        unreachable!();
    };
    assert!(
        choices.contains(&white_filler),
        "white filler card must be eligible for evoke exile: {choices:?}"
    );
    assert!(
        !choices.contains(&solitude),
        "Solitude itself must not be eligible for its own evoke exile"
    );
}
