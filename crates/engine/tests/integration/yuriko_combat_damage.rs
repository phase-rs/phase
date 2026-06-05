//! Runtime regression for the Yuriko, the Tiger's Shadow combat-damage trigger
//! — the bare-anaphoric-possessive sibling of Dark Confidant (issue #511).
//!
//! Oracle text: "Whenever a Ninja you control deals combat damage to a player,
//! reveal the top card of your library and put that card into your hand. Each
//! opponent loses life equal to that card's mana value."
//!
//! Before the fix, "that card's mana value" parsed to
//! `ObjectManaValue { scope: ObjectScope::CostPaidObject }`. At runtime that
//! scope's fallback order is `cost_paid_object → trigger-event source →
//! effect_context_object`. Yuriko's trigger has no cost-paid object, so slot 2
//! (the trigger-event source = the *Ninja that dealt combat damage*) won —
//! and the opponent lost life equal to the Ninja's mana value instead of the
//! revealed card's. When Yuriko herself was the attacker the discrepancy
//! manifested as the user's reported bug: "it does her mana cost in damage
//! [sic; life loss] instead of the revealed card's mana cost."
//!
//! Fix: `classify_possessive_referent` in `parser/oracle_quantity.rs` now
//! returns `ObjectScope::Anaphoric` for the bare anaphoric prefix class
//! ("that card", "that creature", "that spell", "the creature"). The runtime
//! `Anaphoric` arm of `resolve_object_mana_value` reads
//! `effect_context_object` first (the revealed card snapshot stamped by the
//! parent `RevealTop` → child `ChangeZone` chain), then the trigger source,
//! then `cost_paid_object` — the inverse slot order, which is what CR 608.2c
//! requires for anaphora introduced by an earlier instruction in the same
//! ability.
//!
//! CR references (verified against `docs/MagicCompRules.txt`):
//!   - CR 119.3: "lose life" decreases the player's life total. The authorizing
//!     rule for the trigger's effect — life loss, not damage. (The reporter
//!     said "damage" but Yuriko's text reads "loses life".)
//!   - CR 119.2: damage to a player normally causes that player to lose life.
//!   - CR 510.1b + CR 510.2: a creature deals combat damage equal to its power
//!     in the combat damage step (the trigger event for Yuriko).
//!   - CR 603.2: a triggered ability's condition is checked against game events;
//!     the trigger-event source is the object referenced by that event (the
//!     Ninja that dealt combat damage, in Yuriko's case).
//!   - CR 603.7c: the controller follows a triggered ability's instructions in
//!     the order written.
//!   - CR 608.2c: anaphora binds to the earlier-instruction referent in the
//!     same resolution (the revealed card, not the trigger source).
//!   - CR 608.2h: LKI applies once the referenced object moves to a hidden
//!     zone (the revealed card moves Library → Hand during the same chain).
//!   - CR 608.2k: an effect can refer to a specific untargeted object named
//!     by the ability's cost or trigger condition — this is the *other*
//!     anaphora rule that the participle-possessive class ("the sacrificed
//!     creature's power") binds to, but NOT the rule that licenses "that
//!     card's" after a reveal.
//!   - CR 701.20 + CR 701.20b: revealing a card does not move it from its
//!     zone; the parent `RevealTop` followed by a `ChangeZone` to Hand is the
//!     reveal-then-move primitive that stamps the revealed object into
//!     `effect_context_object` for the chained life-loss instruction.
//!   - CR 202.3: an object's mana value is the converted total of its mana
//!     cost — the quantity the resolver reads off the revealed card.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::run_combat;

use crate::support::shared_card_db as load_db;

/// Primary regression — Yuriko herself attacks unblocked and deals combat
/// damage to P1. The combat-damage trigger reveals the top card of P0's
/// library and P1 must lose life equal to *that revealed card's* mana value,
/// NOT Yuriko's own mana value.
///
/// Discriminator: library top = `Cryptic Command` (CMC 4), distinct from
/// Yuriko's CMC of 3. Yuriko is a 1/3, so combat damage to P1 = 1.
/// Total P1 life loss = 1 (combat, CR 119.2/510.1b) + 4 (trigger reveal,
/// CR 119.3 + 608.2c) = 5. P1 ends at 20 − 5 = 15.
///   - Under the pre-fix `CostPaidObject` scope, slot 2 wins → the trigger
///     reads Yuriko's mana value (3): total 1 + 3 = 4, P1 ends at 16. The
///     15-vs-16 gap is the exact symptom the reporter observed: the trigger
///     reads the wrong CMC.
///   - Under the fixed `Anaphoric` scope, slot 1 (`effect_context_object` =
///     the revealed `Cryptic Command` LKI snapshot) wins → total 1 + 4 = 5,
///     P1 ends at 15.
#[test]
fn yuriko_attacks_loses_opponent_life_equal_to_revealed_card_mana_value() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let yuriko = scenario.add_real_card(P0, "Yuriko, the Tiger's Shadow", Zone::Battlefield, db);
    // Cryptic Command is {1}{U}{U}{U} — mana value 4, distinct from Yuriko's 3.
    let revealed = scenario.add_real_card(P0, "Cryptic Command", Zone::Library, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    assert_eq!(
        runner
            .state()
            .objects
            .get(&yuriko)
            .unwrap()
            .mana_cost
            .mana_value(),
        3,
        "precondition: Yuriko's own CMC is 3 (the bug-displaying value)"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&revealed)
            .unwrap()
            .mana_cost
            .mana_value(),
        4,
        "precondition: the revealed library-top card has CMC 4"
    );
    assert_eq!(runner.life(P1), 20, "precondition: P1 starts at 20 life");

    // CR 510.1b — Yuriko (1/3) attacks unblocked → 1 combat damage to P1 →
    // the trigger condition fires (CR 603.2 + Oracle: "Whenever a Ninja you
    // control deals combat damage to a player"). The reveal + life-loss
    // chain then resolves.
    run_combat(&mut runner, vec![yuriko], vec![]);
    runner.advance_until_stack_empty();

    // CR 119.2 + CR 119.3 + CR 510.1b + CR 608.2c — P1's life total drops by
    // the combat damage (1 from Yuriko's power) PLUS the revealed card's
    // mana value (4). Pre-fix the second slot read Yuriko's CMC (3), so P1
    // would have ended at 20 - 1 - 3 = 16. Post-fix the resolver reads the
    // revealed card's CMC (4), so P1 ends at 20 - 1 - 4 = 15. The 15-vs-16
    // gap is the user-reported symptom.
    assert_eq!(
        runner.life(P1),
        15,
        "P1 loses 1 combat damage + life equal to the REVEALED card's mana \
         value (4): 20 - 1 - 4 = 15. The bug-value would be 16 (loses 1 + \
         Yuriko's CMC 3)."
    );

    // CR 701.20 + CR 401/402 — the revealed card ends up in P0's hand.
    assert_eq!(
        runner.state().objects.get(&revealed).unwrap().zone,
        Zone::Hand,
        "the revealed card must be put into P0's hand"
    );

    // CR 119.3 — P0's life is untouched by Yuriko's own opponent's life-loss
    // clause. ("Each opponent loses life…" scopes to opponents of the
    // controller, not the controller.)
    assert_eq!(
        runner.life(P0),
        20,
        "P0 must not lose life; the clause says 'each opponent'"
    );
}

