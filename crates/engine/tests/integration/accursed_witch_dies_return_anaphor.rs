//! Accursed Witch: the dying source returns transformed and attached to the
//! selected opponent, through the production zone-change and trigger pipeline.
//! CR 400.7e + CR 603.6: the trigger finds its source in the destination zone.
//! CR 712.14a: returning a double-faced card transformed uses its back face.

use std::sync::Arc;

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::triggers::process_triggers;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const DIES_TRIGGER: &str = "When this creature dies, return it to the battlefield transformed under your control attached to target opponent.";

#[test]
fn accursed_witch_dies_return_it_binds_self() {
    let db = shared_card_db().expect("the real DFC regression requires generated card data");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let witch = scenario.add_real_card(P0, "Accursed Witch", Zone::Battlefield, db);
    let mut runner = scenario.build();
    // Hydrate the real two-faced card, but parse the trigger with this source
    // tree so stale generated parser output cannot hide an origin-stamp revert.
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let triggers = parse_oracle_text(
        DIES_TRIGGER,
        "Accursed Witch",
        &[],
        &["Creature".to_string()],
        &[],
    )
    .triggers;
    assert_eq!(triggers.len(), 1, "expected Accursed Witch's dies trigger");
    let obj = runner.state_mut().objects.get_mut(&witch).unwrap();
    assert!(!obj.transformed);
    assert!(
        obj.back_face.is_some(),
        "the return must exercise a real DFC"
    );
    // Keep the overlay in the base set: `process_triggers` flushes layers before
    // collecting LKI, which materializes live triggers from this authority.
    obj.install_trigger_base_definitions(Arc::new(triggers))
        .expect("test trigger base set must materialize");
    reindex_object_triggers(runner.state_mut(), witch);

    let mut events = Vec::new();
    assert!(!move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(witch, Zone::Graveyard, witch),
        &mut events,
    ));
    process_triggers(runner.state_mut(), &events);
    assert_eq!(runner.state().objects[&witch].zone, Zone::Graveyard);
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. }
        ),
        "the dying Witch must ask for the opponent attachment target"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("choose the opponent to enchant");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "the trigger must resolve completely"
    );
    let returned = &runner.state().objects[&witch];
    assert_eq!(returned.zone, Zone::Battlefield);
    assert!(returned.transformed);
    assert_eq!(returned.name, "Infectious Curse");
    assert_eq!(returned.attached_to, Some(AttachTarget::Player(P1)));
}
