//! Sauron, Dino Devotee (issue #7031) — runtime proof that a bullet-line
//! triggered-modal mode body's "It's a …" anaphor binds to the MODE'S OWN
//! TARGET (the creature that just received the saurian counter), never the
//! trigger source.
//!
//! Sauron's printed Oracle text (mode 2):
//!   "Turn People into Dinosaurs — Put a saurian counter on another target
//!    creature. It's a green Dinosaur with base power and toughness 5/5 for as
//!    long as it has a saurian counter on it."
//!
//! Regression mechanism (#6811 → #7031): the native-IR modal path threaded the
//! trigger context's `subject: Some(SelfRef)` into mode-body parsing without
//! the `derive_modal_subject` filtering the pre-IR path applied. The mode-body
//! "It's" then resolved to `SelfRef`, the contracted-copula honest-bind gate
//! declined to animate the trigger source, and the clause fell to
//! `Effect::Unimplemented` — no animation at runtime.
//!
//! CR 608.2c: instructions are followed in written order — the mode's own
//! earlier instruction ("Put a saurian counter on another target creature")
//! is the nearest antecedent for the mode-body "It".
//! CR 611.2b: "for as long as" durations — the effect ends when the condition
//! stops being true and does not resume.
//! CR 611.2c: the set of objects the continuous effect applies to is fixed
//! when the effect begins (the recipient is snapshotted).
//! CR 613.4b: base power/toughness setting applies in Layer 7b.
//! CR 122.1: counters placed by the resolving mode.
//! CR 700.2b + CR 603.3c: a mode with no legal targets cannot be chosen.
//!
//! Revert-fail (R1): with the mode-body subject filter reverted, mode 2's
//! second sentence is `Unimplemented`, so Foe Bear keeps (2,2) and the (5,5)
//! assertion fails.

use engine::game::combat::AttackTarget;
use engine::game::derived::derive_display_state;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const SAURON_ORACLE: &str = "Flying\nWhenever Sauron enters or attacks, choose one —\n• Cure Cancer — You gain 3 life.\n• Turn People into Dinosaurs — Put a saurian counter on another target creature. It's a green Dinosaur with base power and toughness 5/5 for as long as it has a saurian counter on it.";

fn saurian() -> CounterType {
    CounterType::Generic("saurian".to_string())
}

/// Post-layer P/T read from object fields (mirrors the established
/// integration-test helper pattern, e.g. `heroic_defiance_recipient_color_4590`).
fn power_toughness(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let obj = runner.state().objects.get(&id).expect("object present");
    (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0))
}

fn saurian_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object present")
        .counters
        .get(&saurian())
        .copied()
        .unwrap_or(0)
}

/// Drive Sauron's attack trigger to resolution: order triggers, select
/// `modes`, answer the target prompt with `target` (if the chosen mode has
/// one), then pass priority until the stack is empty. Bounded loop guards
/// against a stall. Modeled on the Grenzo #2346 driver.
fn drive_attack_trigger(runner: &mut GameRunner, modes: &[usize], target: Option<ObjectId>) {
    let mut chose_mode = false;
    let mut remaining_target = target;
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .or_else(|_| runner.act(GameAction::OrderTriggers { order: vec![] }))
                    .expect("order triggers");
            }
            WaitingFor::AbilityModeChoice { .. } => {
                runner
                    .act(GameAction::SelectModes {
                        indices: modes.to_vec(),
                    })
                    .expect("select mode");
                chose_mode = true;
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                let target = remaining_target
                    .take()
                    .expect("a target prompt requires a declared target");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose target");
            }
            WaitingFor::Priority { .. } => {
                if chose_mode && runner.state().stack.is_empty() {
                    return;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            other => panic!(
                "unexpected WaitingFor while driving Sauron's trigger: {}",
                other.variant_name()
            ),
        }
    }
    panic!("Sauron's trigger did not resolve within the step budget");
}

