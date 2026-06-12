//! Regression for GitHub issue #688 — Mind into Matter optional put filter.
//!
//! With X=4, a permanent with mana value 4 or less in hand (e.g. Resonating
//! Lute, an artifact) must be a legal target for the optional sub-ability.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::Effect;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

#[test]
fn mind_into_matter_put_includes_artifact_at_x_four() {
    let Some(db) = load_db() else {
        return;
    };

    let face = db
        .get_face_by_name("Mind into Matter")
        .expect("Mind into Matter must be in card database");
    let spell = face
        .abilities
        .first()
        .expect("Mind into Matter must have a spell ability");
    let sub = spell
        .sub_ability
        .as_ref()
        .expect("Mind into Matter must parse the optional put sub-ability");
    let Effect::ChangeZone { target, .. } = &*sub.effect else {
        panic!("expected ChangeZone sub-ability, got {:?}", sub.effect);
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lute_id = scenario.add_real_card(P0, "Resonating Lute", Zone::Hand, db);
    let _land_id = scenario.add_real_card(P0, "Forest", Zone::Hand, db);
    let mind_id = scenario.add_real_card(P0, "Mind into Matter", Zone::Hand, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let mut sub_resolved = build_resolved_from_def(sub.as_ref(), mind_id, P0);
    sub_resolved.set_chosen_x_recursive(4);

    let ctx = FilterContext::from_ability(&sub_resolved);
    let lute_legal = matches_target_filter(runner.state(), lute_id, target, &ctx);
    assert!(
        lute_legal,
        "Resonating Lute (2UR artifact) must be legal at X=4 for Mind into Matter put"
    );
}
