//! The Mimeoplasm interactive exile path test.
//!
//! Tests the PayCost arm in engine_resolution_choices.rs by verifying
//! that EffectKind::PayCost is handled correctly for cost-payment exile.
//!
//! This test directly exercises the PayCost arm by calling
//! apply_as_current with a manually constructed EffectZoneChoice
//! with PayCost, bypassing the need for the full Mimeoplasm replacement
//! pipeline to be functional.

use engine::game::scenario::{GameScenario, P0};
use engine::game::zones;
use engine::types::ability::{EffectKind, ReplacementMode};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::zones::EtbTapState;
use engine::types::zones::Zone;
use engine::types::Phase;

#[test]
fn mimeoplasm_replacement_parsed_from_oracle() {
    // Verify that the Mimeoplasm replacement is correctly parsed from Oracle text
    let mut scenario = GameScenario::new();

    let mimeoplasm_id = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Mimeoplasm Test",
            5, 5,
            "As ~ enters, you may exile two creature cards from graveyards. If you do, ~ enters as a copy of one of them, except it has +1/+1 counters equal to the other's power.",
        )
        .id();

    let runner = scenario.build();

    // Verify the replacement was parsed and installed
    let mimeoplasm_obj = runner.state().objects.get(&mimeoplasm_id).unwrap();
    assert!(
        !mimeoplasm_obj.replacement_definitions.is_empty(),
        "Mimeoplasm should have replacement definitions parsed from Oracle text"
    );

    // Verify it's a MayCost replacement
    let repl = &mimeoplasm_obj.replacement_definitions[0];
    assert!(
        matches!(repl.mode, ReplacementMode::MayCost { .. }),
        "Mimeoplasm replacement should be MayCost mode"
    );

    println!(
        "Mimeoplasm replacement parsed successfully: {:?}",
        repl.mode
    );
}

#[test]
fn mimeoplasm_cast_triggers_replacement() {
    // Test that casting Mimeoplasm with graveyard creatures triggers the replacement
    // and surfaces the exile cost choice. This verifies the replacement pipeline
    // correctly identifies and offers the MayCost replacement, and that the
    // replacement continuation is applied after the cost is paid.

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Add 3 creatures to P0's graveyard
    let _bears_id = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let _giant_id = scenario
        .add_creature_to_graveyard(P0, "Hill Giant", 3, 3)
        .id();
    let _angel_id = scenario
        .add_creature_to_graveyard(P0, "Serra Angel", 4, 4)
        .id();

    // Add Mimeoplasm to hand
    let mimeoplasm_id = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Mimeoplasm Test",
            5, 5,
            "As ~ enters, you may exile two creature cards from graveyards. If you do, ~ enters as a copy of one of them, except it has +1/+1 counters equal to the other's power.",
        )
        .id();

    // Add mana to cast it
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);

    let mut runner = scenario.build();

    // Verify graveyard has 3 creatures
    assert_eq!(runner.state().players[0].graveyard.len(), 3);

    // Cast Mimeoplasm
    let _outcome = runner.cast(mimeoplasm_id).resolve();

    // Check if we hit a replacement choice (not just priority)
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { .. } => {
            println!("SUCCESS: Replacement choice surfaced as expected");
        }
        WaitingFor::Priority { .. } => {
            println!("FAILURE: No replacement choice - replacement did not fire");
            println!("Final state: {:?}", runner.state().waiting_for);
            panic!("Replacement should have fired but didn't");
        }
        other => {
            println!("UNEXPECTED waiting_for state: {:?}", other);
            panic!("Unexpected waiting_for state");
        }
    }

    // Accept the replacement choice (index 0 = pay cost, index 1 = decline)
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("Accept replacement should succeed");

    // Now we should be at EffectZoneChoice for the exile cost
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { cards, count, .. } => {
            println!(
                "SUCCESS: EffectZoneChoice surfaced with {} cards, need to select {}",
                cards.len(),
                count
            );
            // Select the first two cards
            let selected = cards.iter().take(*count).copied().collect::<Vec<_>>();
            runner
                .act(GameAction::SelectCards { cards: selected })
                .expect("SelectCards should succeed");
        }
        other => {
            println!(
                "UNEXPECTED waiting_for after accepting replacement: {:?}",
                other
            );
            panic!("Expected EffectZoneChoice after accepting replacement");
        }
    }

    // After the exile cost is paid, the replacement continuation should apply
    // and Mimeoplasm should enter the battlefield
    let state = runner.state();
    println!("Final waiting_for: {:?}", state.waiting_for);
    println!("Battlefield objects: {:?}", state.battlefield);
    println!("Graveyard count: {}", state.players[0].graveyard.len());
    println!("Exile count: {}", state.exile.len());
    println!("Stack count: {}", state.stack.len());

    // Verify cards were exiled (PayCost arm worked)
    assert_eq!(
        state.players[0].graveyard.len(),
        1,
        "Two cards should have been exiled, leaving 1 in graveyard"
    );
    assert!(
        state.exile.len() >= 2,
        "At least 2 cards should be in exile"
    );

    // Verify Mimeoplasm is on the battlefield (replacement continuation applied)
    // After accepting the replacement, Mimeoplasm copies the first exiled card
    let mimeoplasm_on_battlefield = state.battlefield.iter().any(|&id| {
        state
            .objects
            .get(&id)
            .is_some_and(|obj| obj.name == "Grizzly Bears" || obj.name == "Mimeoplasm Test")
    });
    assert!(
        mimeoplasm_on_battlefield,
        "Mimeoplasm should be on the battlefield after replacement continuation"
    );

    println!("SUCCESS: Full replacement → exile-choice → battlefield flow completed");
}

