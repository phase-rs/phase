//! Havi, the All-Father — runtime proof for the historic-card graveyard gate.
//!
use engine::game::engine::apply;
use engine::game::keywords::has_keyword;
use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const HAVI_ORACLE: &str = "Havi has indestructible as long as there are four or more historic cards in your graveyard. (Artifacts, legendaries, and Sagas are historic.)\nSage Project — Whenever Havi or another legendary creature you control dies, return target legendary creature card with lesser mana value from your graveyard to the battlefield tapped.";

fn has_indestructible_after_layers(runner: &mut GameRunner, havi: ObjectId) -> bool {
    flush_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&havi], &Keyword::Indestructible)
}

fn move_to_graveyard(runner: &mut GameRunner, object: ObjectId) {
    move_to_zone(runner.state_mut(), object, Zone::Graveyard, &mut Vec::new());
}

/// The production zone-change path invalidates and re-evaluates Havi's static:
/// four historic cards turn indestructible on; moving one out turns it off;
/// restoring it turns it back on. Artifact, legendary, and Saga membership are
/// each counted once despite overlapping historic axes.
#[test]
fn havi_historic_graveyard_gate_tracks_zone_membership_and_historic_axes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let havi = scenario
        .add_creature(P0, "Havi, the All-Father", 4, 4)
        .from_oracle_text(HAVI_ORACLE)
        .as_legendary()
        .id();
    let artifact = scenario
        .add_creature(P0, "Historic Artifact", 1, 1)
        .as_artifact()
        .id();
    let legendary = scenario
        .add_creature(P0, "Historic Legend", 1, 1)
        .as_legendary()
        .id();
    let saga = scenario
        .add_creature(P0, "Historic Saga", 1, 1)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .id();
    let fourth = scenario
        .add_creature(P0, "Second Historic Artifact", 1, 1)
        .as_artifact()
        .as_legendary()
        .id();
    let p0_nonhistoric = scenario.add_creature(P0, "P0 Nonhistoric", 1, 1).id();
    let p1_historic = scenario
        .add_creature(P1, "P1 Historic", 1, 1)
        .as_artifact()
        .id();
    let mut runner = scenario.build();

    for object in [artifact, legendary, saga] {
        move_to_graveyard(&mut runner, object);
    }
    assert!(
        !has_indestructible_after_layers(&mut runner, havi),
        "three historic cards are below Havi's threshold"
    );
    move_to_graveyard(&mut runner, p0_nonhistoric);
    move_to_graveyard(&mut runner, p1_historic);
    assert!(
        !has_indestructible_after_layers(&mut runner, havi),
        "P0 nonhistoric and P1 historic cards must not satisfy Havi's graveyard threshold"
    );

    move_to_graveyard(&mut runner, fourth);
    assert!(
        has_indestructible_after_layers(&mut runner, havi),
        "artifact, legendary, Saga, and multi-axis historic card count as four cards"
    );

    move_to_zone(runner.state_mut(), artifact, Zone::Exile, &mut Vec::new());
    assert!(
        !has_indestructible_after_layers(&mut runner, havi),
        "a multi-axis historic card counts once, so moving one of four drops the live condition"
    );
    move_to_graveyard(&mut runner, artifact);
    assert!(
        has_indestructible_after_layers(&mut runner, havi),
        "restoring the fourth historic card restores the live condition"
    );

    assert!(
        has_indestructible_after_layers(&mut runner, havi),
        "P0 nonhistoric and P1 historic cards remain excluded after Havi reaches four cards"
    );
}

/// A debug controller transfer is a production action that updates both the
/// Layer-2 base controller and the live controller. Havi's `your graveyard`
/// scope follows the live controller through P0 → P1 → P0 and forces layer
/// dirtiness/recomputation on each transition.
#[test]
fn havi_historic_graveyard_gate_follows_debug_controller_changes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let havi = scenario
        .add_creature(P0, "Havi, the All-Father", 4, 4)
        .from_oracle_text(HAVI_ORACLE)
        .as_legendary()
        .id();
    let mut p0_historic = Vec::new();
    let mut p1_historic = Vec::new();
    for index in 0..4 {
        p0_historic.push(
            scenario
                .add_creature(P0, &format!("P0 Historic {index}"), 1, 1)
                .as_artifact()
                .id(),
        );
        p1_historic.push(
            scenario
                .add_creature(P1, &format!("P1 Historic {index}"), 1, 1)
                .as_artifact()
                .id(),
        );
    }
    let mut runner = scenario.build();
    for object in p0_historic {
        move_to_graveyard(&mut runner, object);
    }
    assert!(has_indestructible_after_layers(&mut runner, havi));

    runner.state_mut().debug_mode = true;
    apply(
        runner.state_mut(),
        P0,
        GameAction::Debug(DebugAction::SetController {
            object_id: havi,
            controller: P1,
        }),
    )
    .expect("debug P0 → P1 controller transfer must succeed");
    assert_eq!(runner.state().objects[&havi].base_controller, Some(P1));
    assert_eq!(runner.state().objects[&havi].controller, P1);
    assert!(
        !runner.state().layers_dirty.is_dirty(),
        "production apply must consume SetController's layer dirtiness by recomputing layers"
    );
    assert!(
        !has_indestructible_after_layers(&mut runner, havi),
        "P1 has no historic graveyard cards yet"
    );

    for &object in &p1_historic {
        move_to_graveyard(&mut runner, object);
    }
    assert!(
        has_indestructible_after_layers(&mut runner, havi),
        "P1's historic graveyard becomes authoritative after control changes"
    );
    move_to_zone(
        runner.state_mut(),
        p1_historic[0],
        Zone::Exile,
        &mut Vec::new(),
    );
    assert!(
        !has_indestructible_after_layers(&mut runner, havi),
        "moving one P1 historic card out drops Havi below P1's threshold"
    );
    apply(
        runner.state_mut(),
        P1,
        GameAction::Debug(DebugAction::SetController {
            object_id: havi,
            controller: P0,
        }),
    )
    .expect("debug P1 → P0 controller transfer must succeed");
    assert_eq!(runner.state().objects[&havi].base_controller, Some(P0));
    assert_eq!(runner.state().objects[&havi].controller, P0);
    assert!(
        !runner.state().layers_dirty.is_dirty(),
        "production apply must again recompute the dirtied layer state"
    );
    assert!(
        has_indestructible_after_layers(&mut runner, havi),
        "P0's historic graveyard is authoritative again after return control"
    );
}
