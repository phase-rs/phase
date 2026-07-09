//! Coastal Wizard / Lady Sun — "{T}: Return this creature and another target
//! creature to their owners' hands."
//!
//! The compound bounce split on " and " into "Return ~" (bounce self) plus the
//! verbless second conjunct "another target creature", which
//! `try_split_targeted_compound`'s verb-carry-forward dropped: its guard only
//! fired for a `"target "` head, not `"another target "`. So only the source was
//! returned; the targeted creature was orphaned (`Effect::Unimplemented`).
//!
//! The fix extends the carry-forward guard to `"another target "` (CR 608.2c;
//! "another target creature" is a standard `FilterProp::Another` target already
//! used by ~249 cards), so the second conjunct re-parses as "Return another
//! target creature" -> a real bounce.
//!
//! This drives the real activate -> resolve pipeline and asserts BOTH the source
//! AND the other creature return to hand (the sole legal "another target
//! creature" is auto-chosen). Reverting the fix leaves only the source bounced,
//! so the second assertion flips.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const COASTAL_WIZARD: &str = "{T}: Return this creature and another target creature to their \
    owners' hands. Activate only during your turn, before attackers are declared.";

#[test]
fn coastal_wizard_returns_self_and_another_target_to_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wizard = scenario
        .add_creature_from_oracle(P0, "Coastal Wizard", 1, 1, COASTAL_WIZARD)
        .id();
    // The "another target creature" — a distinct creature (FilterProp::Another
    // excludes the source; as the sole legal target it is auto-chosen).
    let other = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: wizard,
            ability_index: 0,
        })
        .expect("activating Coastal Wizard's {T} ability must succeed");
    runner.advance_until_stack_empty();

    // CR 608.2c: both conjuncts resolve — the source AND the targeted creature
    // return to their owners' hands.
    assert_eq!(
        runner.state().objects[&wizard].zone,
        Zone::Hand,
        "Coastal Wizard (this creature) must return to its owner's hand",
    );
    assert_eq!(
        runner.state().objects[&other].zone,
        Zone::Hand,
        "the 'another target creature' must ALSO return to hand — this fails if the \
         second bounce conjunct is dropped",
    );
}