/// R1 + R2: mode 2 animates the MODE'S TARGET into a green Dinosaur with base
/// P/T 5/5 (CR 613.4b), the source is untouched (the source-vs-target
/// diagonal), and the `for as long as` duration is LIVE (CR 611.2b): removing
/// the saurian counter ends the effect.
#[test]
fn sauron_mode_two_animates_mode_target_and_duration_is_live() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // P0 controls Sauron (trigger source; 4/4 printed).
    let sauron = scenario
        .add_creature_from_oracle(P0, "Sauron, Dino Devotee", 4, 4, SAURON_ORACLE)
        .id();
    // P1 controls TWO creatures so the engine surfaces a real target-selection
    // prompt (a single legal target could auto-resolve).
    let foe_bear = scenario.add_creature(P1, "Foe Bear", 2, 2).id();
    let _foe_wolf = scenario.add_creature(P1, "Foe Wolf", 3, 1).id();

    let mut runner = scenario.build();

    // The attack limb of "Whenever Sauron enters or attacks" fires the trigger.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(sauron, AttackTarget::Player(P1))])
        .expect("declare attackers");

    // Choose mode 2 ("Turn People into Dinosaurs", index 1), target Foe Bear.
    drive_attack_trigger(&mut runner, &[1], Some(foe_bear));

    // CR 122.1: the mode's first instruction placed the saurian counter on the
    // mode's target (positive reach-guard for the animation assertions below).
    assert_eq!(
        saurian_counters(&runner, foe_bear),
        1,
        "mode 2 must put a saurian counter on the mode's target"
    );

    // R1 (revert-fail): CR 613.4b Layer 7b — the mode-body "It's a green
    // Dinosaur with base power and toughness 5/5" binds to the MODE'S TARGET.
    // Reverted, the clause is Unimplemented and Foe Bear stays (2,2).
    assert_eq!(
        power_toughness(&runner, foe_bear),
        (5, 5),
        "the mode's target must have base power and toughness 5/5"
    );

    // Source-vs-target diagonal (both alive): the trigger source must NOT be
    // animated and must NOT have a counter.
    assert_eq!(
        power_toughness(&runner, sauron),
        (4, 4),
        "the trigger source must not be animated by the mode body"
    );
    assert_eq!(
        saurian_counters(&runner, sauron),
        0,
        "the trigger source must not receive the saurian counter"
    );

    // R2: CR 611.2b — the duration condition is evaluated LIVE against the
    // snapshotted recipient (CR 611.2c): removing the saurian counter ends the
    // effect and Foe Bear reverts to its printed 2/2.
    runner
        .state_mut()
        .objects
        .get_mut(&foe_bear)
        .expect("Foe Bear present")
        .counters
        .remove(&saurian());
    evaluate_layers(runner.state_mut());
    derive_display_state(runner.state_mut());
    assert_eq!(
        power_toughness(&runner, foe_bear),
        (2, 2),
        "removing the saurian counter must end the for-as-long-as effect (CR 611.2b)"
    );
}

/// R4 (negative sibling mode): mode 1 ("Cure Cancer") gains 3 life and
/// modifies no creature. The life-gain positive is the reach-guard for the
/// no-creature-modified negatives.
#[test]
fn sauron_mode_one_gains_life_and_touches_no_creature() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let sauron = scenario
        .add_creature_from_oracle(P0, "Sauron, Dino Devotee", 4, 4, SAURON_ORACLE)
        .id();
    let foe_bear = scenario.add_creature(P1, "Foe Bear", 2, 2).id();
    let _foe_wolf = scenario.add_creature(P1, "Foe Wolf", 3, 1).id();

    let mut runner = scenario.build();
    let p0_life_before = runner.state().players[0].life;

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(sauron, AttackTarget::Player(P1))])
        .expect("declare attackers");

    // Choose mode 1 ("Cure Cancer", index 0) — no target prompt.
    drive_attack_trigger(&mut runner, &[0], None);

    // Positive reach-guard: the chosen mode resolved (life +3).
    assert_eq!(
        runner.state().players[0].life - p0_life_before,
        3,
        "mode 1 must gain its controller 3 life"
    );
    // Adjacent-mode negatives, guarded by the positive above.
    assert_eq!(
        power_toughness(&runner, foe_bear),
        (2, 2),
        "mode 1 must not modify any creature"
    );
    assert_eq!(
        saurian_counters(&runner, foe_bear),
        0,
        "mode 1 must not place a saurian counter"
    );
}

/// R5 (no-legal-target path): with no other creature on the battlefield, the
/// "Turn People into Dinosaurs" mode is illegal (CR 700.2b — "another target
/// creature" has no legal target) and must be excluded from the mode surface;
/// resolution proceeds via the legal mode (CR 603.3c).
#[test]
fn sauron_mode_two_illegal_without_another_creature() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // Board with ONLY Sauron: "another target creature" has no legal target.
    let sauron = scenario
        .add_creature_from_oracle(P0, "Sauron, Dino Devotee", 4, 4, SAURON_ORACLE)
        .id();

    let mut runner = scenario.build();
    let p0_life_before = runner.state().players[0].life;

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(sauron, AttackTarget::Player(P1))])
        .expect("declare attackers");

    // Drive to the mode surface and prove mode 2 is excluded there.
    let mut saw_mode_choice = false;
    for _ in 0..50 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .or_else(|_| runner.act(GameAction::OrderTriggers { order: vec![] }))
                    .expect("order triggers");
            }
            WaitingFor::AbilityModeChoice {
                unavailable_modes, ..
            } => {
                saw_mode_choice = true;
                // CR 700.2b: the targetless mode is surfaced as unavailable.
                assert_eq!(
                    unavailable_modes,
                    vec![1],
                    "mode 2 must be unavailable with no other creature on the battlefield"
                );
                // The engine must also REJECT an attempt to choose it.
                assert!(
                    runner
                        .act(GameAction::SelectModes { indices: vec![1] })
                        .is_err(),
                    "selecting the illegal mode must be rejected (CR 700.2b)"
                );
                break;
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!(
                "unexpected WaitingFor while driving to the mode surface: {}",
                other.variant_name()
            ),
        }
    }
    assert!(
        saw_mode_choice,
        "the trigger must surface a mode choice (mode 1 is still legal, so the \
         trigger is not dropped outright)"
    );

    // Resolution proceeds via the legal mode (CR 603.3c).
    drive_attack_trigger(&mut runner, &[0], None);
    assert_eq!(
        runner.state().players[0].life - p0_life_before,
        3,
        "the legal mode must still resolve"
    );
}
