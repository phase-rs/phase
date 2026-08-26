//! Runtime regression for the latent post-replacement-continuation leak in
//! the GainLife→GainLife amount-substitution class (Boon Reflection, Rhox
//! Faithmender, Heron of Hope, Angel of Vitality, Alhammarret's Archive
//! life-half).
//!
//! Before the round-3 fix:
//!   1. `gain_life_applier` Branch 2 substituted `LifeGain.amount` from the
//!      execute's `Effect::GainLife { Multiply { 2, EventContextAmount } }`.
//!   2. `mandatory_post_effect` stashed the same execute as a Template
//!      continuation because `first_non_modifier_ability` walked past
//!      modifiers and returned the GainLife AST.
//!   3. `apply_life_gain` Execute arm did NOT drain — only the Prevented arm
//!      called `drain_substitution_continuation`.
//!   4. The Template sat in `state.post_replacement_continuation()` and drained
//!      on the next zone-change / land-play as a phantom GainLife on the
//!      wrong player. Same defect class as the Jace WinTheGame leak.
//!
//! The fix:
//!   - Extend the `post_effect` filter in `replacement.rs` to suppress the
//!     redundant stash for `(LifeGain, GainLife)` — the applier already
//!     substituted the amount.
//!   - Add the Execute-arm drain in `apply_life_gain` (mirrors the Prevented
//!     arm's existing drain).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// CR 614.6 + CR 119.3: Boon Reflection on the battlefield doubles a 3-life
/// gain to 6. The Execute arm must (a) deliver the doubled amount, (b) not
/// leak a Template continuation that would drain as a phantom GainLife on
/// the next zone change.
#[test]
fn boon_reflection_doubles_gain_and_does_not_leak_continuation() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Boon Reflection", Zone::Battlefield, db);
    // P1 needs a non-empty library so its own SBAs stay inert.
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;

    let life_before = runner.state().players[0].life;

    // Drive a production-path life-gain event through the replacement
    // pipeline. `apply_life_gain` is the single entry point for every
    // life-gain effect in the engine (`effects::life::resolve_gain`,
    // lifelink combat damage, ETB triggers, ability activations), so
    // exercising it here covers the class.
    let mut events = Vec::new();
    let gained =
        engine::game::effects::life::apply_life_gain(runner.state_mut(), P0, 3, &mut events)
            .expect("life gain must resolve without deferring on replacement choice");

    assert_eq!(
        gained, 6,
        "Boon Reflection must double 3 → 6 (CR 614.6 amount substitution)"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before + 6,
        "controller's life must reflect the doubled gain"
    );
    // The load-bearing assertion: a leaked Template here is the latent
    // GainLife class's analogue of the Jace empty-library win bug.
    assert!(
        runner.state().post_replacement_continuation().is_none(),
        "GainLife → GainLife amount-substitution must not leak a \
         post-replacement continuation; found {:?}",
        runner.state().post_replacement_continuation()
    );
}

/// CR 614.6: Repeat the leak assertion across a second life-gain event in the
/// same game. The first gain's continuation must be fully drained before the
/// second event runs — otherwise the second gain would re-fire the first's
/// stashed Template. This pins the per-event drain semantics, not just
/// "drains once at end."
#[test]
fn boon_reflection_two_sequential_gains_do_not_compound_or_leak() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Boon Reflection", Zone::Battlefield, db);
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;

    let life_before = runner.state().players[0].life;

    let mut events = Vec::new();
    let first =
        engine::game::effects::life::apply_life_gain(runner.state_mut(), P0, 2, &mut events)
            .expect("first life gain must resolve");
    assert_eq!(first, 4, "first gain doubles to 4");
    assert!(
        runner.state().post_replacement_continuation().is_none(),
        "first gain must not leak a continuation; found {:?}",
        runner.state().post_replacement_continuation()
    );

    let second =
        engine::game::effects::life::apply_life_gain(runner.state_mut(), P0, 5, &mut events)
            .expect("second life gain must resolve");
    assert_eq!(second, 10, "second gain doubles to 10");
    assert!(
        runner.state().post_replacement_continuation().is_none(),
        "second gain must not leak a continuation; found {:?}",
        runner.state().post_replacement_continuation()
    );

    assert_eq!(
        runner.state().players[0].life,
        life_before + 4 + 10,
        "total gained life is 4 + 10 = 14; any compounding would indicate the \
         first gain's stashed Template re-fired on the second event"
    );
}
