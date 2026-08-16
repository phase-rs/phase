//! CR 613.4c (#5743 review [HIGH]): Aspect of Wolf's "+X/+Y" pump binds each
//! axis to a DIFFERENT quantity — X = half the number of Forests you control,
//! rounded DOWN (power); Y = the same, rounded UP (toughness). With an ODD
//! Forest count (3) X = floor(3/2) = 1 and Y = ceil(3/2) = 2, so a 2/2 enchanted
//! creature resolves to 3/4 through the layer pipeline. A single-quantity
//! (same-rounding) implementation would produce 3/3 or 4/4, and reverting the
//! per-axis binding fails this test.

use engine::game::derived::derive_display_state;
use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;

const ASPECT_OF_WOLF: &str = "Enchant creature\nEnchanted creature gets +X/+Y, \
     where X is half the number of Forests you control, rounded down, and Y is \
     half the number of Forests you control, rounded up.";

fn power_toughness(
    runner: &engine::game::scenario::GameRunner,
    id: engine::types::identifiers::ObjectId,
) -> (i32, i32) {
    let obj = runner.state().objects.get(&id).expect("object present");
    (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0))
}

#[test]
fn aspect_of_wolf_binds_x_down_y_up_with_odd_forest_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario.add_creature(P0, "Wolf Host", 2, 2).id();
    let aura = scenario
        .add_creature(P0, "Aspect of Wolf", 0, 0)
        .from_oracle_text(ASPECT_OF_WOLF)
        .as_enchantment()
        .id();
    // Odd Forest count (3): X = floor(3/2) = 1, Y = ceil(3/2) = 2 — the counts
    // that distinguish the per-axis rounding from a single shared quantity.
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, creature);
    evaluate_layers(runner.state_mut());
    derive_display_state(runner.state_mut());

    assert_eq!(
        power_toughness(&runner, creature),
        (3, 4),
        "3 Forests → X = half rounded DOWN = 1 (power), Y = half rounded UP = 2 \
         (toughness): the 2/2 host becomes 3/4"
    );
}
