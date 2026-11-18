//! Regression for issue #11040: Darksteel Colossus milled from a library must
//! be revealed and shuffled into its owner's library instead of going to the
//! graveyard.
//!
//! https://github.com/phase-rs/phase/issues/11040
//!
//! Darksteel Colossus: "Trample / Indestructible / If Darksteel Colossus would
//! be put into a graveyard from anywhere, reveal Darksteel Colossus and
//! shuffle it into its owner's library instead."
//!
//! CR 113.6b: "An ability that states which zones it functions in functions
//! only from those zones." Since the card explicitly says "from anywhere",
//! the replacement must function from all zones (Library, Hand, Battlefield,
//! Stack, Exile, Command).
//!
//! CR 701.17a: "For a player to mill a number of cards, that player puts that
//! many cards from the top of their library into their graveyard."

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DARKSTEEL_COLOSSUS: &str = "Trample\nIndestructible\nIf Darksteel Colossus would be put into a graveyard from anywhere, reveal Darksteel Colossus and shuffle it into its owner's library instead.";

const TOME_SCOUR: &str = "Target player mills five cards.";

/// Darksteel Colossus in library: Tome Scour mills it, replacement must
/// trigger, reveal it, and shuffle it into its owner's library.
#[test]
fn darksteel_colossus_milled_from_library_shuffles_back() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P1's library: Darksteel Colossus on top, then filler cards
    // Create on battlefield first to parse Oracle text, then move to library
    let colossus = scenario
        .add_creature_from_oracle(P1, "Darksteel Colossus", 11, 11, DARKSTEEL_COLOSSUS)
        .id();
    
    for i in 0..10 {
        scenario.add_card_to_library_top(P1, &format!("Filler {i}"));
    }

    // P0 has Tome Scour to mill P1 (zero mana cost for auto-pay)
    let tome_scour = scenario
        .add_spell_to_hand_from_oracle(P0, "Tome Scour", /* is_instant */ false, TOME_SCOUR)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    // Move Darksteel Colossus from battlefield to library top
    let mut events = Vec::new();
    move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(colossus, Zone::Library, colossus),
        &mut events,
    );
    // Place on top of library
    {
        let state = runner.state_mut();
        let player_state = state.players.iter_mut().find(|p| p.id == P1).expect("player exists");
        player_state.library.retain(|&oid| oid != colossus);
        player_state.library.insert(0, colossus);
    }

    // Verify initial state
    assert_eq!(runner.state().objects[&colossus].zone, Zone::Library);
    assert_eq!(runner.state().players[1].library.len(), 11); // Colossus + 10 fillers
    assert!(runner.state().players[1].graveyard.is_empty());

    // Verify the replacement definition was attached
    let colossus_obj = &runner.state().objects[&colossus];
    let repl = colossus_obj
        .replacement_definitions
        .as_slice()
        .iter()
        .find(|d| d.event == engine::types::replacements::ReplacementEvent::Moved)
        .expect("Darksteel Colossus must carry a Moved replacement");
    assert!(!repl.active_zones.is_empty(), "active_zones must be set for 'from anywhere'");
    assert!(repl.active_zones.contains(&Zone::Library), "active_zones must include Library");

    // Cast Tome Scour targeting P1
    let outcome = runner
        .cast(tome_scour)
        .target_player(P1)
        .resolve();

    // Verify Darksteel Colossus was revealed and shuffled into library
    let colossus_obj = &runner.state().objects[&colossus];
    assert_eq!(
        colossus_obj.zone,
        Zone::Library,
        "Darksteel Colossus must shuffle into its owner's library instead of going to the graveyard"
    );
    assert!(
        !runner.state().players[1].graveyard.contains(&colossus),
        "Darksteel Colossus must not reach the graveyard"
    );
    assert!(
        runner.state().players[1].library.contains(&colossus),
        "Darksteel Colossus must end up in its owner's library"
    );

    // Verify Tome Scour itself went to graveyard (normal resolution)
    assert_eq!(
        outcome.zone_count(P0, Zone::Graveyard),
        1,
        "Tome Scour must reach its owner's graveyard on resolution"
    );

    // Verify game ends at priority
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution should end at priority, not an unexpected prompt"
    );

    // Verify exactly 4 cards were milled (5 total - 1 Colossus that redirected)
    let p1_graveyard_count = runner.state().players[1].graveyard.len();
    assert_eq!(
        p1_graveyard_count, 4,
        "Exactly 4 filler cards should reach the graveyard (5 milled - 1 redirected)"
    );
}

