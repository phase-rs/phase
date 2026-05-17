//! Regression test for the Tombstone Stairwell swallowed-clause fixes
//! (plan unit 7b-3).
//!
//! Tombstone Stairwell's line-2 ability is:
//!   `At the beginning of each upkeep, if this enchantment is on the
//!    battlefield, each player creates a 2/2 black Zombie creature token with
//!    haste named Tombspawn for each creature card in their graveyard.`
//!
//! Two clauses were silently dropped by the parser:
//!  - SUB-FIX B: the `if this enchantment is on the battlefield` intervening-`if`
//!    (CR 603.4) — `parse_zone_conditions` had no battlefield arm.
//!  - SUB-FIX A: the `for each creature card in their graveyard` per-player
//!    count multiplier — the parsed `ObjectCount` filter came back with
//!    `controller: None`, so it would have counted *all* graveyards combined
//!    instead of each iterating player's own graveyard.
//!
//! This test is the CR 109.4 proof: it drives the engine through a real upkeep
//! trigger with two players whose graveyards hold *different* numbers of
//! creature cards (2 and 3) and asserts each player creates exactly that many
//! Tombspawn tokens — i.e. each player's count is THEIR OWN graveyard's
//! creature cards, not the combined total and not the controller's count.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// CR 109.4 + CR 111.1 + CR 603.4: Tombstone Stairwell's "each player creates …
/// for each creature card in their graveyard" upkeep trigger creates, for each
/// player, a number of Tombspawn tokens equal to THAT player's own graveyard
/// creature-card count.
#[test]
fn tombstone_stairwell_creates_per_player_tokens_scaled_to_own_graveyard() {
    let mut scenario = GameScenario::new();
    // Start at Untap so advancing to the main phase passes through Upkeep,
    // firing the "at the beginning of each upkeep" trigger naturally.
    scenario.at_phase(Phase::Untap);

    // Tombstone Stairwell — only the line-2 trigger matters here. Card type is
    // irrelevant to triggered-ability resolution (see dalkovan_encampment test);
    // `add_creature_from_oracle` puts it on the battlefield so the
    // `if this enchantment is on the battlefield` intervening-if is satisfied.
    scenario.add_creature_from_oracle(
        P0,
        "Tombstone Stairwell",
        0,
        1,
        "At the beginning of each upkeep, if this enchantment is on the \
         battlefield, each player creates a 2/2 black Zombie creature token \
         with haste named Tombspawn for each creature card in their graveyard.",
    );

    // P0's graveyard: 2 creature cards. P1's graveyard: 3 creature cards.
    scenario.add_creature_to_graveyard(P0, "Grizzly Bear", 2, 2);
    scenario.add_creature_to_graveyard(P0, "Hill Giant", 3, 3);
    scenario.add_creature_to_graveyard(P1, "Goblin Piker", 2, 1);
    scenario.add_creature_to_graveyard(P1, "Bog Imp", 1, 1);
    scenario.add_creature_to_graveyard(P1, "Mons's Goblin Raiders", 1, 1);

    // Stock both libraries so the Draw step does not deck a player out and
    // end the game before/while the upkeep trigger resolves.
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
    scenario.with_library_top(P1, &["Swamp", "Swamp", "Swamp"]);

    let mut runner = scenario.build();

    // Advance through the Upkeep step; the upkeep trigger fires, goes on the
    // stack, and the priority-draining helper resolves it on the way to the
    // precombat main phase.
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    // Count Tombspawn (Zombie) tokens controlled by each player.
    let zombie_count = |owner| {
        runner
            .state()
            .objects
            .values()
            .filter(|o| {
                o.controller == owner
                    && o.zone == Zone::Battlefield
                    && o.card_types
                        .subtypes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case("zombie"))
            })
            .map(|o| o.id)
            .collect::<Vec<ObjectId>>()
            .len()
    };

    // CR 109.4: each player's count is THEIR OWN graveyard's creature cards.
    assert_eq!(
        zombie_count(P0),
        2,
        "P0 has 2 creature cards in graveyard → 2 Tombspawn tokens"
    );
    assert_eq!(
        zombie_count(P1),
        3,
        "P1 has 3 creature cards in graveyard → 3 Tombspawn tokens"
    );
}
