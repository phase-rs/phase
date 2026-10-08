//! Issue #5247 (p0-panic) — Kinetic Ooze: "double the number of +1/+1 counters
//! on any number of other target creatures" dropped its variable-target bound.
//!
//! The `MultiplyCounter` clause bound a single REQUIRED creature target instead
//! of a `MultiTargetSpec` of "any number" (min 0), so resolving the X>=10 ETB
//! sub-clause raised `Invalid action: Unused selected target slots` and broke the
//! game. Restoring the multi-target bound lets the controller supply a
//! conditional target without leaving an unused selection slot.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const KINETIC_OOZE: &str = "This creature enters with X +1/+1 counters on it.\n\
When this creature enters, destroy up to one target artifact or enchantment with mana value X or less. \
If X is 5 or more, you draw a card. \
If X is 10 or more, double the number of +1/+1 counters on any number of other target creatures.";

#[test]
fn kinetic_ooze_x10_resolves_conditional_target_without_panic() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The first optional target must be consumed before the conditional
    // counter-doubling target is selected. This is the reported failing shape:
    // before the regression fix, reserving the conditional target caused the
    // first selection to fail with "Unused selected target slots".
    let artifact = scenario
        .add_creature(P1, "Clockwork Relic", 1, 1)
        .as_artifact()
        .id();
    // A legal target for the X>=10 conditional instruction.
    let bear = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    scenario.with_counter(bear, CounterType::Plus1Plus1, 1);
    // The X>=5 draw clause needs a card in the library to avoid an empty-library
    // draw game-loss.
    scenario.add_card_to_library_top(P0, "Library Card");

    let ooze = scenario
        .add_creature_to_hand_from_oracle(P0, "Kinetic Ooze", 0, 0, KINETIC_OOZE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::X],
        })
        .id();

    // Ten generic mana to pay {X} with X = 10.
    scenario.with_mana_pool(
        P0,
        (0..10)
            .map(|i| ManaUnit::new(ManaType::Colorless, ObjectId(9_900 + i), false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    // Cast with X = 10 — this activates the X>=10 counter-doubling clause. The
    // first selection destroys the artifact; the second chooses the bear for
    // the conditional instruction.
    let outcome = runner
        .cast(ooze)
        .x(10)
        .target_objects(&[artifact, bear])
        .resolve();

    // The p0 fix: on `main` this panics with `Invalid action: Unused selected
    // target slots` while resolving the ETB; with the multi-target bound restored
    // the whole cast → ETB → trigger chain resolves cleanly back to Priority.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Kinetic Ooze's ETB must resolve cleanly (no 'Unused selected target slots' \
         panic), got {:?}",
        outcome.final_waiting_for()
    );

    // The cast/ETB pipeline ran to completion: the Ooze and targeted creature
    // remain on the battlefield, and the trigger resolved rather than aborting.
    assert!(
        runner.state().objects.contains_key(&ooze),
        "Kinetic Ooze must resolve onto the battlefield"
    );
    assert!(
        runner.state().objects.contains_key(&bear),
        "the targeted other creature must remain in play after a clean resolution"
    );
}