/// Rest in Peace on battlefield: its "from anywhere" replacement must NOT
/// apply to cards milled from library (non-self replacement; host is on
/// battlefield, so default functioning zones are battlefield/command only).
/// Positive control: Rest in Peace DOES apply when a card on the battlefield
/// would go to the graveyard.
#[test]
fn rest_in_peace_from_anywhere_does_not_apply_from_library_but_does_from_battlefield() {
    const REST_IN_PEACE: &str = "When Rest in Peace enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Rest in Peace on P0's battlefield
    let rip = scenario
        .add_creature(P0, "Rest in Peace", 0, 0)
        .as_enchantment()
        .from_oracle_text(REST_IN_PEACE)
        .id();

    // P1's library: a creature to mill (on top), then filler cards
    // Note: add_card_to_library_top adds to index 0 (top), so add fillers first, then victim
    for i in 0..10 {
        scenario.add_card_to_library_top(P1, &format!("Filler {i}"));
    }
    let victim = scenario.add_card_to_library_top(P1, "Mill Victim");

    // P0 has Tome Scour to mill P1 (zero mana cost for auto-pay)
    let tome_scour = scenario
        .add_spell_to_hand_from_oracle(P0, "Tome Scour", /* is_instant */ false, TOME_SCOUR)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    // Verify Rest in Peace is on battlefield
    assert_eq!(runner.state().objects[&rip].zone, Zone::Battlefield);

    // Verify Rest in Peace's replacement has empty active_zones (non-self replacement)
    let rip_obj = &runner.state().objects[&rip];
    let rip_repl = rip_obj
        .replacement_definitions
        .as_slice()
        .iter()
        .find(|d| d.event == engine::types::replacements::ReplacementEvent::Moved)
        .expect("Rest in Peace must carry a Moved replacement");
    assert!(rip_repl.active_zones.is_empty(), "Rest in Peace (non-self) must have empty active_zones, got {:?}", rip_repl.active_zones);

    // Cast Tome Scour targeting P1
    let _outcome = runner
        .cast(tome_scour)
        .target_player(P1)
        .resolve();

    // The milled creature should go to GRAVEYARD, not exile
    // (Rest in Peace's replacement functions only from battlefield/command)
    let victim_obj = &runner.state().objects[&victim];
    assert_eq!(
        victim_obj.zone,
        Zone::Graveyard,
        "Rest in Peace's replacement must NOT apply from library (non-self replacement; host on battlefield)"
    );
    assert!(
        runner.state().players[1].graveyard.contains(&victim),
        "Milled card must reach the graveyard when Rest in Peace is on battlefield"
    );
    assert_eq!(
        victim_obj.zone,
        Zone::Graveyard,
        "Milled card must NOT be exiled by Rest in Peace from library"
    );

    // POSITIVE CONTROL: Sacrifice the victim from battlefield - Rest in Peace
    // MUST apply here (host and affected object are both on battlefield)
    let mut scenario2 = GameScenario::new();
    scenario2.at_phase(Phase::PreCombatMain);

    let _rip2 = scenario2
        .add_creature(P0, "Rest in Peace", 0, 0)
        .as_enchantment()
        .from_oracle_text(REST_IN_PEACE)
        .id();

    let victim2 = scenario2.add_creature(P1, "Sacrifice Victim", 2, 2).id();

    let mut runner2 = scenario2.build();

    // Sacrifice the creature (move from battlefield to graveyard)
    let mut events = Vec::new();
    engine::game::zone_pipeline::move_object_for_test(
        runner2.state_mut(),
        engine::game::zone_pipeline::ZoneMoveRequest::effect(victim2, Zone::Graveyard, victim2),
        &mut events,
    );

    // The creature should be EXILED by Rest in Peace
    let victim2_obj = &runner2.state().objects[&victim2];
    assert_eq!(
        victim2_obj.zone,
        Zone::Exile,
        "Rest in Peace's replacement MUST apply from battlefield (positive control)"
    );
    assert!(
        !runner2.state().players[1].graveyard.contains(&victim2),
        "Sacrificed creature must NOT reach the graveyard when Rest in Peace is on battlefield"
    );
}