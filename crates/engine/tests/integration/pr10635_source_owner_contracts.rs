//! Production death-trigger coverage for source-bound owner-library shuffles.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Full Oracle verified against https://api.scryfall.com/cards/named?exact=Alabaster%20Dragon
const ALABASTER_DRAGON: &str =
    "Flying\nWhen this creature dies, shuffle it into its owner's library.";
// Full Oracle verified against https://api.scryfall.com/cards/named?exact=Murder
const MURDER: &str = "Destroy target creature.";

#[test]
fn alabaster_dragon_death_follows_the_source_into_its_owners_shuffled_library() {
    for controller in [P0, P1] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let dragon = scenario
            .add_creature(P0, "Alabaster Dragon", 4, 4)
            .controlled_by(controller)
            .from_oracle_text_with_keywords(&["Flying"], ALABASTER_DRAGON)
            .id();
        let other_dragon = scenario
            .add_creature(P0, "Alabaster Dragon", 4, 4)
            .from_oracle_text_with_keywords(&["Flying"], ALABASTER_DRAGON)
            .id();
        let unrelated_graveyard_card = scenario
            .add_creature_to_graveyard(P0, "Unrelated Graveyard Card", 1, 1)
            .id();
        scenario.with_library_top(P0, &["Owner A", "Owner B", "Owner C", "Owner D"]);
        scenario.with_library_top(P1, &["Opponent A", "Opponent B", "Opponent C"]);
        let murder = scenario
            .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
            .id();
        let mut runner = scenario.build();
        assert_eq!(runner.state().objects[&dragon].owner, P0);
        assert_eq!(runner.state().objects[&dragon].controller, controller);
        let opponent_library = runner.state().players[P1.0 as usize].library.clone();

        // The public spell action creates the death event and the untargeted
        // trigger; no trigger or remembered successor is supplied by the fixture.
        let outcome = runner.cast(murder).target_object(dragon).resolve();

        // CR 400.7e: the zone-change trigger finds the new object in the public
        // graveyard. Both transitions must occur, not a direct battlefield return.
        let dragon_transitions: Vec<_> = outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::ZoneChanged {
                    object_id,
                    from,
                    to,
                    ..
                } if *object_id == dragon => Some((*from, *to)),
                _ => None,
            })
            .collect();
        assert_eq!(
            dragon_transitions,
            vec![
                (Some(Zone::Battlefield), Zone::Graveyard),
                (Some(Zone::Graveyard), Zone::Library),
            ],
            "controller={controller:?}; waiting={:?}; stack={}",
            outcome.final_waiting_for(),
            outcome.state().stack.len()
        );
        outcome.assert_zone(&[dragon], Zone::Library);
        outcome.assert_zone(&[other_dragon], Zone::Battlefield);
        outcome.assert_zone(&[unrelated_graveyard_card, murder], Zone::Graveyard);

        let state = outcome.state();
        // CR 400.3: the source goes to its owner's library even when the
        // controller of its death trigger is the opponent.
        assert!(state.players[P0.0 as usize].library.contains(&dragon));
        assert!(!state.players[P0.0 as usize].graveyard.contains(&dragon));
        assert_eq!(state.players[P0.0 as usize].library.len(), 5);
        assert_eq!(state.players[P1.0 as usize].library, opponent_library);

        // CR 701.24a + CR 701.24c: moving the source alone is not completion;
        // the engine's shuffle-action authority must report the owner's shuffle.
        let shuffled_players: Vec<_> = outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } => Some(*player_id),
                _ => None,
            })
            .collect();
        assert!(
            !shuffled_players.is_empty(),
            "the owner must actually shuffle"
        );
        assert!(shuffled_players.iter().all(|player| *player == P0));

        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert!(state.stack.is_empty());
        assert!(state.resolution_stack.is_empty());
        assert!(state.deferred_triggers.is_empty());
        assert!(state.pending_trigger.is_none());
        assert!(state.pending_trigger_order.is_none());
        assert!(state.resolving_stack_entry.is_none());
        assert!(state.pending_resolution_completion.is_none());
    }
}