#[test]
fn mimeoplasm_full_end_to_end_copy_and_counters() {
    // Discriminating test that verifies the full Mimeoplasm flow:
    // - Cast Mimeoplasm with ≥3 creatures in graveyards (force interactive choice)
    // - Exile two creatures with distinct power/toughness (2/2 and 4/4)
    // - Assert the resulting permanent's name equals the first exiled card's name
    // - Assert the permanent has +1/+1 counters equal to the second exiled card's power

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Add 3 creatures to P0's graveyard with distinct power/toughness
    let bears_id = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let giant_id = scenario
        .add_creature_to_graveyard(P0, "Hill Giant", 3, 3)
        .id();
    let _angel_id = scenario
        .add_creature_to_graveyard(P0, "Serra Angel", 4, 4)
        .id();

    // Add Mimeoplasm to hand
    let mimeoplasm_id = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Mimeoplasm Test",
            5, 5,
            "As ~ enters, you may exile two creature cards from graveyards. If you do, ~ enters as a copy of one of them, except it has +1/+1 counters equal to the other's power.",
        )
        .id();

    // Add mana to cast it
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);

    let mut runner = scenario.build();

    // Verify graveyard has 3 creatures
    assert_eq!(runner.state().players[0].graveyard.len(), 3);

    // Cast Mimeoplasm
    let _outcome = runner.cast(mimeoplasm_id).resolve();

    // Accept the replacement choice (index 0 = pay cost)
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("Accept replacement should succeed");

    // Select the first two cards (Bears and Giant) for exile
    // This will make Mimeoplasm copy Bears (index 0) and get counters equal to Giant's power (3)
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { .. } => {
            let selected = vec![bears_id, giant_id];
            runner
                .act(GameAction::SelectCards { cards: selected })
                .expect("SelectCards should succeed");
        }
        other => {
            panic!("Expected EffectZoneChoice, got {:?}", other);
        }
    }

    // Verify the outcome
    let state = runner.state();

    // Verify cards were exiled
    assert_eq!(
        state.players[0].graveyard.len(),
        1,
        "Two cards should have been exiled, leaving 1 in graveyard"
    );
    assert!(
        state.exile.len() >= 2,
        "At least 2 cards should be in exile"
    );

    // Verify Mimeoplasm is on the battlefield
    // The Mimeoplasm should be the object that was cast (mimeoplasm_id), now on battlefield
    let mimeoplasm_obj = state.objects.get(&mimeoplasm_id);
    assert!(
        mimeoplasm_obj.is_some_and(|obj| obj.zone == Zone::Battlefield),
        "Mimeoplasm should be on the battlefield"
    );

    let mimeoplasm_obj = mimeoplasm_obj.expect("Mimeoplasm object should exist");

    // The permanent should have copied Grizzly Bears (the first exiled card at index 0)
    assert_eq!(
        mimeoplasm_obj.name, "Grizzly Bears",
        "Mimeoplasm should have copied the first exiled card (Grizzly Bears)"
    );

    // Verify the permanent has +1/+1 counters equal to the second exiled card's power
    // Hill Giant has power 3, so we should have 3 +1/+1 counters
    let plus_one_counters = mimeoplasm_obj
        .counters
        .iter()
        .filter(|(counter_type, _count)| {
            **counter_type == engine::types::counter::CounterType::Plus1Plus1
        })
        .map(|(_counter_type, count)| *count)
        .sum::<u32>();
    assert_eq!(
        plus_one_counters, 3,
        "Mimeoplasm should have 3 +1/+1 counters (equal to Hill Giant's power)"
    );

    println!("SUCCESS: Full end-to-end test passed - copy and counters verified");
}

