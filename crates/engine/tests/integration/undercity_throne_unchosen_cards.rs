//! Issue #3863: Undercity's Throne of the Dead Three reveals the top ten cards and puts one
//! creature card from among them onto the battlefield. Revealing leaves the other cards in
//! the library (CR 701.20b), so the room's final "Then shuffle" shuffles them back in; they
//! must not be milled.

use engine::game::dungeon::{DungeonId, DungeonProgress};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

#[test]
fn undercity_throne_shuffles_unchosen_revealed_cards_back_into_library() {
    let db = crate::support::shared_card_db().expect("integration card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let unchosen: Vec<_> = (0..9)
        .map(|_| scenario.add_real_card(P0, "Forest", Zone::Library, db))
        .collect();
    let creature = scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };
    state.initiative = Some(P0);
    state.dungeon_progress.insert(
        P0,
        DungeonProgress {
            current_dungeon: Some(DungeonId::Undercity),
            current_room: 6, // Archives leads directly to Throne of the Dead Three.
            completed: Default::default(),
        },
    );

    // CR 726.2: The initiative holder ventures into Undercity at their upkeep.
    for _ in 0..2 {
        runner
            .act(GameAction::PassPriority)
            .expect("pass in P1's end step");
    }
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(runner.state().active_player, P0);
    assert_eq!(runner.state().stack.len(), 1);
    let entry = &runner.state().stack[0];
    assert_eq!(entry.controller, P0);
    assert!(
        matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. }),
        "the initiative upkeep trigger must be on the stack"
    );

    // CR 701.49b + CR 309.4c: Venturing into the next room triggers its room ability.
    for _ in 0..2 {
        runner
            .act(GameAction::PassPriority)
            .expect("resolve the initiative trigger");
    }
    assert_eq!(runner.state().dungeon_progress[&P0].current_room, 8);
    assert_eq!(runner.state().stack.len(), 1);
    let entry = &runner.state().stack[0];
    assert_eq!(entry.controller, P0);
    assert!(
        matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. }),
        "the Throne room trigger must be on the stack"
    );

    // Throne of the Dead Three: "Reveal the top ten cards of your library. Put a
    // creature card from among them onto the battlefield with three +1/+1 counters
    // on it. It gains hexproof until your next turn. Then shuffle."
    for _ in 0..2 {
        runner
            .act(GameAction::PassPriority)
            .expect("resolve the Throne room trigger");
    }
    let WaitingFor::DigChoice {
        player,
        cards,
        selectable_cards,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected Throne's creature choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(cards.len(), 10);
    assert!(unchosen.iter().all(|id| cards.contains(id)));
    assert_eq!(selectable_cards.as_slice(), &[creature]);

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![creature],
        })
        .expect("choose Grizzly Bears");
    // CR 117.3b: The active player receives priority after the ability resolves.
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    let state = runner.state();
    assert!(state.stack.is_empty());

    // CR 608.2c: Follow the room's instructions in order.
    let bears = &state.objects[&creature];
    assert_eq!(bears.zone, Zone::Battlefield);
    assert_eq!(bears.counters.get(&CounterType::Plus1Plus1), Some(&3));
    assert!(bears.has_keyword(&Keyword::Hexproof));

    // CR 701.20b: Revealing a card leaves it in its zone.
    let p0 = &state.players[0];
    for id in unchosen {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Library,
            "unchosen card {id:?}"
        );
        assert!(p0.library.contains(&id));
        assert!(!p0.graveyard.contains(&id));
    }
    assert!(p0.graveyard.is_empty());
    assert_eq!(p0.library.len(), 9);
    // CR 701.24a: The final instruction shuffles the remaining library.
    assert!(result.events.iter().any(|event| matches!(
        event,
        GameEvent::PlayerPerformedAction {
            player_id: P0,
            action: PlayerActionKind::ShuffledLibrary,
            ..
        }
    )));
}
