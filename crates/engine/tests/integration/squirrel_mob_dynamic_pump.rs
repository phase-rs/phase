//! Squirrel Mob — "This creature gets +1/+1 for each other Squirrel on the
//! battlefield."
//!
//! Regression coverage for the **dynamic self-pump** building block — a static
//! P/T modification whose magnitude is a count-based `QuantityExpr` resolved
//! against game state (CR 613.4c Layer 7c, magnitude recomputed each layer
//! pass per CR 611.3). Axes exercised:
//!   - **dynamic count** — the bonus scales with the number of Squirrels,
//!   - **"other" / "on the battlefield"** — the count excludes the source
//!     itself but includes Squirrels under ANY controller (CR 109.5, no
//!     controller clause),
//!   - **self-only target** — only Squirrel Mob is pumped, not the counted
//!     Squirrels,
//!   - **recompute** — the bonus tracks the count as Squirrels leave.
//!
//! Drives the REAL parse → synthesis → layer pipeline and reads back the
//! EFFECTIVE post-`evaluate_layers` power/toughness — a runtime test, not an
//! AST-shape test.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const SQUIRREL_MOB: &str = "This creature gets +1/+1 for each other Squirrel on the battlefield.";

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
fn squirrel_mob_scales_with_other_squirrels_any_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: a 1/1 Squirrel carrying the dynamic self-pump (real parse +
    // synthesis pipeline).
    let mob = scenario
        .add_creature_from_oracle(P0, "Squirrel Mob", 1, 1, SQUIRREL_MOB)
        .with_subtypes(vec!["Squirrel"])
        .id();

    // Another Squirrel you control.
    let your_squirrel = scenario
        .add_creature(P0, "Squirrel Token", 1, 1)
        .with_subtypes(vec!["Squirrel"])
        .id();

    // An opponent's Squirrel — still counts ("on the battlefield", no controller).
    let foe_squirrel = scenario
        .add_creature(P1, "Squirrel Nest Token", 1, 1)
        .with_subtypes(vec!["Squirrel"])
        .id();

    let mut runner = scenario.build();

    // CR 613.4c: base 1/1 + (2 other Squirrels) = 3/3.
    assert_eq!(
        effective_pt(&mut runner, mob),
        (3, 3),
        "Squirrel Mob counts 2 OTHER Squirrels (one yours, one opponent's) → 3/3"
    );

    // The buff is "This creature gets …" — self-only; the counted Squirrels are
    // not pumped.
    assert_eq!(
        effective_pt(&mut runner, your_squirrel),
        (1, 1),
        "a counted Squirrel must NOT itself be pumped (self-only target)"
    );
    assert_eq!(
        effective_pt(&mut runner, foe_squirrel),
        (1, 1),
        "the opponent's counted Squirrel must NOT be pumped"
    );
}

#[test]
fn squirrel_mob_recomputes_as_squirrels_leave() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mob = scenario
        .add_creature_from_oracle(P0, "Squirrel Mob", 1, 1, SQUIRREL_MOB)
        .with_subtypes(vec!["Squirrel"])
        .id();
    let s1 = scenario
        .add_creature(P0, "Squirrel Token", 1, 1)
        .with_subtypes(vec!["Squirrel"])
        .id();
    let s2 = scenario
        .add_creature(P0, "Squirrel Token", 1, 1)
        .with_subtypes(vec!["Squirrel"])
        .id();

    let mut runner = scenario.build();
    assert_eq!(
        effective_pt(&mut runner, mob),
        (3, 3),
        "baseline: 1/1 + 2 other Squirrels = 3/3"
    );

    // Remove one other Squirrel; CR 611.3: the dynamic magnitude recomputes to
    // +1/+1 → 2/2. With the source the only Squirrel left, the bonus is 0.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != s1);
        state.objects.remove(&s1);
    }
    assert_eq!(
        effective_pt(&mut runner, mob),
        (2, 2),
        "one Squirrel removed → 1/1 + 1 = 2/2"
    );

    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != s2);
        state.objects.remove(&s2);
    }
    assert_eq!(
        effective_pt(&mut runner, mob),
        (1, 1),
        "no other Squirrels → base 1/1 (the source is excluded by 'other')"
    );
}
