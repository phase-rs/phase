//! CR 702.73a + CR 205.3m: a Changeling card satisfies "a creature card of the
//! chosen type" while it is still in the library.
//!
//! Reported from live play: Herald's Horn naming Slivers triggered on upkeep
//! with Morophon, the Boundless on top of the library, and the player was never
//! offered the card. Morophon prints no Sliver subtype — it is a Shapeshifter
//! with Changeling, so per CR 702.73a it IS every creature type.
//!
//! The layer system physically expands a Changeling permanent's subtypes only
//! on the battlefield, so a card sitting in the LIBRARY keeps its printed
//! `Shapeshifter`. The reveal gate therefore has to ask the shared filter
//! authority (`subtype_matches_with_changeling`) rather than compare printed
//! subtype strings, which is what this test pins.
//!
//! Three legs, because the negative leg is only meaningful next to a positive
//! one that proves the pipeline actually reaches the offer:
//!
//!   * `metallic_sliver` — a real printed Sliver. REACH GUARD: if this stops
//!     being offered the scenario itself has broken, and the Grizzly Bears leg
//!     below would start passing for the wrong reason.
//!   * `morophon, the boundless` — the regression. Fails before the fix.
//!   * `grizzly bears` — an ordinary non-Sliver, non-Changeling creature. Must
//!     NOT be offered, or the fix would just be "always true".

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::ChosenAttribute;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// What Herald's Horn's upkeep trigger did with the top card of the library.
struct HornUpkeep {
    /// Whether the "you may ... put it into your hand" offer was made.
    offered: bool,
    /// Zone of the top card after accepting any offer that was made.
    top_card_zone: Zone,
}

/// Put Herald's Horn onto the battlefield with `chosen_type` named, stack
/// `top_card` on top of the controller's library, and run to the controller's
/// upkeep trigger.
///
/// Returns `None` when the card fixture is unavailable (CI without card data).
fn run_horn_upkeep(chosen_type: &str, top_card: &str) -> Option<HornUpkeep> {
    let db = load_db()?;

    let mut scenario = GameScenario::new();
    let horn = scenario.add_real_card(P0, "Herald's Horn", Zone::Battlefield, db);
    let top = scenario.add_real_card(P0, top_card, Zone::Library, db);
    // A little library depth so no draw/empty-library state-based action
    // (CR 704.5b) interferes with the upkeep window.
    for _ in 0..3 {
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    }
    let mut runner = scenario.build();

    {
        let state = runner.state_mut();

        // CR 205.3m: the Changeling expansion is bounded by the runtime creature
        // subtype catalog. Production seeds this from the whole card corpus in
        // `load_and_hydrate_decks`; mirror that here so "Sliver" is a real
        // catalog entry rather than a value this test invented.
        state.all_creature_types = db.creature_type_vocabulary().to_vec();

        // `add_real_card` abandons as-enters choices during scenario setup, so
        // Herald's Horn's "As this artifact enters, choose a creature type"
        // (CR 614.12) leaves no choice behind. Name the type directly — this
        // test is about the reveal gate, not about the enters-choice prompt.
        state
            .objects
            .get_mut(&horn)
            .expect("Herald's Horn is on the battlefield")
            .chosen_attributes
            .push(ChosenAttribute::CreatureType(chosen_type.to_string()));

        // Put the card under test on TOP of the library (CR 701.20a looks at
        // the top card, so library order is the whole setup).
        let player = state.players.iter_mut().find(|p| p.id == P0).unwrap();
        player.library.retain(|id| *id != top);
        player.library.push_front(top);
    }

    runner.advance_to_upkeep();

    // Reach guard: the trigger must actually be on the stack. Without this a
    // silently-missing trigger would make every "not offered" assertion below
    // pass for the wrong reason.
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "scenario must reach the controller's upkeep"
    );
    assert_eq!(
        runner.state().active_player,
        P0,
        "the upkeep reached must be the Horn controller's own (CR 506.1)"
    );
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == horn),
        "Herald's Horn's upkeep trigger must be on the stack, stack was {:?}",
        runner.stack_names()
    );

    // CR 603.3b + CR 608.1: both players pass, the trigger resolves, and the
    // look-at-top / reveal chain runs.
    runner
        .act(GameAction::PassPriority)
        .expect("controller passes priority");
    runner
        .act(GameAction::PassPriority)
        .expect("opponent passes priority");

    let offered = matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    );

    if offered {
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accepting the Horn's optional reveal");
    }

    Some(HornUpkeep {
        offered,
        top_card_zone: zone_of(&runner, top),
    })
}

fn zone_of(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> Zone {
    runner
        .state()
        .objects
        .get(&id)
        .expect("the top card still exists")
        .zone
}

/// CR 702.73a: the regression. Morophon has Changeling and therefore IS a
/// Sliver, even in the library where the layer system does not run. Before the
/// fix the gate compared printed subtypes (`Shapeshifter`) against the chosen
/// type and the offer was never made.
#[test]
fn heralds_horn_offers_a_changeling_card_of_the_chosen_type() {
    let Some(result) = run_horn_upkeep("Sliver", "Morophon, the Boundless") else {
        return;
    };

    assert!(
        result.offered,
        "Herald's Horn naming Slivers must offer Morophon, the Boundless — \
         Changeling makes it every creature type (CR 702.73a)"
    );
    assert_eq!(
        result.top_card_zone,
        Zone::Hand,
        "accepting the offer must put the Changeling card into its owner's hand"
    );
}

/// CR 205.3m: REACH GUARD for the negative test below. A real printed Sliver
/// must be offered. If this leg ever fails, the scenario stopped reaching the
/// offer at all and the `grizzly_bears` assertion would be vacuous.
#[test]
fn heralds_horn_offers_a_printed_card_of_the_chosen_type() {
    let Some(result) = run_horn_upkeep("Sliver", "Metallic Sliver") else {
        return;
    };

    assert!(
        result.offered,
        "Herald's Horn naming Slivers must offer a printed Sliver"
    );
    assert_eq!(
        result.top_card_zone,
        Zone::Hand,
        "accepting the offer must put the Sliver into its owner's hand"
    );
}

/// CR 205.3m: the Changeling expansion must not leak into ordinary creatures.
/// Grizzly Bears is a Bear with no Changeling, so naming Slivers must leave it
/// on top of the library with no offer made.
#[test]
fn heralds_horn_does_not_offer_an_ordinary_creature_of_another_type() {
    let Some(result) = run_horn_upkeep("Sliver", "Grizzly Bears") else {
        return;
    };

    assert!(
        !result.offered,
        "an ordinary Bear must NOT satisfy \"creature card of the chosen type\" \
         when Slivers were named"
    );
    assert_eq!(
        result.top_card_zone,
        Zone::Library,
        "a card that fails the chosen-type gate stays on top of the library"
    );
}
