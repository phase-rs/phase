//! Stack-budget regression for `GameState`'s inline size.
//!
//! `phase-server` moves `GameState` **by value** through the action + AI path:
//! `pre_action_state` (`server-core/session.rs`) and `boundary_snapshot`
//! (`game/engine.rs`, inside `apply_action_boundary_with_stack_limit`) are live
//! simultaneously, and the AI planner adds one live `Option<GameState>` per
//! search ply. A single `apply()` on a four-player Commander table therefore
//! keeps many `GameState` values alive on one frame chain, and inline size is
//! multiplied by every live slot.
//!
//! Overrunning that budget is **not** a catchable panic — Rust's stack-overflow
//! handler `abort()`s the process, which `panic = "unwind"` in
//! `[profile.server-release]` cannot contain. In production that means every
//! player at the table loses the game. So the compile-time ceilings in
//! `types/game_state_size.rs` are not sufficient on their own: this test pins
//! the *runtime* consequence of the layout, through the real `apply()`
//! pipeline.
//!
//! Shape adopted from `combo_infinite_pile.rs` (`thread::Builder` +
//! reach-guard). The explicit builder stack is what bounds the depth, so the
//! result is independent of libtest's `RUST_MIN_STACK`. Under nextest's
//! process-per-test isolation a guard-page abort stays attributable here.
//!
//! # Calibration
//!
//! Bisected against this exact fixture on a **debug** build
//! (nightly-2026-04-19, aarch64-apple-darwin), by reverting the boxing change
//! in the working tree and re-applying it:
//!
//! | layout | `size_of::<GameState>()` | this fixture needs | at 3 MiB |
//! |---|---:|---:|---|
//! | pre-fix (inline `ResolvedAbility`) | 30,112 B | > 3,328 KiB, <= 3,584 KiB | **abort** |
//! | post-fix (boxed) | 12,464 B | > 2,304 KiB, <= 2,560 KiB | **pass** |
//!
//! 3 MiB sits inside the discriminating window `[2,560 KiB, 3,328 KiB]`.
//! Reverting the boxing flips this test to a process abort, which is the
//! strongest available non-vacuity signal.
//!
//! **Read the ratio honestly.** `size_of::<GameState>()` fell by 2.42x, but
//! this fixture's stack high-water fell only ~1.36x. The high-water is
//! therefore **not** proportional to the struct size, and no claim in this file
//! depends on it being so.
//!
//! **The residual is not attributed.** Nobody has instrumented it, so treat the
//! following as ranked hypotheses, not findings:
//!
//!   1. **By-value `ResolvedAbility` parameters — still size-scaling.** This
//!      change boxed `ResolvedAbility` at every *storage* site but left the
//!      ~33 by-value *parameter* sites alone (13 of them in
//!      `game/casting_costs.rs`), and it unboxes into them at the call sites.
//!      They nest on the path this very fixture drives: casting Murder reaches
//!      `casting_costs::check_additional_cost_or_pay` (`ability:
//!      ResolvedAbility`), which calls
//!      `check_additional_cost_or_pay_with_distribute` (also by value) — two
//!      live 5,264 B frames from one cast. `finish_pending_cast_cost_or_pay`
//!      takes one by value only to `Box::new` it immediately. So part of the
//!      remainder almost certainly *does* scale with `ResolvedAbility`; the
//!      earlier claim that it does not was wrong.
//!   2. Recursion depth and per-frame overhead that genuinely does not scale
//!      with any one type.
//!
//! Hypothesis 1 is a real follow-up with a measurable prize, not a dead end.
//! Instrument before acting on either.
//!
//! Because the window is a ~30% band rather than an order of magnitude, the
//! bound is deliberately taken near the top of it: that maximises post-fix
//! headroom while still going red pre-fix.
//!
//! # Why this test is gated to aarch64
//!
//! The discriminating window is `[2,560 KiB, 3,328 KiB]` — only ~30% wide — and
//! it was measured on **one** target. There is no safer number available: going
//! above 3,328 KiB makes the *pre-fix* layout pass too, at which point the test
//! stops discriminating and becomes decoration. So the bound cannot be widened
//! to absorb an unmeasured platform delta.
//!
//! Debug frame sizes are target-dependent (ABI, register pressure, spill
//! decisions), and `[profile.test]` inherits `dev`, so nothing is optimized
//! away. CI runs `ubuntu-latest` — x86_64-unknown-linux-gnu, never measured
//! here. Running an uncalibrated bound there does not fail politely: a stack
//! overflow `abort()`s, so the symptom is a **SIGABRT with no assertion
//! message** on whatever unrelated PR happens to be in the queue, and the
//! documented remedy ("re-run the bisection") is hours of work for someone with
//! no context for it.
//!
//! Gating to the target it was calibrated on keeps it a precise instrument
//! where the number means something, instead of a coin flip where it does not.
//! To un-gate: run the same bisection on the new target, add its row to the
//! calibration table above, and set `BOUNDED_STACK_BYTES` from the
//! *intersection* of all measured windows. Cross-compiling is not enough — the
//! bisection has to execute.
#![cfg(target_arch = "aarch64")]

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::format::FormatConfig;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::{PlayerId, WaitingFor};

/// See the calibration table in the module docs. Not a guess — bisected with
/// the boxing reverted and re-applied.
const BOUNDED_STACK_BYTES: usize = 3 << 20;

const MURDER_ORACLE: &str = "Destroy target creature.";

/// A death trigger on every seat, so the measured resolution carries a real
/// trigger cascade rather than a bare removal. Deliberately non-targeting: a
/// targeted drain would add a target-selection pause and cut the run short of
/// the deep resolution path this test exists to measure.
const DEATH_TRIGGER_ORACLE: &str =
    "Whenever this creature or another creature dies, you gain 1 life.";

#[test]
fn four_player_commander_action_fits_a_bounded_stack() {
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 4, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // A populated four-player Commander board: every seat contributes objects
    // and a death trigger, so the resolution under measurement fans out.
    for seat in [P0, P1, PlayerId(2), PlayerId(3)] {
        scenario.add_creature_from_oracle(seat, "Zulaport Cutthroat", 0, 1, DEATH_TRIGGER_ORACLE);
        scenario.add_vanilla(seat, 2, 2);
        scenario.add_vanilla(seat, 3, 3);
    }
    let victim = scenario.add_creature(P1, "Doomed Bystander", 4, 4).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();

    // Reach-guards: without these, a fixture that never reached the cast would
    // run a no-op on the bounded stack and pass for any layout.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "reach-guard: P0 holds priority before the measured action, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&victim)
            .map(|object| object.zone),
        Some(Zone::Battlefield),
        "reach-guard: the removal target is on the battlefield before the action"
    );
    let life_before = runner.life(P0);

    let handle = std::thread::Builder::new()
        .stack_size(BOUNDED_STACK_BYTES)
        .spawn(move || {
            runner.cast(murder).target_objects(&[victim]).resolve();
            runner.advance_until_stack_empty();
            runner
        })
        .expect("spawn bounded-stack action thread");
    let runner = handle.join().expect(
        "a four-player Commander cast + resolve must NOT overflow a 3 MiB stack \
         (see the calibration table in this file's module docs)",
    );

    // Positive outcome assertions: the measured action really did resolve.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&victim)
            .map(|object| object.zone),
        Some(Zone::Graveyard),
        "Murder resolved and put the target in the graveyard"
    );
    assert!(
        runner.life(P0) > life_before,
        "the death triggers resolved and gained P0 life (before {life_before}, after {})",
        runner.life(P0)
    );
}
