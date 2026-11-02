use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DOOMSDAY_EXCRUCIATOR_ORACLE: &str = "\
Flying\n\
When Doomsday Excruciator enters, if it was cast, each player exiles all but the bottom six cards of their library face down.\n\
At the beginning of your upkeep, draw a card.";

const SUPERIOR_SPIDER_MAN_ORACLE: &str = "\
Mind Swap — You may have Superior Spider-Man enter as a copy of any creature card in a graveyard, except his name is Superior Spider-Man and he's a 4/4 Spider Human Hero in addition to his other types. When you do, exile that card.";

#[test]
fn doomsday_excruciator_cast_exiles_all_but_bottom_six_cards_face_down() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Set up P0 with 20 cards in library, P1 with 15 cards in library
    for i in 0..20 {
        scenario.add_card_to_library_top(P0, &format!("P0 Card {}", i));
    }
    for i in 0..15 {
        scenario.add_card_to_library_top(P1, &format!("P1 Card {}", i));
    }

    // Add permanents for each player that must survive
    let p0_land = scenario.add_basic_land(P0, ManaColor::Black);
    let p1_creature = scenario.add_vanilla(P1, 2, 2);

    let excruciator = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Doomsday Excruciator",
            6,
            6,
            DOOMSDAY_EXCRUCIATOR_ORACLE,
        )
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(excruciator).resolve();

    let state = outcome.state();

    // Check libraries are reduced to exactly 6 cards each
    assert_eq!(
        state
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .library
            .len(),
        6,
        "P0 should have exactly 6 cards remaining in library"
    );
    assert_eq!(
        state
            .players
            .iter()
            .find(|p| p.id == P1)
            .unwrap()
            .library
            .len(),
        6,
        "P1 should have exactly 6 cards remaining in library"
    );
    for (player, prefix) in [(P0, "P0"), (P1, "P1")] {
        let remaining: Vec<_> = state
            .players
            .iter()
            .find(|p| p.id == player)
            .unwrap()
            .library
            .iter()
            .map(|id| state.objects[id].name.clone())
            .collect();
        let bottom_six: Vec<_> = (0..6).rev().map(|i| format!("{prefix} Card {i}")).collect();
        assert_eq!(
            remaining, bottom_six,
            "bottom six cards must remain in order"
        );
    }

    // Check exiled cards: 14 from P0 + 9 from P1 = 23 cards in exile
    assert_eq!(state.exile.len(), 23, "23 total cards should be in exile");
    let mut p0_exiled_count = 0;
    let mut p1_exiled_count = 0;
    for &obj_id in &state.exile {
        let obj = &state.objects[&obj_id];
        assert!(obj.face_down, "Exiled cards must be face down");
        // CR 608.2c: subject-worded "each player exiles" is performed by the library owner
        if obj.owner == P0 {
            assert_eq!(obj.exiled_by, Some(P0), "P0's cards must be exiled by P0");
            p0_exiled_count += 1;
        } else if obj.owner == P1 {
            assert_eq!(obj.exiled_by, Some(P1), "P1's cards must be exiled by P1");
            p1_exiled_count += 1;
        }
    }
    assert_eq!(p0_exiled_count, 14);
    assert_eq!(p1_exiled_count, 9);

    // Check permanents remained on the battlefield (not wiped!)
    outcome.assert_zone(&[p0_land, p1_creature, excruciator], Zone::Battlefield);
}

