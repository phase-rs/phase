//! Implements the previously-unparsed static ability on The Eternal Wanderer:
//! "No more than one creature can attack The Eternal Wanderer each combat."
//!
//! Oracle text (verified against the live Scryfall API, 2026-08-28) confirms
//! this restricts how many ATTACKING creatures may be assigned to this one
//! specific planeswalker as a defender (CR 508.1c) — distinct from a
//! "can't attack you/planeswalkers" prohibition and from a
//! "can't be blocked by more than N creatures" restriction.
//!
//! This is a discriminator test: it drives `GameAction::DeclareAttackers`
//! through the real `apply()` legality gate
//! (`validate_declaration_core` -> `validate_per_defender_attacker_caps`).
//! Reverting the `AttackDefenderScope::ThisPermanent` cap (or the parser arm
//! that emits it) makes the first assertion below fail — two creatures would
//! be allowed to attack the same planeswalker.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// P1 controls the Wanderer (the defending permanent); P0 is the active
/// (attacking) player with three independent creatures. A fresh scenario per
/// declaration attempt avoids reconstructing mid-combat undo state.
fn setup() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let db = shared_card_db().expect("card database must be available");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wanderer = scenario.add_real_card(P1, "The Eternal Wanderer", Zone::Battlefield, db);
    let c1 = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let c2 = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let c3 = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.advance_to_combat();

    (runner, wanderer, c1, c2, c3)
}

#[test]
fn two_creatures_attacking_the_wanderer_is_illegal() {
    if shared_card_db().is_none() {
        return;
    }
    let (mut runner, wanderer, c1, c2, _c3) = setup();

    // NEGATIVE: two creatures attacking the SAME planeswalker in the same
    // combat is illegal (CR 508.1c) — the exact clause under test.
    let result = runner.declare_attackers(&[
        (c1, AttackTarget::Planeswalker(wanderer)),
        (c2, AttackTarget::Planeswalker(wanderer)),
    ]);
    assert!(
        result.is_err(),
        "no more than one creature may attack The Eternal Wanderer each \
         combat, got {result:?}"
    );
}

#[test]
fn one_creature_attacking_the_wanderer_is_legal() {
    if shared_card_db().is_none() {
        return;
    }
    let (mut runner, wanderer, c1, _c2, _c3) = setup();

    // POSITIVE reach-guard: the pair rejected above is meaningless unless
    // attacking this specific object is otherwise a legal action at all.
    let result = runner.declare_attackers(&[(c1, AttackTarget::Planeswalker(wanderer))]);
    assert!(
        result.is_ok(),
        "a single creature attacking The Eternal Wanderer must remain legal, \
         got {result:?}"
    );
}

#[test]
fn cap_does_not_restrict_attacks_on_the_wanderers_controller_or_other_creatures() {
    if shared_card_db().is_none() {
        return;
    }
    let (mut runner, wanderer, c1, c2, c3) = setup();

    // SIBLING (unaffected scope, CR 508.5a): the cap is scoped to THIS
    // permanent only — it does not restrict attacks against the Wanderer's
    // controller (P1) directly, nor against other creatures/players. One
    // creature on the Wanderer plus two more creatures attacking P1 directly
    // (a different `AttackTarget` variant entirely) must all be legal in the
    // SAME combat.
    let result = runner.declare_attackers(&[
        (c1, AttackTarget::Planeswalker(wanderer)),
        (c2, AttackTarget::Player(P1)),
        (c3, AttackTarget::Player(P1)),
    ]);
    assert!(
        result.is_ok(),
        "attacking the Wanderer's controller (or other creatures attacking a \
         player) must remain unrestricted by the ThisPermanent cap, got \
         {result:?}"
    );
}
