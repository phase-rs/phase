//! Issue #4831: Bloodthorn Flail equip must accept "Pay {3} or discard a card".

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const BLOODTHORN_FLAIL_ORACLE: &str =
    "Equipped creature gets +2/+1.\nEquip—Pay {3} or discard a card.";

fn fund_generic(runner: &mut engine::game::scenario::GameRunner, amount: u32) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(ManaType::Colorless, dummy, false, vec![]));
    }
}

#[test]
fn bloodthorn_flail_equip_offers_disjunctive_cost_and_pays_mana_branch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let flail = scenario
        .add_creature(P0, "Bloodthorn Flail", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(BLOODTHORN_FLAIL_ORACLE)
        .id();

    let mut runner = scenario.build();
    fund_generic(&mut runner, 3);

    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: flail,
            ability_index: 0,
        })
        .expect("equip activation must be legal when {3} is available");

    match result.waiting_for {
        WaitingFor::ActivationCostOneOfChoice { ref costs, .. } => {
            assert_eq!(costs.len(), 2, "expected Pay {{3}} or discard branches");
        }
        other => panic!("expected ActivationCostOneOfChoice, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseActivationCostBranch { index: 0 })
        .expect("choosing the mana branch is accepted");

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&flail].zone,
        Zone::Battlefield,
        "equipment stays on the battlefield after equipping"
    );
    assert!(
        runner.state().objects[&creature]
            .attachments
            .contains(&flail),
        "Bloodthorn Flail must attach to the chosen creature after paying {{3}}"
    );
}

#[test]
fn bloodthorn_flail_from_card_db_equip_accepts_disjunctive_cost() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let flail = scenario.add_real_card(P0, "Bloodthorn Flail", Zone::Battlefield, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    fund_generic(&mut runner, 3);

    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: flail,
            ability_index: 0,
        })
        .expect("card-db Bloodthorn Flail equip must be activatable");

    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::ActivationCostOneOfChoice { .. }
        ),
        "exported card-data cost must normalize to OneOf, got {:?}",
        result.waiting_for
    );

    runner
        .act(GameAction::ChooseActivationCostBranch { index: 0 })
        .expect("mana branch selection accepted");

    runner.advance_until_stack_empty();

    assert!(
        runner.state().objects[&creature]
            .attachments
            .contains(&flail),
        "card-db Bloodthorn Flail must equip after paying {{3}}"
    );
}
