//! Issue #4831: Bloodthorn Flail equip must accept "Pay {3} or discard a card".

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, WaitingFor};
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

    let mana_branch_index = match result.waiting_for {
        WaitingFor::ActivationCostOneOfChoice { ref costs, .. } => {
            assert!(
                !costs.is_empty(),
                "expected at least one payable Pay {{3}} or discard branch, got {costs:?}"
            );
            costs
                .iter()
                .position(|c| matches!(c, engine::types::ability::AbilityCost::Mana { .. }))
                .expect("mana branch must be offered when {3} is available")
        }
        other => panic!("expected ActivationCostOneOfChoice, got {other:?}"),
    };

    runner
        .act(GameAction::ChooseActivationCostBranch {
            index: mana_branch_index,
        })
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
fn bloodthorn_flail_equip_pays_discard_branch_without_mana_and_attaches() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let flail = scenario
        .add_creature(P0, "Bloodthorn Flail", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(BLOODTHORN_FLAIL_ORACLE)
        .id();
    let discard_card = scenario.add_card_to_hand(P0, "Hand Card");

    let mut runner = scenario.build();

    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: flail,
            ability_index: 0,
        })
        .expect("equip activation must be legal when discard is available");

    let discard_branch_index = match result.waiting_for {
        WaitingFor::ActivationCostOneOfChoice { ref costs, .. } => {
            assert!(
                costs
                    .iter()
                    .all(|c| !matches!(c, engine::types::ability::AbilityCost::Mana { .. })),
                "with no mana only the discard branch should be offered, got {costs:?}"
            );
            costs
                .iter()
                .position(|c| matches!(c, engine::types::ability::AbilityCost::Discard { .. }))
                .expect("discard branch must be offered when mana is unavailable")
        }
        other => panic!("expected ActivationCostOneOfChoice, got {other:?}"),
    };

    runner
        .act(GameAction::ChooseActivationCostBranch {
            index: discard_branch_index,
        })
        .expect("choosing the discard branch is accepted");

    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::Discard,
            choices,
            ..
        } => {
            assert!(
                choices.contains(&discard_card),
                "discardable hand card must be eligible: {choices:?}"
            );
        }
        other => panic!("expected PayCost Discard prompt, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![discard_card],
        })
        .expect("discarding a card pays the equip cost");

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&discard_card].zone,
        Zone::Graveyard,
        "the chosen card must be discarded to pay the equip cost"
    );
    assert!(
        runner.state().objects[&creature]
            .attachments
            .contains(&flail),
        "Bloodthorn Flail must attach to the chosen creature after discarding a card"
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

    let mana_branch_index = match result.waiting_for {
        WaitingFor::ActivationCostOneOfChoice { ref costs, .. } => costs
            .iter()
            .position(|c| matches!(c, engine::types::ability::AbilityCost::Mana { .. }))
            .expect("card-db equip must offer a payable mana branch"),
        other => panic!("exported card-data cost must normalize to OneOf, got {other:?}"),
    };

    runner
        .act(GameAction::ChooseActivationCostBranch {
            index: mana_branch_index,
        })
        .expect("mana branch selection accepted");

    runner.advance_until_stack_empty();

    assert!(
        runner.state().objects[&creature]
            .attachments
            .contains(&flail),
        "card-db Bloodthorn Flail must equip after paying {{3}}"
    );
}

#[test]
fn bloodthorn_flail_from_card_db_equip_accepts_discard_without_mana() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let flail = scenario.add_real_card(P0, "Bloodthorn Flail", Zone::Battlefield, db);
    let discard_card = scenario.add_card_to_hand(P0, "Hand Card");

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: flail,
            ability_index: 0,
        })
        .expect("card-db Bloodthorn Flail equip must be activatable via discard");

    let discard_branch_index = match result.waiting_for {
        WaitingFor::ActivationCostOneOfChoice { ref costs, .. } => {
            assert!(
                costs
                    .iter()
                    .all(|c| !matches!(c, engine::types::ability::AbilityCost::Mana { .. })),
                "with no mana only the discard branch should be offered, got {costs:?}"
            );
            costs
                .iter()
                .position(|c| matches!(c, engine::types::ability::AbilityCost::Discard { .. }))
                .expect("card-db equip must offer a payable discard branch")
        }
        other => panic!("exported card-data cost must normalize to OneOf, got {other:?}"),
    };

    runner
        .act(GameAction::ChooseActivationCostBranch {
            index: discard_branch_index,
        })
        .expect("discard branch selection accepted");

    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::Discard,
            choices,
            ..
        } => {
            assert!(
                choices.contains(&discard_card),
                "discardable hand card must be eligible: {choices:?}"
            );
        }
        other => panic!("expected PayCost Discard prompt, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![discard_card],
        })
        .expect("discarding a card pays the card-db equip cost");

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&discard_card].zone,
        Zone::Graveyard,
        "the chosen card must be discarded to pay the equip cost"
    );
    assert!(
        runner.state().objects[&creature]
            .attachments
            .contains(&flail),
        "card-db Bloodthorn Flail must equip after discarding a card"
    );
}

#[test]
fn bloodthorn_flail_from_card_db_equip_is_legal_action_without_mana() {
    use engine::ai_support::legal_actions;
    use engine::game::casting::can_activate_ability_now;

    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let flail = scenario.add_real_card(P0, "Bloodthorn Flail", Zone::Battlefield, db);
    let _discard_card = scenario.add_card_to_hand(P0, "Hand Card");

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let state = runner.state();

    assert!(
        can_activate_ability_now(state, P0, flail, 0),
        "card-db Bloodthorn Flail equip must be legal when discard is affordable"
    );
    assert!(
        legal_actions(state).iter().any(|action| matches!(
            action,
            GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            } if *source_id == flail
        )),
        "legal actions must offer card-db Bloodthorn Flail equip when discard is affordable"
    );
}
