//! Tishana's Tidebinder (phase-rs/phase issue #7625): the loses-abilities rider
//! applies only when the countered ability's source is an artifact, creature, or
//! planeswalker. A land or enchantment source is left untouched.
//!
//! Oracle text is verbatim from `data/card-data.json` (self-ref normalized to
//! "this creature").
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 608.2c: "if an ability of an artifact, creature, or planeswalker is
//!     countered this way" is a condition on the prior counter outcome.
//!   - CR 109.2: a description with a card type and no zone means a permanent
//!     of that type (so a land or enchantment source fails the condition).
//!   - CR 608.2h: the source's types are read live when the effect is applied.
//!   - CR 701.6a: countering removes the ability from the stack; none of its
//!     effects occur.
//!   - CR 611.2b + CR 702.26f: the rider lasts while Tishana is on the
//!     battlefield.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::move_to_zone;
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TISHANA: &str = "Flash\nWhen this creature enters, counter up to one target activated or triggered ability. If an ability of an artifact, creature, or planeswalker is countered this way, that permanent loses all abilities for as long as this creature remains on the battlefield. (Mana abilities can't be targeted.)";

/// A non-mana activated ability for each source kind (a mana ability can't be
/// targeted, so it could never be countered).
const TAP_GAIN_LIFE: &str = "{T}: You gain 1 life.";
const PAY_LIFE_GAIN_LIFE: &str = "Pay 1 life: You gain 2 life.";
const LOYALTY_GAIN_LIFE: &str = "+1: You gain 1 life.";

/// Builds the permanent whose ability will be countered.
type SourceBuilder = fn(&mut GameScenario) -> ObjectId;

struct Armed {
    runner: GameRunner,
    tishana: ObjectId,
    source: ObjectId,
}

/// Production path up to the resolved counter: P0 activates `source`'s ability
/// (CR 602.2) and leaves it on the stack, flashes in Tishana in response, and
/// her ETB trigger targets that ability (CR 603.3d) and counters it (CR 701.6a).
fn counter_ability_of(build_source: SourceBuilder) -> Armed {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tishana = scenario
        .add_creature_to_hand_from_oracle(P0, "Tishana's Tidebinder", 3, 2, TISHANA)
        .id();
    let source = build_source(&mut scenario);
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activating the source's untargeted ability must be accepted");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: exactly the activated ability sits on the stack"
    );
    let ability_on_stack = runner.state().stack.back().unwrap().id;
    let life_before = runner.state().players[0].life;

    runner
        .cast(tishana)
        .target_object(ability_on_stack)
        .resolve();

    assert!(
        runner.state().stack.is_empty(),
        "reach-guard: Tishana, her trigger and the countered ability left the stack"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before,
        "reach-guard: the countered ability never resolved (CR 701.6a)"
    );
    evaluate_layers(runner.state_mut());
    Armed {
        runner,
        tishana,
        source,
    }
}

fn rider_installed(armed: &Armed) -> bool {
    armed
        .runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|e| e.source_id == armed.tishana)
}

fn has_abilities(armed: &Armed) -> bool {
    !armed.runner.state().objects[&armed.source]
        .abilities
        .is_empty()
}

fn assert_loses_abilities(armed: &Armed) {
    assert!(
        rider_installed(armed),
        "the rider must be installed on the countered ability's source"
    );
    assert!(
        !has_abilities(armed),
        "CR 608.2c: the source must have lost all abilities"
    );
}

fn assert_keeps_abilities(armed: &Armed) {
    assert!(
        !rider_installed(armed),
        "CR 109.2: a source that is not an artifact, creature or planeswalker must not receive the rider"
    );
    assert!(has_abilities(armed), "the source must keep its abilities");
}

fn land(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_land_from_oracle(P0, "Abandoned Air Temple", TAP_GAIN_LIFE)
        .id()
}

fn enchantment(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(P0, "Underworld Breach", PAY_LIFE_GAIN_LIFE)
        .id()
}

fn artifact(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_artifact_from_oracle(P0, "Test Trinket", TAP_GAIN_LIFE)
        .id()
}

fn planeswalker(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_planeswalker_from_oracle(P0, "Test Walker", "Jace", 3, LOYALTY_GAIN_LIFE)
        .id()
}

fn creature(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Test Creature", 2, 2, TAP_GAIN_LIFE)
        .id()
}

fn artifact_land(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_land_from_oracle(P0, "Test Artifact Land", TAP_GAIN_LIFE)
        .as_artifact_land()
        .id()
}

fn animated_land(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_land_from_oracle(P0, "Test Animated Land", TAP_GAIN_LIFE)
        .as_creature()
        .id()
}

/// Issue #7625: a land's ability is countered, the land keeps its abilities.
/// Revert-failing: at base the rider is installed unconditionally, so
/// `assert_keeps_abilities` fails on "must not receive the rider".
#[test]
fn land_source_keeps_its_abilities() {
    assert_keeps_abilities(&counter_ability_of(land));
}

/// Issue #7625: an enchantment's ability is countered, it keeps its abilities.
/// Revert-failing for the same reason as the land case.
#[test]
fn enchantment_source_keeps_its_abilities() {
    assert_keeps_abilities(&counter_ability_of(enchantment));
}

/// Preservation (green at base by design): an artifact source loses all
/// abilities. Over-applying the gate flips "the rider must be installed".
#[test]
fn artifact_source_loses_all_abilities() {
    assert_loses_abilities(&counter_ability_of(artifact));
}

/// Preservation (green at base by design): a planeswalker source loses all
/// abilities.
#[test]
fn planeswalker_source_loses_all_abilities() {
    assert_loses_abilities(&counter_ability_of(planeswalker));
}

/// Preservation (green at base by design): a creature source loses all
/// abilities.
#[test]
fn creature_source_loses_all_abilities() {
    assert_loses_abilities(&counter_ability_of(creature));
}

/// Hostile: a multi-type source. An artifact land IS an artifact (CR 109.2),
/// so it loses its abilities. The gate is asked of the countered ability's
/// source: were it asked of Tishana (a creature), every land would be stripped.
#[test]
fn artifact_land_source_loses_all_abilities() {
    assert_loses_abilities(&counter_ability_of(artifact_land));
}

/// Hostile: an animated land is a land creature, so it IS a creature at
/// resolution (CR 608.2h: types are read live) and loses its abilities.
#[test]
fn animated_land_source_loses_all_abilities() {
    assert_loses_abilities(&counter_ability_of(animated_land));
}

/// Hostile: the source left the battlefield before the counter resolved. The
/// ability is still countered, but the rider has nothing to apply to and does
/// not panic.
#[test]
fn source_that_left_the_battlefield_is_not_given_the_rider() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tishana = scenario
        .add_creature_to_hand_from_oracle(P0, "Tishana's Tidebinder", 3, 2, TISHANA)
        .id();
    let source = creature(&mut scenario);
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activation accepted");
    let ability_on_stack = runner.state().stack.back().unwrap().id;
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), source, Zone::Graveyard, &mut events);
    assert!(
        !runner.state().battlefield.contains(&source),
        "reach-guard: the source has left the battlefield"
    );

    runner
        .cast(tishana)
        .target_object(ability_on_stack)
        .resolve();

    assert!(
        runner.state().stack.is_empty(),
        "reach-guard: the ability was countered and the stack emptied"
    );
    assert!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .all(|e| e.source_id != tishana),
        "no rider is installed on a source that left the battlefield"
    );
}
