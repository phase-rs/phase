//! Runtime regression for #2931 — Winding Way reveal-and-partition.
//!
//! "Choose creature or land. Reveal the top four cards of your library. Put all
//! cards of the chosen type revealed this way into your hand and the rest into
//! your graveyard."
//!
//! On `main` the spell revealed the top four cards but moved nothing: the
//! chosen-type move read `IsChosenCreatureType` (never matches the chosen
//! Creature/Land card type), its `ChangeZoneAll` `origin` was inferred as
//! Graveyard (so it scanned the wrong zone), and the "and the rest into your
//! graveyard" complement was dropped. After the fix the chosen-type cards go to
//! hand and the rest go to the graveyard (CR 205.2a / CR 608.2c / CR 701.20b).

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WINDING_WAY: &str = "Choose creature or land. Reveal the top four cards of your library. \
     Put all cards of the chosen type revealed this way into your hand and the rest into your graveyard.";

#[test]
fn winding_way_routes_chosen_type_to_hand_and_rest_to_graveyard() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Stage the top four cards of P0's library, top-first: two creatures and two
    // lands interleaved so a zone-blind move can't accidentally land them right.
    // `add_card_to_library_top` inserts at index 0, so add bottom-of-the-four
    // first and top-of-the-four last to get [creature, land, creature, land].
    let bottom_land = scenario.add_card_to_library_top(P0, "Bottom Land");
    let bottom_creature = scenario.add_card_to_library_top(P0, "Bottom Creature");
    let top_land = scenario.add_card_to_library_top(P0, "Top Land");
    let top_creature = scenario.add_card_to_library_top(P0, "Top Creature");

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Winding Way", false, WINDING_WAY)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    // Type the staged library cards (the library helper creates them typeless).
    for &(id, core) in &[
        (top_creature, CoreType::Creature),
        (top_land, CoreType::Land),
        (bottom_creature, CoreType::Creature),
        (bottom_land, CoreType::Land),
    ] {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(core);
        obj.base_card_types = obj.card_types.clone();
    }

    // Cast: the resolution driver halts at the creature/land choice prompt.
    runner.cast(spell).resolve();
    runner
        .act(GameAction::ChooseOption {
            choice: "Creature".to_string(),
        })
        .expect("ChooseOption(Creature) must resolve");
    runner.advance_until_stack_empty();

    let zone_of = |id| runner.state().objects[&id].zone;

    // CR 608.2c: the chosen type (Creature) goes to hand.
    assert_eq!(
        zone_of(top_creature),
        Zone::Hand,
        "the top revealed creature must go to hand"
    );
    assert_eq!(
        zone_of(bottom_creature),
        Zone::Hand,
        "the deeper revealed creature must go to hand"
    );

    // CR 608.2c: "the rest" (the non-chosen lands) go to the graveyard.
    assert_eq!(
        zone_of(top_land),
        Zone::Graveyard,
        "a revealed land (not of the chosen type) must go to the graveyard"
    );
    assert_eq!(
        zone_of(bottom_land),
        Zone::Graveyard,
        "the deeper revealed land must go to the graveyard"
    );
}
