//! Runtime regression for issue #1534 — Duskmantle Seer's upkeep trigger must
//! make EACH player (not just the controller) reveal the top card of THEIR OWN
//! library and lose life equal to THAT card's mana value.
//!
//! Duskmantle Seer: "At the beginning of your upkeep, each player reveals the
//! top card of their library, loses life equal to that card's mana value, then
//! puts it into their hand."
//!
//! This is the `player_scope: All` analog of Dark Confidant (which reveals only
//! the controller's library). The discriminating property is the *per-player*
//! binding: when the `player_scope` driver rebinds the acting controller to
//! each player in turn, the `RevealTop { player: Controller }` clause must
//! reveal that player's library, and the chained
//! `LoseLife { ObjectManaValue { Demonstrative } }` must read THAT player's
//! revealed card — not a single shared card, and not zero.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 608.2: a spell or ability's controller follows its instructions in
//!     the order written; for "each player", the instruction is performed once
//!     for each player (CR 608.2 / CR 101.4 APNAP per-player iteration).
//!   - CR 608.2c: an anaphoric noun phrase ("that card's mana value") binds to
//!     the object the earlier instruction (the per-player reveal) introduced.
//!   - CR 608.2h: once the revealed card moves to a hidden zone (Hand), the
//!     effect uses last-known information for its mana value.
//!   - CR 701.20b: revealing a card doesn't cause it to leave its zone.
//!   - CR 119.3: a player loses the specified amount of life.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// CR 608.2 + CR 608.2c — Duskmantle Seer's upkeep trigger makes each player
/// reveal their own library top and lose life equal to that card's mana value.
///
/// Discriminator: P0's library top is Bonesplitter (MV 1); P1's library top is
/// Balance (MV 2). The two MVs differ from each other and from Duskmantle
/// Seer's own MV (4). If the per-player binding is wrong, the symptom is
/// observable:
///   - only the controller loses life  → P1 stays at 20.
///   - both lose the same (controller's) card MV → P1 loses 1, not 2.
///   - the demonstrative referent is empty → either loses 0.
///
/// Correct behavior: P0 ends at 19 (20 − 1), P1 ends at 18 (20 − 2), and each
/// player's revealed card ends up in their own hand.
#[test]
fn duskmantle_seer_each_player_loses_life_for_their_own_revealed_card() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    let seer = scenario.add_real_card(P0, "Duskmantle Seer", Zone::Battlefield, db);
    // P0's library top: Bonesplitter — mana value 1.
    let p0_top = scenario.add_real_card(P0, "Bonesplitter", Zone::Library, db);
    // P1's library top: Balance — mana value 2, distinct from P0's.
    let p1_top = scenario.add_real_card(P1, "Balance", Zone::Library, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Pre-existing battlefield permanent — entered a previous turn.
    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // Confirm the discriminator preconditions.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&seer)
            .unwrap()
            .mana_cost
            .mana_value(),
        4,
        "precondition: Duskmantle Seer's own MV is 4"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&p0_top)
            .unwrap()
            .mana_cost
            .mana_value(),
        1,
        "precondition: P0's library top (Bonesplitter) has MV 1"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&p1_top)
            .unwrap()
            .mana_cost
            .mana_value(),
        2,
        "precondition: P1's library top (Balance) has MV 2"
    );
    assert_eq!(runner.life(P0), 20, "precondition: P0 starts at 20 life");
    assert_eq!(runner.life(P1), 20, "precondition: P1 starts at 20 life");

    // Drive Untap → Upkeep (trigger fires + resolves) → Draw → PreCombatMain.
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    // CR 608.2 — each player reveals their OWN top card and loses life equal to
    // THAT card's mana value (per-player binding), not just the controller.
    assert_eq!(
        runner.life(P0),
        19,
        "P0 must lose life equal to P0's revealed card's MV (Bonesplitter = 1): 20 - 1 = 19"
    );
    assert_eq!(
        runner.life(P1),
        18,
        "P1 must lose life equal to P1's OWN revealed card's MV (Balance = 2): 20 - 2 = 18"
    );

    // SHAPE sub-assertion: each revealed card ended up in its owner's hand.
    assert_eq!(
        runner.state().objects.get(&p0_top).unwrap().zone,
        Zone::Hand,
        "P0's revealed card must be put into P0's hand"
    );
    assert_eq!(
        runner.state().objects.get(&p1_top).unwrap().zone,
        Zone::Hand,
        "P1's revealed card must be put into P1's hand"
    );
}
