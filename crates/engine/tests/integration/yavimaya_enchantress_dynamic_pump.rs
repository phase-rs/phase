//! Yavimaya Enchantress — "This creature gets +1/+1 for each enchantment on the
//! battlefield."
//!
//! Regression coverage for the **dynamic self-pump** building block where the
//! count ranges over a DIFFERENT object class than the source's own type —
//! enchantments, not creatures (CR 613.4c Layer 7c, magnitude recomputed each
//! layer pass per CR 611.3). Axes:
//!   - **count over enchantments** — the bonus scales with enchantment count,
//!   - **"on the battlefield"** — enchantments under ANY controller count
//!     (CR 109, no controller clause),
//!   - **self-only target** — only Yavimaya is pumped,
//!   - **recompute** — the bonus tracks the count as enchantments leave.
//!
//! Drives the REAL parse → synthesis → layer pipeline and reads back the
//! EFFECTIVE post-`evaluate_layers` power/toughness — a runtime test, not an
//! AST-shape test.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const YAVIMAYA_ENCHANTRESS: &str =
    "This creature gets +1/+1 for each enchantment on the battlefield.";

/// Recompute layers and read an object's effective (post-layer) power/toughness.
fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

#[test]
fn yavimaya_enchantress_scales_with_enchantments_any_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: a 0/0 creature carrying the dynamic self-pump (real parse +
    // synthesis pipeline). It is a creature, not an enchantment, so it does not
    // count itself.
    let enchantress = scenario
        .add_creature_from_oracle(P0, "Yavimaya Enchantress", 0, 0, YAVIMAYA_ENCHANTRESS)
        .with_subtypes(vec!["Elf", "Druid"])
        .id();

    // One enchantment you control + one the opponent controls — both count
    // ("on the battlefield", no controller clause).
    let _your_ench = scenario
        .add_creature(P0, "Pacifism", 0, 0)
        .as_enchantment()
        .id();
    let _foe_ench = scenario
        .add_creature(P1, "Oblivion Ring", 0, 0)
        .as_enchantment()
        .id();

    let mut runner = scenario.build();

    // CR 613.4c: base 0/0 + 2 enchantments = 2/2.
    assert_eq!(
        effective_pt(&mut runner, enchantress),
        (2, 2),
        "Yavimaya Enchantress counts 2 enchantments (one yours, one opponent's) → 2/2"
    );
}

#[test]
fn yavimaya_enchantress_recomputes_as_enchantments_leave() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let enchantress = scenario
        .add_creature_from_oracle(P0, "Yavimaya Enchantress", 0, 0, YAVIMAYA_ENCHANTRESS)
        .with_subtypes(vec!["Elf", "Druid"])
        .id();
    let e1 = scenario
        .add_creature(P0, "Pacifism", 0, 0)
        .as_enchantment()
        .id();
    let e2 = scenario
        .add_creature(P0, "Fertile Ground", 0, 0)
        .as_enchantment()
        .id();

    let mut runner = scenario.build();
    assert_eq!(
        effective_pt(&mut runner, enchantress),
        (2, 2),
        "baseline: 0/0 + 2 enchantments = 2/2"
    );

    // Remove one enchantment; CR 611.3: the dynamic magnitude recomputes to +1/+1.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != e1);
        state.objects.remove(&e1);
    }
    assert_eq!(
        effective_pt(&mut runner, enchantress),
        (1, 1),
        "one enchantment removed → 0/0 + 1 = 1/1"
    );

    // Keep e2 referenced for clarity; remove it too → 0/0 (no enchantments).
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != e2);
        state.objects.remove(&e2);
    }
    assert_eq!(
        effective_pt(&mut runner, enchantress),
        (0, 0),
        "no enchantments → base 0/0"
    );
}