#[test]
fn mimeoplasm_exiles_from_opponent_graveyard() {
    // Discriminating test that verifies Mimeoplasm can exile from any player's graveyard,
    // not just the controller's. This is critical for Commander where Mimeoplasm's
    // primary use case is exiling opponents' creatures.

    let p1 = engine::game::scenario::P1;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Add a creature to P1's graveyard (opponent's graveyard)
    let opponent_bears_id = scenario
        .add_creature_to_graveyard(p1, "Grizzly Bears", 2, 2)
        .id();

    // Add a creature to P0's graveyard (controller's graveyard)
    let controller_giant_id = scenario
        .add_creature_to_graveyard(P0, "Hill Giant", 3, 3)
        .id();

    // Add a third creature to P0's graveyard to prevent forced-choice fast path
    let _controller_angel_id = scenario
        .add_creature_to_graveyard(P0, "Serra Angel", 4, 4)
        .id();

    // Add Mimeoplasm to P0's hand
    let mimeoplasm_id = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Mimeoplasm Test",
            5, 5,
            "As ~ enters, you may exile two creature cards from graveyards. If you do, ~ enters as a copy of one of them, except it has +1/+1 counters equal to the other's power.",
        )
        .id();

    // Add mana to cast it
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);

    let mut runner = scenario.build();

    // Verify graveyards have the expected cards
    assert_eq!(runner.state().players[0].graveyard.len(), 2);
    assert_eq!(runner.state().players[1].graveyard.len(), 1);

    // Cast Mimeoplasm
    let _outcome = runner.cast(mimeoplasm_id).resolve();

    // Accept the replacement choice (index 0 = pay cost)
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("Accept replacement should succeed");

    // Verify that both cards (from both graveyards) are eligible for exile
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { cards, .. } => {
            // Should have 3 cards available: 2 from P0's graveyard, 1 from P1's graveyard
            assert_eq!(
                cards.len(),
                3,
                "Should have 3 eligible cards from both graveyards"
            );
            assert!(
                cards.contains(&opponent_bears_id),
                "Opponent's creature should be eligible for exile"
            );
            assert!(
                cards.contains(&controller_giant_id),
                "Controller's creature should be eligible for exile"
            );

            // Select the opponent's creature and the controller's creature
            let selected = vec![opponent_bears_id, controller_giant_id];
            runner
                .act(GameAction::SelectCards { cards: selected })
                .expect("SelectCards should succeed");
        }
        other => {
            panic!("Expected EffectZoneChoice, got {:?}", other);
        }
    }

    // Verify the outcome
    let state = runner.state();

    // Verify cards were exiled from both graveyards
    assert_eq!(
        state.players[0].graveyard.len(),
        1,
        "Controller's graveyard should have 1 card remaining (Angel) after exile"
    );
    assert_eq!(
        state.players[1].graveyard.len(),
        0,
        "Opponent's graveyard should be empty after exile"
    );
    assert!(
        state.exile.len() >= 2,
        "At least 2 cards should be in exile"
    );

    // Verify Mimeoplasm is on the battlefield
    let mimeoplasm_obj = state.objects.get(&mimeoplasm_id);
    assert!(
        mimeoplasm_obj.is_some_and(|obj| obj.zone == Zone::Battlefield),
        "Mimeoplasm should be on the battlefield"
    );

    let mimeoplasm_obj = mimeoplasm_obj.expect("Mimeoplasm object should exist");

    // The permanent should have copied the opponent's Grizzly Bears (index 0)
    assert_eq!(
        mimeoplasm_obj.name, "Grizzly Bears",
        "Mimeoplasm should have copied the opponent's Grizzly Bears"
    );

    // Verify the permanent has +1/+1 counters equal to the controller's Hill Giant's power (3)
    let plus_one_counters = mimeoplasm_obj
        .counters
        .iter()
        .filter(|(counter_type, _count)| {
            **counter_type == engine::types::counter::CounterType::Plus1Plus1
        })
        .map(|(_counter_type, count)| *count)
        .sum::<u32>();
    assert_eq!(
        plus_one_counters, 3,
        "Mimeoplasm should have 3 +1/+1 counters (equal to Hill Giant's power)"
    );

    println!("SUCCESS: Opponent graveyard exile test passed");
}

