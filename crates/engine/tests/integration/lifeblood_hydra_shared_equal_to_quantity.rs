//! Regression tests for Lifeblood Hydra:
//! "When this creature dies, you gain life and draw cards equal to its power."
//!
//! CR 608.2c: the trailing "equal to its power" is shared by BOTH coordinated
//! amount-elided verbs — the controller gains that much life AND draws that
//! many cards, in the order written.
//!
//! CR 608.2h + CR 113.7a: "its power" is read from the dying Hydra's last known
//! information. CR 122.2 makes its +1/+1 counters cease to exist on the zone
//! change, and the card's printed power is 0, so a live graveyard read would
//! give 0 — only the LKI rung yields the counter-boosted power.
//!
//! Revert baseline: before the shared-tail compound, the trigger body split
//! into "you gain life" (lowered to `Effect::Unimplemented`) plus a draw — the
//! controller gained no life.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall + MTGJSON agree byte for byte). A paraphrase
/// can take a different parser branch, so this must stay exact.
const LIFEBLOOD_HYDRA_ORACLE: &str = "Trample\n\
This creature enters with X +1/+1 counters on it.\n\
When this creature dies, you gain life and draw cards equal to its power.";

/// CR 608.2c + CR 608.2h + CR 122.2: a 0/0 Hydra with three +1/+1 counters
/// dies to a bolt; its controller gains 3 life AND draws exactly 3 cards (the
/// top three), the opponent is untouched.
///
/// Revert-failing: without the shared-tail compound the gain conjunct is
/// `Unimplemented`, so P0's life stays 20.
#[test]
fn lifeblood_hydra_dies_gains_life_and_draws_cards_equal_to_its_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hydra = scenario
        .add_creature_from_oracle(P0, "Lifeblood Hydra", 0, 0, LIFEBLOOD_HYDRA_ORACLE)
        .with_plus_counters(3)
        .id();
    let top = [
        scenario.add_card_to_library_top(P0, "Library E"),
        scenario.add_card_to_library_top(P0, "Library D"),
        scenario.add_card_to_library_top(P0, "Library C"),
        scenario.add_card_to_library_top(P0, "Library B"),
        scenario.add_card_to_library_top(P0, "Library A"),
    ];
    let p1_library = scenario.add_card_to_library_top(P1, "Opponent Card");
    let bolt = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;
    let p1_life_before = runner.state().players[P1.0 as usize].life;

    runner.cast(bolt).target_objects(&[hydra]).resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(
        state.objects[&hydra].zone,
        Zone::Graveyard,
        "reach-guard: the Hydra died"
    );
    assert_eq!(
        state.players[P0.0 as usize].life - life_before,
        3,
        "CR 608.2c + CR 608.2h: gain life equal to the Hydra's LKI power (3)"
    );
    // CR 121.1: the top three library cards (A, B, C) go to hand; D and E stay.
    for &drawn in &top[2..] {
        assert_eq!(
            state.objects[&drawn].zone,
            Zone::Hand,
            "drawn card {drawn:?}"
        );
    }
    for &kept in &top[..2] {
        assert_eq!(
            state.objects[&kept].zone,
            Zone::Library,
            "undrawn card {kept:?}"
        );
    }
    assert_eq!(
        state.players[P1.0 as usize].life, p1_life_before,
        "the opponent gains/loses nothing"
    );
    assert_eq!(state.objects[&p1_library].zone, Zone::Library);
}

/// The shared quantity scales with power (not a constant, not the printed 0):
/// five counters and two marked damage → the bolt kills a 5/5, gain 5 and
/// draw 5.
#[test]
fn lifeblood_hydra_shared_quantity_tracks_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hydra = scenario
        .add_creature_from_oracle(P0, "Lifeblood Hydra", 0, 0, LIFEBLOOD_HYDRA_ORACLE)
        .with_plus_counters(5)
        .with_damage_marked(2)
        .id();
    let library: Vec<_> = (0..6)
        .map(|i| scenario.add_card_to_library_top(P0, &format!("Library {i}")))
        .collect();
    let bolt = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    runner.cast(bolt).target_objects(&[hydra]).resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(state.objects[&hydra].zone, Zone::Graveyard, "reach-guard");
    assert_eq!(state.players[P0.0 as usize].life - life_before, 5);
    // `add_card_to_library_top` puts each new card on top: the last five added
    // are the top five and are drawn; the first-added (bottom) card stays.
    let in_hand = library
        .iter()
        .filter(|id| state.objects[*id].zone == Zone::Hand)
        .count();
    assert_eq!(in_hand, 5, "CR 608.2h: draw count equals LKI power (5)");
    assert_eq!(state.objects[&library[0]].zone, Zone::Library);
}
