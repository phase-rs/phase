//! Issue #2380 — Xenagos, God of Revels: the begin-combat trigger grants the
//! target creature haste AND `+X/+X` where X is that creature's power. The haste
//! was granted but the `+X/+X` power/toughness boost was dropped at runtime.
//!
//! CR 611.2c: the value of X is determined once, when the continuous effect is
//! created (at resolution), and "that creature's power" refers to the chosen
//! target. A 3/3 target therefore becomes 6/6 until end of turn.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

/// Effective P/T (post-layers) of an object, read off the materialized
/// `GameObject` fields the layer pipeline writes into.
fn power_toughness(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let obj = runner
        .state()
        .objects
        .get(&id)
        .expect("object still present");
    (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0))
}

const XENAGOS: &str = "Indestructible\nAs long as your devotion to red and green is less than seven, Xenagos isn't a creature.\nAt the beginning of combat on your turn, another target creature you control gains haste and gets +X/+X until end of turn, where X is that creature's power.";

#[test]
fn issue_2380_xenagos_doubles_target_power_and_grants_haste() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Xenagos, God of Revels", 0, 0, XENAGOS);
    let target = scenario.add_creature(P0, "Bear", 3, 3).id();

    let mut runner = scenario.build();
    runner.pass_both_players();

    assert_eq!(runner.state().phase, Phase::BeginCombat);

    // The begin-combat trigger is already on the stack bound to the only legal
    // "another target creature you control" (the Bear). If target selection is
    // still pending, submit it explicitly; then resolve the trigger.
    if matches!(
        runner.state().waiting_for,
        engine::types::game_state::WaitingFor::TriggerTargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target)],
            })
            .expect("select Xenagos begin-combat target");
    }
    runner.advance_until_stack_empty();

    // CR 611.2c: X = the target's power at resolution (3), so a 3/3 becomes 6/6.
    assert_eq!(
        power_toughness(&runner, target),
        (6, 6),
        "Xenagos must apply +X/+X where X = target's power (3/3 -> 6/6)"
    );

    let obj = runner
        .state()
        .objects
        .get(&target)
        .expect("target still present");
    assert!(
        obj.keywords.contains(&Keyword::Haste),
        "Xenagos must also grant haste to the target"
    );
}

/// CR 611.2c: X is the TARGET's own power at resolution — not a fixed amount and
/// not the source's power. A 5/5 target must become 10/10, proving the boost
/// scales with the recipient's power (the class behavior, not one P/T value).
#[test]
fn issue_2380_xenagos_scales_with_target_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Xenagos, God of Revels", 0, 0, XENAGOS);
    let target = scenario.add_creature(P0, "Beast", 5, 5).id();

    let mut runner = scenario.build();
    runner.pass_both_players();

    if matches!(
        runner.state().waiting_for,
        engine::types::game_state::WaitingFor::TriggerTargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target)],
            })
            .expect("select Xenagos begin-combat target");
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        power_toughness(&runner, target),
        (10, 10),
        "Xenagos +X/+X must scale with the target's own power (5/5 -> 10/10)"
    );
}
