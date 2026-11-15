//! Runtime regression tests for Acorn Catapult (issue #7191):
//!
//! "{1}, {T}: This artifact deals 1 damage to any target. That permanent's
//! controller or that player creates a 1/1 green Squirrel creature token."
//!
//! CR 115.1 + CR 608.2c + CR 111.2: the token goes to the damage RECIPIENT's
//! controller (a permanent target) or to the recipient itself (a player
//! target) — never to the Catapult's controller. The parser binds the
//! disjunctive subject to `ParentTargetController` and lifts it into the
//! chained Token's `owner`; the resolver then reads the parent target
//! (object → controller, player → itself).
//!
//! Two discriminating cases:
//! (a) Target the OPPONENT's creature — 1 damage marked on it, and the
//!     OPPONENT (not the Catapult's controller) creates a 1/1 green Squirrel.
//! (b) Target the OPPONENT player — life 20→19, and the OPPONENT creates it.
//! A third leg targets the controller's OWN creature: damage is marked and
//! the controller creates the token (same player on both sides).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::AbilityKind;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const ACORN_CATAPULT: &str = "{1}, {T}: This artifact deals 1 damage to any target. That permanent's controller or that player creates a 1/1 green Squirrel creature token.";

fn generic_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn activated_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| matches!(a.kind, AbilityKind::Activated))
        .expect("Acorn Catapult must expose its activated ability")
}

/// Count 1/1 green Squirrel tokens on the battlefield controlled by `player`.
fn squirrel_count(runner: &GameRunner, player: PlayerId) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            obj.controller == player
                && obj.card_types.subtypes.iter().any(|s| s == "Squirrel")
                && obj.power == Some(1)
                && obj.toughness == Some(1)
        })
        .count()
}

fn life_of(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.life)
        .unwrap_or(-999)
}

/// Leg A: target the opponent's creature — damage is marked on it and the
/// OPPONENT creates the Squirrel (CR 110.2: the targeted permanent's
/// controller; CR 608.2c + CR 115.1: the ability's written text and target
/// legality resolve the recipient anaphor to that player).
#[test]
fn acorn_catapult_targeting_opponent_creature_token_goes_to_its_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, generic_mana(1));

    let catapult = scenario
        .add_artifact_from_oracle(P0, "Acorn Catapult", ACORN_CATAPULT)
        .id();
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    let idx = activated_index(&runner, catapult);

    runner
        .activate(catapult, idx)
        .target_object(victim)
        .resolve();

    assert_eq!(
        runner.state().objects[&victim].damage_marked,
        1,
        "Acorn Catapult must deal 1 damage to the targeted creature"
    );
    assert_eq!(
        squirrel_count(&runner, P1),
        1,
        "the targeted creature's controller (P1) must create the Squirrel"
    );
    assert_eq!(
        squirrel_count(&runner, P0),
        0,
        "the Catapult's controller (P0) must NOT create a Squirrel"
    );
}

/// Leg B: target the opponent player — life drops and the OPPONENT creates it.
#[test]
fn acorn_catapult_targeting_opponent_player_token_goes_to_that_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, generic_mana(1));

    let catapult = scenario
        .add_artifact_from_oracle(P0, "Acorn Catapult", ACORN_CATAPULT)
        .id();

    let mut runner = scenario.build();
    let idx = activated_index(&runner, catapult);
    let life_before = life_of(&runner, P1);

    runner.activate(catapult, idx).target_player(P1).resolve();

    assert_eq!(
        life_of(&runner, P1),
        life_before - 1,
        "Acorn Catapult must deal 1 damage to the targeted player"
    );
    assert_eq!(
        squirrel_count(&runner, P1),
        1,
        "the targeted player (P1) must create the Squirrel"
    );
    assert_eq!(
        squirrel_count(&runner, P0),
        0,
        "the Catapult's controller (P0) must NOT create a Squirrel"
    );
}

/// Leg C (control): target the controller's own creature — damage is marked
/// and the controller creates the token (both sides coincide here).
#[test]
fn acorn_catapult_targeting_own_creature_token_goes_to_own_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, generic_mana(1));

    let catapult = scenario
        .add_artifact_from_oracle(P0, "Acorn Catapult", ACORN_CATAPULT)
        .id();
    let own = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();
    let idx = activated_index(&runner, catapult);

    runner.activate(catapult, idx).target_object(own).resolve();

    assert_eq!(
        runner.state().objects[&own].damage_marked,
        1,
        "Acorn Catapult must deal 1 damage to the targeted own creature"
    );
    assert_eq!(
        squirrel_count(&runner, P0),
        1,
        "the targeted creature's controller (P0) must create the Squirrel"
    );
}
