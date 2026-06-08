//! Regression for issue #2374: Fblthp, the Lost must draw 1 from hand and 2
//! when it entered or was cast from library.
//!
//! https://github.com/phase-rs/phase/issues/2374

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{AbilityCondition, Effect, QuantityExpr};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FBLTHP_ETB: &str = "When Fblthp enters, draw a card. If it entered from your library or was cast from your library, draw two cards instead.";

#[test]
fn fblthp_etb_parses_library_origin_instead_draw() {
    let parsed = parse_oracle_text(
        FBLTHP_ETB,
        "Fblthp, the Lost",
        &[],
        &["Creature".to_string()],
        &["Homunculus".to_string()],
    );
    let trigger = parsed
        .triggers
        .first()
        .expect("Fblthp must have an ETB trigger");
    let execute = trigger.execute.as_ref().expect("ETB trigger must execute");
    assert!(matches!(
        execute.effect.as_ref(),
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            ..
        }
    ));
    let instead = execute
        .sub_ability
        .as_ref()
        .expect("library-origin instead rider must be a sub_ability");
    assert!(matches!(
        instead.effect.as_ref(),
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            ..
        }
    ));
    assert!(matches!(
        instead.condition.as_ref(),
        Some(AbilityCondition::ConditionInstead { inner })
            if matches!(
                inner.as_ref(),
                AbilityCondition::Or { conditions }
                    if conditions.len() == 2
            )
    ));
}

fn reseat_for_setup(
    runner: &mut engine::game::scenario::GameRunner,
    object_id: ObjectId,
    to: Zone,
) {
    let (owner, from) = runner
        .state()
        .objects
        .get(&object_id)
        .map(|obj| (obj.owner, obj.zone))
        .expect("setup object exists");
    engine::game::zones::remove_from_zone(runner.state_mut(), object_id, from, owner);
    engine::game::zones::add_to_zone(runner.state_mut(), object_id, to, owner);
    runner
        .state_mut()
        .objects
        .get_mut(&object_id)
        .expect("setup object exists")
        .zone = to;
}

fn resolve_fblthp_entry_from(origin: Zone) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fblthp = scenario
        .add_creature_to_hand_from_oracle(P0, "Fblthp, the Lost", 1, 1, FBLTHP_ETB)
        .id();
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    if origin != Zone::Hand {
        reseat_for_setup(&mut runner, fblthp, origin);
    }

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), fblthp, Zone::Battlefield, &mut events);
    let library_before_draw = runner.state().players[0].library.len();
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();

    library_before_draw - runner.state().players[0].library.len()
}

#[test]
fn fblthp_runtime_draws_two_only_from_library_origin() {
    assert_eq!(
        resolve_fblthp_entry_from(Zone::Library),
        2,
        "Fblthp entering from library must satisfy the origin gate and draw two"
    );
    assert_eq!(
        resolve_fblthp_entry_from(Zone::Hand),
        1,
        "Fblthp entering from hand must leave the origin gate false and draw one"
    );
}
