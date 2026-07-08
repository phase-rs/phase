//! Spell-form "Tap target X. … That X doesn't untap during its controller's
//! next untap step." must lock ONLY the single tapped object (CR 608.2c
//! anaphor), not broadcast the CR 502.3 untap restriction over every matching
//! permanent.
//!
//! Chandra's Revolution: "deals 4 damage to target creature. Tap target land.
//! That land doesn't untap during its controller's next untap step." — the
//! lock must bind the tapped land, leave a second land free to untap, and NOT
//! bind the separately-targeted damaged creature.
//!
//! Glacial Grasp: "Tap target creature. Its controller mills two cards. That
//! creature doesn't untap…" — the lock must bind the tapped creature and leave
//! a second creature free to untap.
//!
//! Revert-discriminating: without `|| inherits_parent` in
//! `static_affected_for_application`, the CantUntap static's `affected` is the
//! broadcast `Typed(Land)` / `Typed(Creature)`, the engine installs a
//! `SpecificObject` lock on EVERY matching permanent, and the "second
//! permanent untaps" assertions below fail.
//!
//! CR 608.2c (anaphora), CR 502.3 (untap-step restriction).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;

const CHANDRA_REVOLUTION: &str = "Chandra's Revolution deals 4 damage to target creature. \
Tap target land. That land doesn't untap during its controller's next untap step.";

const GLACIAL_GRASP: &str = "Tap target creature. Its controller mills two cards. \
That creature doesn't untap during its controller's next untap step. Draw a card.";

#[test]
fn chandra_revolution_locks_only_the_tapped_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Opponent (P1) controls two lands and a high-toughness creature so the 4
    // damage doesn't kill it (we need it on the battlefield at P1's untap step).
    let tapped_land = scenario.add_basic_land(P1, ManaColor::Red);
    let other_land = scenario.add_basic_land(P1, ManaColor::Green);
    let creature = scenario.add_creature(P1, "Stone Wall", 0, 9).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Chandra's Revolution", false, CHANDRA_REVOLUTION)
        .id();

    let mut runner = scenario.build();
    // Pre-tap the opponent's lands and creature so the untap step is observable.
    for id in [tapped_land, other_land, creature] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }

    // CR 601.2c: damage→creature, tap→land. Targets bind to slots in order.
    runner
        .cast(spell)
        .target_objects(&[creature, tapped_land])
        .resolve();

    // Advance into the opponent's turn, past their untap step (CR 502.3), to
    // the upkeep where priority is next granted.
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(
        runner.state().active_player,
        P1,
        "should now be the opponent's turn (their untap step has processed)"
    );

    assert!(
        runner.state().objects[&tapped_land].tapped,
        "the tapped land must stay tapped (its untap was locked)"
    );
    assert!(
        !runner.state().objects[&other_land].tapped,
        "a second land the opponent controls must UNTAP — the lock must not broadcast"
    );
    assert!(
        !runner.state().objects[&creature].tapped,
        "the damaged creature must UNTAP — the land lock must not bind the damage target"
    );
}

#[test]
fn glacial_grasp_locks_only_the_tapped_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let tapped_creature = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let other_creature = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    // Library content so the "mills two cards" clause has cards to mill, and a
    // card for P0's "Draw a card" so the draw doesn't deck-out P0 (CR 104.3c).
    scenario.with_library_top(P1, &["Plains", "Plains", "Plains"]);
    scenario.with_library_top(P0, &["Island", "Island", "Island"]);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Glacial Grasp", true, GLACIAL_GRASP)
        .id();

    let mut runner = scenario.build();
    for id in [tapped_creature, other_creature] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }

    runner.cast(spell).target_object(tapped_creature).resolve();

    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(runner.state().active_player, P1);

    assert!(
        runner.state().objects[&tapped_creature].tapped,
        "the tapped creature must stay tapped (its untap was locked)"
    );
    assert!(
        !runner.state().objects[&other_creature].tapped,
        "a second creature the opponent controls must UNTAP — the lock must not broadcast"
    );
}
