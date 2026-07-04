//! Regression for GitHub issue #3987 — Jace, Telepath Unbound's −3 targets a
//! split card in the graveyard; casting it must offer ModalFaceChoice so the
//! controller can cast the affordable half (Life // Death → Death).
//!
//! https://github.com/phase-rs/phase/issues/3987

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{CastingPermission, Duration, SpellStackToGraveyardReplacement};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

#[test]
fn graveyard_split_card_cast_offers_face_choice_for_affordable_half() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let life = scenario.add_real_card(P0, "Life", Zone::Graveyard, db);
    let creature_in_gy = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, life, false, vec![]),
            ManaUnit::new(ManaType::Colorless, life, false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    {
        let obj = runner.state_mut().objects.get_mut(&life).unwrap();
        obj.casting_permissions
            .push(CastingPermission::ExileWithAltCost {
                cost: obj.mana_cost.clone(),
                cast_transformed: false,
                constraint: None,
                granted_to: Some(P0),
                resolution_cleanup: None,
                duration: Some(Duration::UntilEndOfTurn),
                graveyard_replacement: Some(SpellStackToGraveyardReplacement::Exile),
                enters_with_counter: None,
                mana_spend_permission: None,
            });
    }

    assert!(
        engine::game::casting::can_cast_object_now(runner.state(), P0, life),
        "Death half must be castable from graveyard when only {{1}}{{B}} is affordable"
    );

    let commit = runner
        .cast(life)
        .modal_back_face(true)
        .target_object(creature_in_gy)
        .commit();

    let stack_spell = commit
        .state()
        .stack
        .last()
        .map(|e| &commit.state().objects[&e.source_id]);
    let Some(spell) = stack_spell else {
        panic!("Death half should reach the stack after graveyard face choice");
    };
    assert_eq!(spell.name, "Death");
}

#[test]
fn exiled_split_card_free_cast_permission_stays_free_after_face_choice() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let life = scenario.add_real_card(P0, "Life", Zone::Exile, db);
    let creature_in_gy = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    {
        let obj = runner.state_mut().objects.get_mut(&life).unwrap();
        obj.casting_permissions
            .push(CastingPermission::ExileWithAltCost {
                cost: ManaCost::zero(),
                cast_transformed: false,
                constraint: None,
                granted_to: Some(P0),
                resolution_cleanup: None,
                duration: None,
                graveyard_replacement: None,
                enters_with_counter: None,
                mana_spend_permission: None,
            });
    }

    assert!(
        engine::game::casting::can_cast_object_now(runner.state(), P0, life),
        "free-cast split card must stay castable with no mana in pool"
    );

    let commit = runner
        .cast(life)
        .modal_back_face(true)
        .target_object(creature_in_gy)
        .commit();

    let stack_spell = commit
        .state()
        .stack
        .last()
        .map(|e| &commit.state().objects[&e.source_id]);
    let Some(spell) = stack_spell else {
        panic!("Death half should reach the stack via a free exile permission");
    };
    assert_eq!(spell.name, "Death");
}
