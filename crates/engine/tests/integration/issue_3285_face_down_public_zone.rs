//! Issue #3285 — face-down cards must turn face up when they leave the
//! battlefield or stack and enter a public zone (CR 708.9).

use engine::game::morph::manifest_card;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::move_to_zone;
use engine::types::ability::FaceDownProfile;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

#[test]
fn manifested_creature_turns_face_up_in_graveyard_when_it_dies() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let card = scenario.add_card_to_library_top(P0, "Hidden Grizzly");
    let mut runner = scenario.build();
    let mut events = Vec::new();
    manifest_card(
        runner.state_mut(),
        P0,
        card,
        card,
        FaceDownProfile::vanilla_2_2(),
        &mut events,
    )
    .expect("manifest must succeed");

    assert!(
        runner.state().objects[&card].face_down,
        "manifested card must enter face down"
    );

    move_to_zone(runner.state_mut(), card, Zone::Graveyard, &mut events);

    let obj = runner
        .state()
        .objects
        .get(&card)
        .expect("object must exist");
    assert_eq!(obj.zone, Zone::Graveyard);
    assert!(
        !obj.face_down,
        "CR 708.9: leaving the battlefield must reveal the card in graveyard"
    );
    assert_eq!(
        obj.name, "Hidden Grizzly",
        "graveyard must show the real card name after CR 708.9 reveal"
    );
}
