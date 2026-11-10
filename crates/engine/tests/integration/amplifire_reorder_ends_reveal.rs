//! CR 701.20d: "any revealed cards that are reordered stop being revealed and
//! become new objects."
//!
//! Amplifire — "At the beginning of your upkeep, reveal cards from the top of
//! your library until you reveal a creature card. Until your next turn, this
//! creature's base power becomes twice that card's power and its base toughness
//! becomes twice that card's toughness. Put the revealed cards on the bottom
//! of your library in a random order."
//!
//! When the upkeep trigger resolves:
//! 1. The top cards are revealed until a creature card is revealed.
//! 2. A public `CardsRevealed` event is emitted.
//! 3. Amplifire's base power and toughness are set based on the revealed creature.
//! 4. The revealed cards are reordered to the bottom of the library in random order.
//! 5. Under CR 701.20d, the reorder causes those cards to stop being revealed and
//!    retires their old occurrences (bump incarnation).
//! 6. Immediately after settlement, active reveal marks (`revealed_cards`,
//!    `public_revealed_cards`) must be cleared, and seated views (P0, P1) as well
//!    as spectator views (unseated) must project those cards as hidden (`HIDDEN_CARD_NAME`).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::game::visibility::{
    filter_state_for_unseated_viewer, filter_state_for_viewer, HIDDEN_CARD_NAME,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const AMPLIFIRE_ORACLE: &str = "At the beginning of your upkeep, reveal cards from the top of your library until you reveal a creature card. Until your next turn, this creature's base power becomes twice that card's power and its base toughness becomes twice that card's toughness. Put the revealed cards on the bottom of your library in a random order.";

fn put_library_top(runner: &mut GameRunner, id: ObjectId) {
    let owner = runner.state().objects[&id].owner;
    let zone = runner.state().objects[&id].zone;
    let mut events = Vec::new();
    if zone != Zone::Library {
        engine::game::zones::remove_from_zone(runner.state_mut(), id, zone, owner);
        runner.state_mut().objects.get_mut(&id).unwrap().zone = Zone::Library;
        runner.state_mut().players[owner.0 as usize]
            .library
            .push_back(id);
    }
    engine::game::zones::move_to_library_position(runner.state_mut(), id, true, &mut events);
}

fn arrange_library_top_first(runner: &mut GameRunner, ids: &[ObjectId]) {
    for &id in ids.iter().rev() {
        put_library_top(runner, id);
    }
    let owner = runner.state().objects[&ids[0]].owner;
    let top: Vec<ObjectId> = runner.state().players[owner.0 as usize]
        .library
        .iter()
        .take(ids.len())
        .copied()
        .collect();
    assert_eq!(
        top, ids,
        "library top must be staged in the requested order"
    );
}

