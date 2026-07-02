//! Runtime regression for issue #1375 — Twilight Prophet's upkeep drain binds
//! the *revealed* card's mana value via the same-resolution anaphora path, not
//! an (empty) target slot.
//!
//! Twilight Prophet: "At the beginning of your upkeep, if you have the city's
//! blessing, reveal the top card of your library and put it into your hand.
//! Each opponent loses X life and you gain X life, where X is that card's mana
//! value." The "that card" refers to the *revealed* card (CR 608.2c anaphora),
//! which is NOT a target (CR 115.10a — no "target" word appears).
//!
//! Root cause (pre-fix): the "where X is that card's mana value" binding was
//! classified by `parse_cda_quantity` → `parse_mana_value_reference_qty`, which
//! hard-mapped "that card's mana value" to `ObjectScope::Target`. At runtime the
//! upkeep trigger has no object target, so the target-slot read yields nothing.
//! The observable symptom (confirmed by a runtime investigation): with the bug,
//! **the caster gains 0 instead of the revealed card's mana value**. The loss
//! side already "works" only by a target-slot coincidence — the LoseLife slot
//! happens to carry the revealed card — but the deeper chained GainLife's target
//! slot is empty, so `ObjectScope::Target` reads nothing there and P0 gains 0.
//!
//! Fix: in `parse_where_x_quantity_expression`, route ONLY the literal "that
//! card's mana value" (and the "converted mana cost" synonym) through
//! `parse_event_context_quantity`, which classifies the demonstrative referent
//! as `ObjectScope::Demonstrative`. That scope resolves via
//! `effect_context_object` — the revealed card, LKI-snapshotted before the
//! chained `ChangeZone` moves it to hand (CR 608.2h) — yielding the correct
//! mana value (CR 202.3) for BOTH the loss and the gain.
//!
//! Guard load-bearing at runtime: Twilight Prophet is built here via LIVE PARSE
//! (`add_creature_from_oracle`), so the `lower.rs` guard governs the scope of
//! the parsed where-X binding *at test time* — not a pre-baked DB fixture. The
//! library-top MV-5 card (Baneslayer Angel) is a real DB card so it carries a
//! genuine nonzero mana value. Reverting the guard (Demonstrative → Target)
//! flips the gain binding to an empty slot and makes P0 gain 0 — this test then
//! fails on the gain assertion. (Proven by temporarily toggling the guard OFF.)
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 608.2c: a spell or ability's controller follows its instructions in
//!     the order written; anaphora reads the whole text per English rules.
//!   - CR 115.10a: an affected object is not a target unless "target" is used.
//!   - CR 202.3: the mana value of an object equals the total mana in its cost.
//!   - CR 119.3: life gain/loss adjusts the affected player's life total.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Twilight Prophet's real upkeep-ability Oracle text, LIVE-PARSED so the
/// `lower.rs` where-X guard governs the "that card's mana value" scope at
/// runtime (the whole point of #1375). Includes the Ascend reminder so the
/// keyword parses; the runtime trigger is the upkeep drain.
const TWILIGHT_PROPHET_ORACLE: &str = "Ascend (If you control ten or more permanents, you get the city's blessing for the rest of the game.)\nAt the beginning of your upkeep, if you have the city's blessing, reveal the top card of your library and put it into your hand. Each opponent loses X life and you gain X life, where X is that card's mana value.";

/// CR 608.2c + CR 202.3 — Twilight Prophet's upkeep trigger reveals the top
/// card of P0's library; each opponent loses that card's mana value and P0
/// gains that much.
///
/// Discriminator (gain side is the load-bearing one): the library top is
/// Baneslayer Angel (mana value 5). With the bug the caster GAINS 0 instead of
/// 5 — the empty target slot the reverted guard binds. P0 must end at 25
/// (20 + 5) and the opponent at 15 (20 − 5).
#[test]
fn twilight_prophet_upkeep_drains_revealed_card_mana_value() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    // LIVE PARSE the drain-carrying permanent so the lower.rs guard is
    // load-bearing at runtime (Twilight Prophet is a 2/4 Vampire Cleric).
    // Twilight Prophet is intentionally NOT in the base fixture, so the later
    // `rehydrate_game_from_card_db` never re-stamps this live-parsed face.
    scenario.add_creature_from_oracle(P0, "Twilight Prophet", 2, 4, TWILIGHT_PROPHET_ORACLE);
    // Baneslayer Angel is {3}{W}{W} — mana value 5. A real DB card so it carries
    // a genuine nonzero mana value; it is the revealed library-top card.
    let revealed = scenario.add_real_card(P0, "Baneslayer Angel", Zone::Library, db);
    // Keep a card under Baneslayer Angel so moving the revealed card to hand
    // does not empty P0's library and deck them out on the draw step.
    scenario.add_real_card(P0, "Island", Zone::Library, db);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    let mut runner = scenario.build();
    // Hydrate the real DB cards (Baneslayer Angel / Island) so their printed
    // mana values are populated. Twilight Prophet is absent from the DB, so its
    // live-parsed upkeep trigger is untouched.
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // CR 702.131a: grant P0 the city's blessing so the trigger's intervening-if
    // ("if you have the city's blessing") is satisfied.
    runner.state_mut().city_blessing.insert(P0);

    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // Precondition for the discriminator: the revealed library-top card has a
    // genuine nonzero mana value of 5.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&revealed)
            .unwrap()
            .mana_cost
            .mana_value(),
        5,
        "precondition: the revealed library-top card (Baneslayer Angel) has mana value 5"
    );
    assert_eq!(runner.life(P0), 20, "precondition: P0 starts at 20 life");
    assert_eq!(runner.life(P1), 20, "precondition: P1 starts at 20 life");

    // Drive Untap → Upkeep (trigger fires) → resolve the non-targeted trigger.
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    // CR 119.3 + CR 202.3: P0 GAINS the revealed card's mana value. This is the
    // load-bearing assertion — with the bug (guard reverted to Target) the gain
    // side reads an empty target slot and P0 gains 0 (stays at 20).
    assert_eq!(
        runner.life(P0),
        25,
        "P0 must GAIN life equal to the revealed card's mana value (5): 20 + 5 = 25. \
         With the #1375 bug the caster gains 0 (the reverted guard binds an empty \
         target slot for the you-gain half of the drain)."
    );
    // CR 119.3 + CR 202.3: each opponent loses that same amount.
    assert_eq!(
        runner.life(P1),
        15,
        "each opponent must lose life equal to the revealed card's mana value (5): 20 - 5 = 15"
    );

    // SHAPE sub-assertion: the revealed card ended up in P0's hand.
    assert_eq!(
        runner.state().objects.get(&revealed).unwrap().zone,
        Zone::Hand,
        "the revealed card must be put into P0's hand"
    );
}
