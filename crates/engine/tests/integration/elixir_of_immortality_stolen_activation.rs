//! CR 109.5 + CR 400.3 + CR 400.6: "Shuffle this artifact and your graveyard
//! into their owner's library" (Elixir of Immortality) names the ACTIVATOR's
//! graveyard, not the artifact owner's.
//!
//! CR 109.5: for an activated ability, "you"/"your" is the player who activated
//! it. CR 400.6 then moves every card in that zone, and CR 400.3 sends each
//! moved card to ITS OWNER's corresponding library — so the activator's
//! graveyard empties into each card's own owner's library while the source
//! owner's graveyard is untouched. CR 701.24c also requires the library to be
//! shuffled even when the moved set is empty.
//!
//! The parser used to re-read this explicit populate as the source owner's
//! (CR 108.3), which is the wrong possessor whenever another player controls
//! the artifact. The row below is the runtime witness: the artifact is OWNED by
//! P1 and CONTROLLED by P0 (the active player, who activates it), and both
//! graveyards are nonempty and distinguishable, so either possessor choice moves
//! a visibly different card.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ELIXIR: &str = "{2}, {T}: You gain 5 life. Shuffle this artifact and your graveyard into their owner's library.";

fn zone_of(runner: &GameRunner, id: ObjectId) -> Option<Zone> {
    runner.state().objects.get(&id).map(|obj| obj.zone)
}

fn in_library(runner: &GameRunner, player: engine::types::player::PlayerId, id: ObjectId) -> bool {
    runner.state().players[player.0 as usize]
        .library
        .contains(&id)
}

#[test]
fn stolen_elixir_shuffles_the_activators_graveyard_not_the_owners() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P1 owns the Elixir; P0 (the active player) controls and activates it.
    let elixir = scenario
        .add_artifact_from_oracle(P1, "Elixir of Immortality", ELIXIR)
        .controlled_by(P0)
        .id();
    // CR 107.4e / CR 601.2h: the activator pays {2} from their own mana.
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    // Two nonempty, distinguishable graveyards — one card each.
    let owners_card = scenario
        .add_creature_to_graveyard(P1, "Owner's Fallen", 1, 1)
        .id();
    let activators_card = scenario
        .add_creature_to_graveyard(P0, "Activator's Fallen", 2, 2)
        .id();
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: elixir,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the stolen Elixir's ability can be activated");
    runner.advance_until_stack_empty();

    // CR 109.5 + CR 400.6: the ACTIVATOR's graveyard is the population, so the
    // card staged in P0's graveyard is the one that moves — into ITS owner's
    // library (CR 400.3).
    assert_eq!(
        zone_of(&runner, activators_card),
        Some(Zone::Library),
        "the activator's graveyard card must be shuffled into its owner's library"
    );
    assert!(
        in_library(&runner, P0, activators_card),
        "it must reach P0's library, its owner's corresponding zone"
    );

    // The owner's graveyard is NOT the population: nothing in it moved.
    assert_eq!(
        zone_of(&runner, owners_card),
        Some(Zone::Graveyard),
        "the source owner's graveyard must be untouched — the possessor is the activator's"
    );

    // CR 400.3: the artifact itself goes to the library of ITS owner (P1), not
    // the activator's.
    assert_eq!(
        zone_of(&runner, elixir),
        Some(Zone::Library),
        "the Elixir itself is shuffled into a library"
    );
    assert!(
        in_library(&runner, P1, elixir),
        "the Elixir must reach its OWNER's library (P1), never the activator's"
    );
    assert!(
        !in_library(&runner, P0, elixir),
        "the Elixir must not land in the activator's library"
    );
}