#[test]
fn superior_spider_man_copies_doomsday_excruciator_in_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Set up libraries
    for i in 0..20 {
        scenario.add_card_to_library_top(P0, &format!("P0 Card {}", i));
    }
    for i in 0..15 {
        scenario.add_card_to_library_top(P1, &format!("P1 Card {}", i));
    }

    // Put Doomsday Excruciator in P1's graveyard
    let mut excruciator_builder =
        scenario.add_creature_to_graveyard(P1, "Doomsday Excruciator", 6, 6);
    excruciator_builder.from_oracle_text(DOOMSDAY_EXCRUCIATOR_ORACLE);
    let excruciator_card = excruciator_builder.id();

    // Add permanents on battlefield
    let p0_perm = scenario.add_basic_land(P0, ManaColor::Blue);
    let p1_perm = scenario.add_vanilla(P1, 1, 1);

    // P0 casts Superior Spider-Man
    let spidey = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Superior Spider-Man",
            0,
            0,
            SUPERIOR_SPIDER_MAN_ORACLE,
        )
        .id();

    let mut runner = scenario.build();

    // Cast Superior Spider-Man, resolving replacement choice to copy Excruciator in graveyard
    let outcome = runner
        .cast(spidey)
        .replacement_choice(0)
        .copy_target(excruciator_card)
        .resolve();

    let state = outcome.state();

    // Check libraries are reduced to 6 cards each
    assert_eq!(
        state
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .library
            .len(),
        6,
        "P0 should have 6 cards left in library"
    );
    assert_eq!(
        state
            .players
            .iter()
            .find(|p| p.id == P1)
            .unwrap()
            .library
            .len(),
        6,
        "P1 should have 6 cards left in library"
    );

    // Check permanents on battlefield are untouched
    outcome.assert_zone(&[p0_perm, p1_perm, spidey], Zone::Battlefield);

    // Check Superior Spider-Man's characteristics (CR 707.9b: name overridden, 4/4, Spider Human Hero)
    let spidey_obj = &state.objects[&spidey];
    assert_eq!(spidey_obj.name, "Superior Spider-Man");
    assert_eq!(spidey_obj.power, Some(4));
    assert_eq!(spidey_obj.toughness, Some(4));
}

/// CR 608.2c + CR 109.5: Jace, Reality Sculptor ("Exile all but the bottom card
/// of each opponent's library") is a controller-worded instruction distributed across opponents.
/// The controller (P0) performs the exile, so `exiled_by` on the opponent's exiled cards must
/// record P0 (the original controller), NOT the rebound opponent (P1).
#[test]
fn jace_reality_sculptor_distributive_exile_records_controller_as_actor() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    for i in 0..10 {
        scenario.add_card_to_library_top(P1, &format!("P1 Card {}", i));
    }

    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Jace's Ultimate",
            false,
            "Exile all but the bottom card of each opponent's library.",
        )
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();

    let state = outcome.state();

    // P1 library reduced to 1 card
    assert_eq!(
        state
            .players
            .iter()
            .find(|p| p.id == P1)
            .unwrap()
            .library
            .len(),
        1,
        "P1 should have 1 card remaining in library"
    );
    let remaining_id = state.players.iter().find(|p| p.id == P1).unwrap().library[0];
    assert_eq!(state.objects[&remaining_id].name, "P1 Card 0");

    // 9 cards exiled from P1's library
    assert_eq!(state.exile.len(), 9);
    for &obj_id in &state.exile {
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.owner, P1);
        assert_eq!(
            obj.exiled_by,
            Some(P0),
            "CR 608.2c: Jace's controller (P0) must be recorded as exiling player"
        );
    }
}

/// CR 608.2c + CR 109.5: a controller-worded instruction remains performed by
/// the spell's controller even when it names each player's library.
#[test]
fn each_player_library_exile_keeps_spell_controller_as_actor() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("P0 Card {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Card {i}"));
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Library Exile",
            false,
            "Exile all but the bottom card of each player's library.",
        )
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    let state = outcome.state();

    assert_eq!(
        state
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .library
            .len(),
        1
    );
    assert_eq!(
        state
            .players
            .iter()
            .find(|p| p.id == P1)
            .unwrap()
            .library
            .len(),
        1
    );
    assert_eq!(state.exile.len(), 6);
    for &obj_id in &state.exile {
        assert_eq!(state.objects[&obj_id].exiled_by, Some(P0));
    }
}
