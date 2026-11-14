//! Runtime regression for targeted-player mass graveyard shuffles.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const REMINISCE_ORACLE: &str = "Target player shuffles their graveyard into their library.";

/// CR 400.3 + CR 608.2c + CR 701.24a: Reminisce moves every card from the
/// chosen player's graveyard into that player's library and shuffles that
/// library. The caster's zones and shuffle history are independent.
#[test]
fn reminisce_moves_and_shuffles_only_the_target_players_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reminisce = scenario
        .add_spell_to_hand_from_oracle(P0, "Reminisce", false, REMINISCE_ORACLE)
        .id();

    let p0_library = scenario.add_card_to_library_top(P0, "Caster Library Card");
    let p1_library = scenario.add_card_to_library_top(P1, "Target Library Card");
    let p0_graveyard = scenario
        .add_creature_to_graveyard(P0, "Caster Graveyard Card", 1, 1)
        .id();
    let p1_graveyard_a = scenario
        .add_creature_to_graveyard(P1, "Target Graveyard Card A", 1, 1)
        .id();
    let p1_graveyard_b = scenario
        .add_creature_to_graveyard(P1, "Target Graveyard Card B", 1, 1)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(reminisce).target_player(P1).resolve();

    outcome.assert_zone(&[p1_graveyard_a, p1_graveyard_b], Zone::Library);
    outcome.assert_zone(&[p0_graveyard], Zone::Graveyard);
    assert_eq!(outcome.zone_of(p0_library), Zone::Library);
    assert_eq!(outcome.zone_of(p1_library), Zone::Library);

    let p0_state = &outcome.state().players[P0.0 as usize];
    let p1_state = &outcome.state().players[P1.0 as usize];
    assert!(p0_state.graveyard.contains(&p0_graveyard));
    assert!(!p0_state.library.contains(&p0_graveyard));
    assert!(p0_state.library.contains(&p0_library));
    assert!(p1_state.graveyard.is_empty());
    assert!(p1_state.library.contains(&p1_library));
    assert!(p1_state.library.contains(&p1_graveyard_a));
    assert!(p1_state.library.contains(&p1_graveyard_b));

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
    assert_eq!(shuffled_players, vec![P1]);
}

/// CR 701.24a: The designated player still shuffles when the prospective
/// graveyard population is empty. The participant ledger, rather than a moved
/// object, carries the target player through to the terminal shuffle.
#[test]
fn reminisce_empty_graveyard_still_shuffles_the_target_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reminisce = scenario
        .add_spell_to_hand_from_oracle(P0, "Reminisce", false, REMINISCE_ORACLE)
        .id();
    scenario.add_card_to_library_top(P0, "Caster Library Card");
    scenario.add_card_to_library_top(P1, "Target Library Card");

    let mut runner = scenario.build();
    let outcome = runner.cast(reminisce).target_player(P1).resolve();

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
    assert_eq!(
        shuffled_players,
        vec![P1],
        "the empty designated set must still publish and shuffle P1"
    );
}

const HEAD_GAMES_ORACLE: &str = "Target opponent puts the cards from their hand on top of their library. Search that player's library for that many cards. The player puts those cards into their hand, then shuffles.";

/// CR 701.23a + CR 701.24a: Head Games searches and shuffles the targeted
/// opponent's library. The search result replaces object targets only; the
/// original player target remains available to the final parent-target shuffle.
#[test]
fn head_games_search_selection_preserves_targeted_opponent_for_final_shuffle() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let head_games = scenario
        .add_spell_to_hand_from_oracle(P0, "Head Games", false, HEAD_GAMES_ORACLE)
        .id();
    let p0_library = scenario.add_card_to_library_top(P0, "Caster Library Card");
    let p1_hand = scenario.add_card_to_hand(P1, "Target Hand Card");
    let p1_library = scenario.add_card_to_library_top(P1, "Target Library Card");

    let mut runner = scenario.build();
    let outcome = runner.cast(head_games).target_player(P1).resolve();
    match outcome.final_waiting_for() {
        engine::types::game_state::WaitingFor::SearchChoice { cards, .. } => {
            assert!(
                cards.contains(&p1_library),
                "the targeted opponent's original library card must be searchable: {cards:?}"
            );
        }
        other => panic!("expected Head Games search choice, got {other:?}"),
    }

    let selection = runner
        .act(engine::types::actions::GameAction::SelectCards {
            cards: vec![p1_library],
        })
        .expect("selecting a card from the targeted opponent's library resolves Head Games");

    assert_eq!(runner.state().objects[&p1_library].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&p1_hand].zone, Zone::Library);
    assert_eq!(runner.state().objects[&p0_library].zone, Zone::Library);
    let shuffled_players: Vec<_> = selection
        .events
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
    assert_eq!(shuffled_players, vec![P1]);
}
