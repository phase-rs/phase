//! Regression for issue #868: Szarekh, the Silent King's attack trigger must
//! offer only artifact creature cards or Vehicle cards from among the cards
//! milled this way — not other artifacts.
//!
//! https://github.com/phase-rs/phase/issues/868

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, TargetFilter, TypeFilter};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const SZAREKH_ATTACK: &str = "Whenever Szarekh attacks, mill three cards. You may put an artifact creature card or Vehicle card from among the cards milled this way into your hand.";

fn mark_artifact(runner: &mut GameRunner, id: engine::types::identifiers::ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(engine::types::card_type::CoreType::Artifact);
}

fn mark_creature(runner: &mut GameRunner, id: engine::types::identifiers::ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(engine::types::card_type::CoreType::Creature);
}

fn mark_vehicle(runner: &mut GameRunner, id: engine::types::identifiers::ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types
        .core_types
        .push(engine::types::card_type::CoreType::Artifact);
    obj.card_types.subtypes.push("Vehicle".into());
}

fn seed_szarekh_library(
    scenario: &mut GameScenario,
) -> (
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
) {
    for i in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("Padding {i}"));
    }
    let equipment = scenario.add_card_to_library_top(P0, "Milled Equipment");
    let art_creature = scenario.add_card_to_library_top(P0, "Milled Art Creature");
    let vehicle = scenario.add_card_to_library_top(P0, "Milled Vehicle");
    (vehicle, art_creature, equipment)
}

#[test]
fn szarekh_attack_trigger_parses_artifact_creature_or_vehicle_from_milled() {
    let parsed = parse_oracle_text(
        SZAREKH_ATTACK,
        "Szarekh, the Silent King",
        &[],
        &[
            "Legendary".to_string(),
            "Artifact".to_string(),
            "Creature".to_string(),
        ],
        &["Necron".to_string()],
    );

    let attack = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Attacks)
        .expect("Szarekh must have an attacks trigger");

    let execute = attack.execute.as_ref().expect("attack trigger execute");
    let put = execute
        .sub_ability
        .as_ref()
        .expect("mill must chain to optional put-from-milled");
    let Effect::ChangeZone { target, .. } = &*put.effect else {
        panic!("expected ChangeZone, got {:?}", put.effect);
    };
    let TargetFilter::TrackedSetFiltered { filter, .. } = target else {
        panic!("expected TrackedSetFiltered, got {target:?}");
    };
    let TargetFilter::Or { filters } = filter.as_ref() else {
        panic!("expected Or filter, got {filter:?}");
    };
    let TargetFilter::Typed(left) = &filters[0] else {
        panic!("expected left Typed branch, got {:?}", filters[0]);
    };
    assert!(
        left.type_filters.contains(&TypeFilter::Artifact)
            && left.type_filters.contains(&TypeFilter::Creature),
        "artifact creature branch must require both types, got {:?}",
        left.type_filters
    );
}

#[test]
fn szarekh_attack_offers_only_artifact_creature_or_vehicle_from_milled() {
    let parsed = parse_oracle_text(
        SZAREKH_ATTACK,
        "Szarekh, the Silent King",
        &[],
        &[
            "Legendary".to_string(),
            "Artifact".to_string(),
            "Creature".to_string(),
        ],
        &["Necron".to_string()],
    );
    let attack = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Attacks)
        .expect("attacks trigger")
        .clone();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szarekh = scenario
        .add_creature(P0, "Szarekh, the Silent King", 3, 4)
        .id();
    let (vehicle, art_creature, equipment) = seed_szarekh_library(&mut scenario);

    let mut runner = scenario.build();
    mark_artifact(&mut runner, equipment);
    mark_artifact(&mut runner, art_creature);
    mark_creature(&mut runner, art_creature);
    mark_vehicle(&mut runner, vehicle);

    let execute = attack.execute.as_ref().expect("attack execute");
    let ability = build_resolved_from_def(execute, szarekh, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Szarekh attack chain should resolve");

    let engine::types::game_state::WaitingFor::EffectZoneChoice {
        cards, destination, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected EffectZoneChoice for optional put-from-milled, got {:?}",
            runner.state().waiting_for
        );
    };

    assert!(
        cards.contains(&art_creature),
        "artifact creature must be offered"
    );
    assert!(cards.contains(&vehicle), "Vehicle must be offered");
    assert!(
        !cards.contains(&equipment),
        "noncreature artifacts must not be offered (issue #868); offered = {cards:?}"
    );
    assert_eq!(*destination, Some(Zone::Hand));
}