#[test]
fn amplifire_reorder_ends_reveal_and_hides_cards_for_all_viewers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);

    let amplifire = scenario
        .add_creature_from_oracle(P0, "Amplifire", 1, 1, AMPLIFIRE_ORACLE)
        .id();

    // Stage library for P0:
    // Top to bottom:
    // 1. Lightning Bolt (Instant, non-creature)
    // 2. Mountain (Land, non-creature)
    // 3. Hill Giant (Creature, 3/3)
    // 4. Island (unrevealed land)
    // 5. Forest (unrevealed land)
    let bolt = scenario.add_spell_to_hand(P0, "Lightning Bolt", true).id();
    let mountain = scenario.add_land_to_library_top(P0, "Mountain").id();
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let island = scenario.add_land_to_library_top(P0, "Island").id();
    let forest = scenario.add_land_to_library_top(P0, "Forest").id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    arrange_library_top_first(&mut runner, &[bolt, mountain, giant, island, forest]);

    let incarnation_bolt_before = runner.state().objects[&bolt].incarnation;
    let incarnation_mountain_before = runner.state().objects[&mountain].incarnation;
    let incarnation_giant_before = runner.state().objects[&giant].incarnation;

    // Trigger Amplifire's beginning of upkeep ability
    let events = vec![GameEvent::PhaseChanged {
        phase: Phase::Upkeep,
    }];
    process_triggers(runner.state_mut(), &events);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Amplifire trigger must be on the stack"
    );

    // Pass priority from both players to resolve the upkeep trigger
    let _ = runner.act(GameAction::PassPriority).expect("P0 passes");
    let result = runner.act(GameAction::PassPriority).expect("P1 passes");
    runner.advance_until_stack_empty();

    // 1. Amplifire base P/T became twice Hill Giant's P/T (3/3 -> 6/6)
    let amplifire_obj = &runner.state().objects[&amplifire];
    assert_eq!(
        (amplifire_obj.power, amplifire_obj.toughness),
        (Some(6), Some(6)),
        "Amplifire base P/T must become twice the revealed creature's P/T (6/6)"
    );

    // 2. Verify public CardsRevealed event was emitted during resolution
    let revealed_in_event: Option<Vec<ObjectId>> = result.events.iter().find_map(|e| match e {
        GameEvent::CardsRevealed {
            player, card_ids, ..
        } if *player == P0 => Some(card_ids.clone()),
        _ => None,
    });
    assert!(
        revealed_in_event.is_some(),
        "CardsRevealed event must be emitted during resolution"
    );
    let revealed = revealed_in_event.unwrap();
    assert!(revealed.contains(&bolt));
    assert!(revealed.contains(&mountain));
    assert!(revealed.contains(&giant));
    assert!(!revealed.contains(&island));
    assert!(!revealed.contains(&forest));

    // CR 701.20d + CR 400.2: Filtered response boundary — reveal names are public
    // across both seated viewers (P0, P1) and spectators, while library positions
    // stay hidden (card_ids filtered out of the event for hidden library cards).
    for viewer in [P0, P1] {
        let filtered_events = engine::game::visibility::filter_events_for_viewer(
            &result.events,
            runner.state(),
            viewer,
        );
        let (card_names, card_ids) = filtered_events
            .iter()
            .find_map(|e| match e {
                GameEvent::CardsRevealed {
                    card_names,
                    card_ids,
                    ..
                } => Some((card_names, card_ids)),
                _ => None,
            })
            .expect("CardsRevealed must be present in filtered events");
        // Public reveal names:
        assert!(card_names.contains(&"Lightning Bolt".to_string()));
        assert!(card_names.contains(&"Mountain".to_string()));
        assert!(card_names.contains(&"Hill Giant".to_string()));
        // Library positions stay hidden:
        assert!(
            !card_ids.contains(&bolt),
            "reordered bolt ID must be redacted from filtered event for {viewer:?}"
        );
        assert!(
            !card_ids.contains(&mountain),
            "reordered mountain ID must be redacted from filtered event for {viewer:?}"
        );
        assert!(
            !card_ids.contains(&giant),
            "reordered giant ID must be redacted from filtered event for {viewer:?}"
        );
    }

    // 3. CR 701.20d: All 3 revealed cards were reordered to bottom in random order.
    // They stop being revealed and become new objects (bump incarnation).
    for (id, label, inc_before) in [
        (bolt, "Lightning Bolt", incarnation_bolt_before),
        (mountain, "Mountain", incarnation_mountain_before),
        (giant, "Hill Giant", incarnation_giant_before),
    ] {
        assert!(
            !runner.state().revealed_cards.contains(&id),
            "revealed_cards must not contain {label}"
        );
        assert!(
            !runner.state().public_revealed_cards.contains(&id),
            "public_revealed_cards must not contain {label}"
        );
        assert!(
            !runner.state().holds_stack_bound_reveal(id),
            "holds_stack_bound_reveal must be false for {label}"
        );
        assert!(
            !runner.state().viewer_knows_card_identity(P0, id),
            "P0 must not know card identity for {label}"
        );
        assert!(
            !runner.state().viewer_knows_card_identity(P1, id),
            "P1 must not know card identity for {label}"
        );
        assert!(
            runner.state().objects[&id].incarnation > inc_before,
            "incarnation must be bumped for {label}"
        );
    }

    // 4. Seated and spectator views immediately after settlement:
    // P0 view:
    let p0_view = filter_state_for_viewer(runner.state(), P0);
    assert_eq!(
        p0_view.objects[&bolt].name, HIDDEN_CARD_NAME,
        "P0 view must hide reordered bolt"
    );
    assert_eq!(
        p0_view.objects[&mountain].name, HIDDEN_CARD_NAME,
        "P0 view must hide reordered mountain"
    );
    assert_eq!(
        p0_view.objects[&giant].name, HIDDEN_CARD_NAME,
        "P0 view must hide reordered giant"
    );

    // P1 (opponent) view:
    let p1_view = filter_state_for_viewer(runner.state(), P1);
    assert_eq!(
        p1_view.objects[&bolt].name, HIDDEN_CARD_NAME,
        "P1 view must hide reordered bolt"
    );
    assert_eq!(
        p1_view.objects[&mountain].name, HIDDEN_CARD_NAME,
        "P1 view must hide reordered mountain"
    );
    assert_eq!(
        p1_view.objects[&giant].name, HIDDEN_CARD_NAME,
        "P1 view must hide reordered giant"
    );

    // Spectator (unseated) view:
    let spectator_view = filter_state_for_unseated_viewer(runner.state());
    assert_eq!(
        spectator_view.objects[&bolt].name, HIDDEN_CARD_NAME,
        "Spectator view must hide reordered bolt"
    );
    assert_eq!(
        spectator_view.objects[&mountain].name, HIDDEN_CARD_NAME,
        "Spectator view must hide reordered mountain"
    );
    assert_eq!(
        spectator_view.objects[&giant].name, HIDDEN_CARD_NAME,
        "Spectator view must hide reordered giant"
    );

    // 5. Verify the remaining library cards: Island and Forest are on top
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .library
            .front()
            .copied(),
        Some(island),
        "Island is on top of library"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        5,
        "All 5 cards remain in library"
    );
}