#[test]
fn paycost_arm_exiles_cards_via_apply_as_current() {
    // This test directly exercises the PayCost arm by:
    // 1. Setting up a GameState with 3 cards in graveyard
    // 2. Manually setting WaitingFor::EffectZoneChoice with PayCost
    // 3. Calling apply_as_current with SelectCards
    // 4. Verifying the cards are exiled (not just selected)
    //
    // This test will fail if the PayCost arm is removed, because
    // apply_as_current would return "EffectZoneChoice unsupported
    // for PayCost" from the catch-all.

    let mut scenario = GameScenario::new();

    // Add 3 creatures to P0's battlefield, then move to graveyard
    let bears_id = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let giant_id = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let angel_id = scenario.add_creature(P0, "Serra Angel", 4, 4).id();

    let mut runner = scenario.build();

    // Move them to graveyard
    let mut events = vec![];
    zones::move_to_zone(runner.state_mut(), bears_id, Zone::Graveyard, &mut events);
    zones::move_to_zone(runner.state_mut(), giant_id, Zone::Graveyard, &mut events);
    zones::move_to_zone(runner.state_mut(), angel_id, Zone::Graveyard, &mut events);

    // Verify they're in graveyard
    let state = runner.state();
    assert_eq!(state.players[0].graveyard.len(), 3);

    // Manually set WaitingFor::EffectZoneChoice with PayCost
    {
        let state = runner.state_mut();
        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: P0,
            cards: vec![bears_id, giant_id, angel_id],
            count: 2,
            min_count: 0,
            up_to: false,
            source_id: ObjectId(100),
            effect_kind: EffectKind::PayCost,
            zone: Zone::Graveyard,
            destination: Some(Zone::Exile),
            enter_tapped: EtbTapState::Unspecified,
            enter_transformed: false,
            enters_under_player: None,
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: true,
            face_down_profile: None,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            count_param: 0,
            library_position: None,
            is_cost_payment: true,
        };
    }

    // Call apply_as_current with SelectCards
    let result = runner.act(GameAction::SelectCards {
        cards: vec![bears_id, giant_id],
    });

    // This will fail with "EffectZoneChoice unsupported for PayCost" if the PayCost arm is missing
    assert!(
        result.is_ok(),
        "apply_as_current should succeed with PayCost arm"
    );

    // Verify the cards were exiled (not just selected)
    let state = runner.state();
    assert_eq!(
        state.players[0].graveyard.len(),
        1, // Only Serra Angel should remain
        "Two cards should have been exiled, leaving 1 in graveyard"
    );

    // Verify the exiled cards are in exile zone
    assert!(
        state.exile.len() >= 2,
        "At least 2 cards should be in exile"
    );
}
