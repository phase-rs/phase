//! Regression for GitHub issue #3287 — Life // Death must allow casting either half.
//!
//! CR 709.3 / CR 709.3a-b: Each face of a split card is a separate spell. Casting
//! the right half (Death) requires a cast-time face choice, mirroring spell//spell
//! MDFCs.

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

#[test]
fn life_death_split_card_prompts_face_choice_and_casts_death_half() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let life = scenario.add_real_card(P0, "Life", Zone::Hand, db);
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
        let obj = runner.state().objects.get(&life).unwrap();
        assert_eq!(obj.name, "Life");
        assert_eq!(
            obj.back_face.as_ref().map(|b| b.name.as_str()),
            Some("Death"),
            "Life // Death must hydrate the other split half"
        );
    }

    let commit = runner
        .cast(life)
        .modal_back_face(true)
        .target_object(creature_in_gy)
        .commit();

    let stack_obj = commit
        .state()
        .stack
        .last()
        .map(|e| &commit.state().objects[&e.source_id]);
    let Some(spell) = stack_obj else {
        panic!("Death half should reach the stack");
    };
    assert_eq!(spell.name, "Death");
}

#[test]
fn life_death_castable_via_legal_actions_when_only_death_half_affordable() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let life = scenario.add_real_card(P0, "Life", Zone::Hand, db);
    let _creature_in_gy = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, life, false, vec![]),
            ManaUnit::new(ManaType::Colorless, life, false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let state = runner.state();

    assert!(
        engine::game::casting::can_cast_object_now(state, P0, life),
        "Life // Death must be castable when only Death {{1}}{{B}} is affordable"
    );

    let actions = engine::ai_support::legal_actions(state);
    assert!(
        actions.iter().any(|action| matches!(
            action,
            engine::types::actions::GameAction::CastSpell { object_id, .. }
                if *object_id == life
        )),
        "legal_actions must include CastSpell for Life // Death"
    );
}

#[test]
fn life_death_death_half_reverts_to_life_after_resolution() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let life = scenario.add_real_card(P0, "Life", Zone::Hand, db);
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

    let outcome = runner
        .cast(life)
        .modal_back_face(true)
        .target_object(creature_in_gy)
        .resolve();

    let obj = outcome.state().objects.get(&life).unwrap();
    assert_eq!(
        obj.zone,
        Zone::Graveyard,
        "resolved Death half should put Life // Death in graveyard"
    );
    assert_eq!(
        obj.name, "Life",
        "object must revert to the front split half after leaving the stack"
    );
    assert!(
        !obj.modal_back_face,
        "modal_back_face marker must clear after zone-exit revert"
    );
}
