//! MSH Wave 2 — Captain America, Living Legend.
//!
//! "Whenever a creature you control becomes tapped during your turn, if it's the
//! first time that creature has become tapped this turn, untap it."
//!
//! Building-block coverage for the first-tap-this-turn intervening-if class
//! (CR 701.26 tap/untap + CR 603.4 intervening-if). The new
//! `TriggerCondition::FirstTimeObjectTappedThisTurn` reads a per-object tap-count
//! ledger (`GameState.object_tap_count_this_turn`) that is incremented at the
//! single trigger-collection chokepoint (`observe_object_taps`), so combat,
//! effect, and crew taps all count uniformly.
//!
//! Captain America is release-gated out of the local fixture, so this drives the
//! real parser + trigger pipeline from representative Oracle text. This test taps
//! the creature via a **combat attack** (not a tap effect), proving the observer
//! seam counts combat-declaration taps emitted directly by `combat.rs` (which
//! bypass `process_one_tap`).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::phase::Phase;

const CAPTAIN_AMERICA: &str = "Whenever a creature you control becomes tapped during your turn, \
     if it's the first time that creature has become tapped this turn, untap it.";

/// CR 701.26 + CR 603.4: on your turn, the first time a creature you control
/// becomes tapped (here, by being declared as an attacker — a combat tap emitted
/// directly by `combat.rs`), Captain America untaps it. Reverting the tap-count
/// ledger (`observe_object_taps`) leaves the count at 0, the intervening-if reads
/// `!= Some(1)`, the trigger never fires, and the attacker stays tapped.
#[test]
fn captain_america_untaps_first_combat_tap_on_your_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Captain America's tap-watcher trigger only (card type irrelevant to the
    // trigger; he is himself a creature you control).
    scenario
        .add_creature_from_oracle(P0, "Captain America, Living Legend", 4, 4, CAPTAIN_AMERICA)
        .id();

    // A separate creature you control to attack with — it taps on attack, which
    // is its first tap this turn.
    let attacker = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();

    let bear = runner
        .state()
        .objects
        .get(&attacker)
        .expect("attacker exists");
    assert!(
        !bear.tapped,
        "Captain America must untap the attacker on its first tap this turn"
    );
}
