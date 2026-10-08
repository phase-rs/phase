//! All-player private-zone wheels keep every move, shuffle, and draw local to
//! the player whose iteration is resolving.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, Effect, MassLibraryShuffleMode, PlayerFilter, QuantityExpr, TargetFilter,
};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const ECHO_OF_EONS_ORACLE: &str =
    "Each player shuffles their hand and graveyard into their library, then draws seven cards.";

/// CR 608.2c + CR 701.24a: Echo of Eons resolves one complete private-zone
/// wheel for each player. Both origin moves, the terminal shuffle, and the draw
/// use the current iteration's player rather than the spell controller.
#[test]
fn echo_of_eons_scopes_every_origin_shuffle_and_draw_to_each_player() {
    let parsed = parse_effect_chain(ECHO_OF_EONS_ORACLE, AbilityKind::Spell);
    assert_eq!(parsed.player_scope, Some(PlayerFilter::All));
    assert!(matches!(
        parsed.effect.as_ref(),
        Effect::ChangeZoneAll {
            origin: Some(Zone::Hand),
            destination: Zone::Library,
            target: TargetFilter::ScopedPlayer,
            library_shuffle: MassLibraryShuffleMode::TerminalShuffle,
            ..
        }
    ));
    let graveyard = parsed.sub_ability.as_deref().expect("graveyard move");
    assert!(matches!(
        graveyard.effect.as_ref(),
        Effect::ChangeZoneAll {
            origin: Some(Zone::Graveyard),
            destination: Zone::Library,
            target: TargetFilter::ScopedPlayer,
            library_shuffle: MassLibraryShuffleMode::TerminalShuffle,
            ..
        }
    ));
    let shuffle = graveyard.sub_ability.as_deref().expect("terminal shuffle");
    assert!(matches!(
        shuffle.effect.as_ref(),
        Effect::Shuffle {
            target: TargetFilter::ScopedPlayer,
        }
    ));
    let draw = shuffle.sub_ability.as_deref().expect("seven-card draw");
    assert!(matches!(
        draw.effect.as_ref(),
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 7 },
            target: TargetFilter::ScopedPlayer,
        }
    ));

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let echo = scenario
        .add_spell_to_hand_from_oracle(P0, "Echo of Eons", false, ECHO_OF_EONS_ORACLE)
        .id();

    let mut moved_cards = Vec::new();
    for player in [P0, P1, P2] {
        for index in 0..2 {
            let hand_name = format!("P{} hand {index}", player.0);
            scenario.add_card_to_hand(player, &hand_name);
            moved_cards.push((hand_name, Zone::Hand, player));

            let graveyard_name = format!("P{} graveyard {index}", player.0);
            scenario
                .add_creature_to_graveyard(player, &graveyard_name, 1, 1)
                .id();
            moved_cards.push((graveyard_name, Zone::Graveyard, player));
        }
        for index in 0..10 {
            scenario.add_card_to_library_top(player, &format!("P{} library {index}", player.0));
        }
    }

    let mut runner = scenario.build();
    let outcome = runner.cast(echo).resolve();

    for (name, origin, owner) in moved_cards {
        assert!(
            outcome.events().iter().any(|event| matches!(
                event,
                GameEvent::ZoneChanged {
                    from: Some(from),
                    to: Zone::Library,
                    record,
                    ..
                } if *from == origin && record.name == name && record.owner == owner
            )),
            "{name} must move from {origin:?} to P{}'s library",
            owner.0
        );
    }

    let mut shuffled_players: Vec<_> = outcome
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
    shuffled_players.sort_by_key(|player| player.0);
    assert_eq!(shuffled_players, vec![P0, P1, P2]);

    for player in [P0, P1, P2] {
        assert_eq!(
            outcome.state().players[player.0 as usize].cards_drawn_this_turn,
            7,
            "P{} must draw seven from their own shuffled library",
            player.0
        );
    }
}