/// Secondary regression — a *different* Ninja deals the combat damage. The
/// mana value of the attacking Ninja must NOT be read; the revealed card is
/// what counts.
///
/// Discriminator: the attacker is Thousand-Faced Shadow (a 1/1 Ninja, mana
/// value 1), and the revealed library top is a 2-CMC `Counterspell`. Combat
/// damage = 1 (Shadow's power); trigger life loss = 2 (revealed card's CMC).
/// Total: 20 − 3 = 17.
///   - Under the pre-fix `CostPaidObject` scope, slot 2 (trigger-event
///     source) would be read — that's the attacking Shadow (CMC 1) —
///     so P1 would lose 1 + 1 = 2 life, ending at 18.
///   - Under the fixed `Anaphoric` scope, slot 1 (`effect_context_object` =
///     the revealed Counterspell snapshot) is read — P1 loses 1 + 2 = 3
///     life, ending at 17.
///
/// The 17-vs-18 gap proves the chained life-loss reads the revealed card's
/// CMC and not the attacker's, even when the attacker is not Yuriko herself.
///
/// CR 603.3b: Ingenious Infiltrator carries its *own* combat-damage trigger
/// ("...draw a card"). Both same-controller triggers fire and P0 orders them;
/// whichever resolves first consumes the top of the library. The library is
/// therefore seeded with TWO mana-value-2 cards (Counterspell + Negate) so
/// that Infiltrator's draw and Yuriko's reveal each take a distinct CMC-2 card
/// regardless of resolution order — Yuriko's reveal always yields mana value 2.
/// A single library card would let the draw steal it, leaving Yuriko's reveal
/// empty (no anaphoric referent) and falling through to the trigger-event
/// source (the attacker, CMC 4) — the 14 bug-value masquerading as a fix
/// failure.
#[test]
fn another_ninja_attacks_yuriko_still_reads_revealed_card_mana_value() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _yuriko = scenario.add_real_card(P0, "Yuriko, the Tiger's Shadow", Zone::Battlefield, db);
    // Counterspell {U}{U} and Negate {1}{U} are both mana value 2, distinct
    // from Yuriko's 3 and Infiltrator's 4. Two cards so Infiltrator's own
    // "draw a card" trigger and Yuriko's reveal each take a CMC-2 card.
    let revealed = scenario.add_real_card(P0, "Counterspell", Zone::Library, db);
    let _drawn = scenario.add_real_card(P0, "Negate", Zone::Library, db);
    // Attacker — also a real Ninja so it matches the trigger condition
    // ("Whenever a Ninja you control deals combat damage to a player").
    // Thousand-Faced Shadow is a Ninja with mana cost {U} (mana value 1) and
    // no combat-damage trigger that can draw the intended reveal card first.
    // The discriminator is the gap to the 2-CMC reveal — under the buggy
    // `CostPaidObject` scope P1 would have lost 1 (the attacker's CMC); the
    // fixed `Anaphoric` scope makes P1 lose exactly 2.
    let attacker = scenario.add_real_card(P0, "Thousand-Faced Shadow", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    assert_eq!(
        runner
            .state()
            .objects
            .get(&revealed)
            .unwrap()
            .mana_cost
            .mana_value(),
        2,
        "precondition: the revealed library-top card has CMC 2"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&attacker)
            .unwrap()
            .mana_cost
            .mana_value(),
        1,
        "precondition: the attacking Ninja's CMC is 1 (the bug-displaying value)"
    );
    assert_eq!(runner.life(P1), 20, "precondition: P1 starts at 20 life");

    run_combat(&mut runner, vec![attacker], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        17,
        "P1 loses 1 combat damage (Shadow's power) + life equal to the \
         REVEALED card's mana value (2): 20 - 1 - 2 = 17. The bug-value \
         would be 18 (loses 1 + Shadow's CMC 1)."
    );
    assert_eq!(
        runner.state().objects.get(&revealed).unwrap().zone,
        Zone::Hand,
        "the revealed card must be put into P0's hand"
    );
}
