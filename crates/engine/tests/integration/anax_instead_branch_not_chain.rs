//! CR 614.1a + CR 614.6 — an "instead" override is a BRANCH, never a sequel.
//!
//! CR 614.1a: "Effects that use the word 'instead' are replacement effects."
//! CR 614.6:  "If an event is replaced, it never happens. A modified event
//!             occurs instead."
//!
//! The class defect this pins: when the parser recognizes the "if <cond>,
//! <body> instead" override grammar but cannot lower <cond> into a typed
//! `AbilityCondition`, it used to fall through and re-emit <body> as an
//! ordinary unconditional chain clause. The engine then ran the replaced effect
//! AND its replacement — both branches, every time — with the condition dropped
//! entirely.
//!
//! Witness (Oracle read from the pool export, not from memory):
//!   Anax, Hardened in the Forge —
//!     "Whenever Anax or another nontoken creature you control dies, create a
//!      1/1 red Satyr creature token with "This token can't block." If the
//!      creature had power 4 or greater, create two of those tokens instead."
//!
//! Its base parse was `Token{count:1} -> sub_ability Token{count:2}` with
//! `condition: null` on BOTH defs, so a 2/2 dying produced THREE Satyrs.
//!
//! The assertion below is the CR-correct outcome for the power < 4 case (one
//! token), so it stays valid whether the override is honestly unimplemented or
//! is later lowered to a real conditional branch. It only fails if the override
//! body runs unconditionally — exactly the defect.

use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

/// Oracle text as printed (read from the full-pool export).
const ANAX: &str = "Anax's power is equal to your devotion to red.\nWhenever Anax or another nontoken creature you control dies, create a 1/1 red Satyr creature token with \"This token can't block.\" If the creature had power 4 or greater, create two of those tokens instead.";

fn satyr_token_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|obj| {
                obj.is_token
                    && obj
                        .card_types
                        .subtypes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case("Satyr"))
            })
        })
        .count()
}

/// Mark `victim` with lethal damage, run SBAs so it dies, process the resulting
/// triggers, then resolve the stack.
fn kill_and_resolve(runner: &mut GameRunner, victim: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&victim)
        .expect("victim exists")
        .damage_marked = 99;

    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();
}

/// CR 614.6: a creature with power < 4 dies, so the "create two of those tokens
/// instead" override does NOT apply. Exactly one Satyr must be created.
///
/// RED before the fix: the override body was chained unconditionally, so the
/// engine created 1 + 2 = THREE Satyrs on every nontoken creature death.
#[test]
fn anax_instead_override_does_not_run_unconditionally() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Anax, Hardened in the Forge", 0, 7, ANAX);
    // A nontoken creature with power < 4 — the override's condition is FALSE.
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    assert_eq!(
        satyr_token_count(&runner),
        0,
        "no Satyr tokens before the trigger"
    );

    kill_and_resolve(&mut runner, bear);

    assert_eq!(
        satyr_token_count(&runner),
        1,
        "CR 614.6: the dying creature had power 2, so the \"create two of those \
         tokens instead\" override must NOT apply — exactly one Satyr. Three \
         Satyrs means the override body ran as an unconditional chain sibling \
         (both branches), which is the CR 614 lowering defect this test pins."
    );
}
