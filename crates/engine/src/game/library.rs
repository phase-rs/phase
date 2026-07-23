//! Final authority for resolved in-place library order mutations.

use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;
use crate::types::resolved_commands::{
    validate_library_shuffle_receipt, ResolvedLibraryShuffleCommand,
    ResolvedLibraryShuffleReplayInvariantError,
};

/// Resolves, applies, and journals one library shuffle.
///
/// CR 701.24a: the ordinary path samples a clone of the current ChaCha20 stream
/// to determine the final order. The real stream and library are changed only
/// by [`apply_resolved_library_shuffle`].
pub fn resolve_and_apply_library_shuffle(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<(), ResolvedLibraryShuffleReplayInvariantError> {
    let precondition_order: Vec<_> = state
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .ok_or(ResolvedLibraryShuffleReplayInvariantError::UnknownPlayer(
            player,
        ))?
        .library
        .iter()
        .copied()
        .collect();

    // Existing random consumers may have advanced the live stream before their
    // own P2 authority is factored. Synchronize the persisted high-water through
    // its one monotonic primitive before capturing this receipt's predecessor.
    state.capture_rng_word_pos();
    let pre_word_pos = state.rng_word_pos;
    let mut receipt_rng = state.rng.clone();
    let mut resulting_order = im::Vector::from(precondition_order.clone());
    crate::util::im_ext::shuffle_vector(&mut resulting_order, &mut receipt_rng);
    let command = ResolvedLibraryShuffleCommand {
        player,
        precondition_order,
        resulting_order: resulting_order.iter().copied().collect(),
        pre_word_pos,
        post_word_pos: receipt_rng.get_word_pos(),
        cause: state.current_or_begin_rules_execution_node(),
    };

    apply_resolved_library_shuffle(state, &command, events)?;
    state
        .resolved_rules_journal
        .record_library_shuffle(command)
        .expect("resolved library shuffle must have a live journal cause");
    Ok(())
}

/// Applies one exact library permutation and entropy receipt without sampling
/// ChaCha20, inspecting a new top card, or rerunning an effect.
pub fn apply_resolved_library_shuffle(
    state: &mut GameState,
    command: &ResolvedLibraryShuffleCommand,
    events: &mut Vec<GameEvent>,
) -> Result<(), ResolvedLibraryShuffleReplayInvariantError> {
    validate_library_shuffle_receipt(command)?;
    let player_index = state
        .players
        .iter()
        .position(|candidate| candidate.id == command.player)
        .ok_or(ResolvedLibraryShuffleReplayInvariantError::UnknownPlayer(
            command.player,
        ))?;
    let current_order: Vec<_> = state.players[player_index]
        .library
        .iter()
        .copied()
        .collect();
    if current_order != command.precondition_order {
        return Err(ResolvedLibraryShuffleReplayInvariantError::LibraryOrderPreconditionMismatch);
    }
    if state.rng_word_pos != command.pre_word_pos {
        return Err(
            ResolvedLibraryShuffleReplayInvariantError::RngWordPositionPreconditionMismatch {
                expected: command.pre_word_pos,
                found: state.rng_word_pos,
            },
        );
    }
    let current_word_pos = state.rng.get_word_pos();
    if current_word_pos != command.pre_word_pos {
        return Err(
            ResolvedLibraryShuffleReplayInvariantError::RngCursorPositionPreconditionMismatch {
                expected: command.pre_word_pos,
                found: current_word_pos,
            },
        );
    }

    // CR 701.24a: Install the already-randomized order once. The applier never
    // calls `shuffle_vector`, so retained-prefix replay cannot consume entropy
    // or choose a different permutation.
    state.players[player_index].library = im::Vector::from(command.resulting_order.clone());
    state
        .advance_rng_high_water(command.post_word_pos)
        .expect("validated library entropy receipt cannot rewind the RNG");

    // CR 401.5 + CR 611.3a: the changed top card can alter a live static
    // ability. This mark is derived cache state, rebuilt from the installed
    // order rather than stored as an independent command operand.
    crate::game::layers::mark_layers_full_if_top_of_library_static_live(state);
    // CR 701.24a + CR 701.24e: every library shuffle, including an identity
    // shuffle of a zero- or one-card library, publishes one action event.
    events.push(GameEvent::PlayerPerformedAction {
        player_id: command.player,
        action: PlayerActionKind::ShuffledLibrary,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;
    use crate::types::resolved_commands::{ResolvedPlayerEdit, ResolvedRulesCommand};
    use crate::types::zones::Zone;

    fn state_with_library(seed: u64) -> GameState {
        let mut state = GameState::new_two_player(seed);
        for card in 1..=5 {
            create_object(
                &mut state,
                CardId(card),
                PlayerId(0),
                format!("Card {card}"),
                Zone::Library,
            );
        }
        state
    }

    fn recorded_shuffle(state: &GameState) -> ResolvedLibraryShuffleCommand {
        state
            .resolved_rules_journal
            .entries()
            .iter()
            .find_map(|entry| match &entry.command {
                Some(ResolvedRulesCommand::LibraryShuffle(command)) => Some(command.clone()),
                _ => None,
            })
            .expect("ordinary shuffle must record one command")
    }

    #[test]
    fn shuffle_library_matches_seeded_pre_factor_permutation() {
        let mut state = state_with_library(0x733);
        let mut expected_order = state.players[0].library.clone();
        let mut expected_rng = state.rng.clone();
        crate::util::im_ext::shuffle_vector(&mut expected_order, &mut expected_rng);
        let mut events = Vec::new();

        crate::game::effects::change_zone::shuffle_library(&mut state, PlayerId(0), &mut events);

        assert_eq!(state.players[0].library, expected_order);
        assert_eq!(state.rng.get_word_pos(), expected_rng.get_word_pos());
        assert_eq!(state.rng_word_pos, expected_rng.get_word_pos());
        assert_eq!(
            events,
            vec![GameEvent::PlayerPerformedAction {
                player_id: PlayerId(0),
                action: PlayerActionKind::ShuffledLibrary,
            }]
        );
    }

    #[test]
    fn replay_installs_recorded_order_without_sampling_rng_or_revealing_cards() {
        let pre_state = state_with_library(0x733);
        let mut ordinary = pre_state.clone();
        let mut ordinary_events = Vec::new();
        crate::game::effects::change_zone::shuffle_library(
            &mut ordinary,
            PlayerId(0),
            &mut ordinary_events,
        );
        let command = recorded_shuffle(&ordinary);

        let mut replay = pre_state;
        // A different stream seed proves the applier cannot recreate the order
        // by shuffling. Its stream position still satisfies the receipt.
        replay.rng = ChaCha20Rng::seed_from_u64(0x0BAD_5EED);
        replay.rng_word_pos = command.pre_word_pos;
        let mut replay_events = Vec::new();
        apply_resolved_library_shuffle(&mut replay, &command, &mut replay_events).unwrap();

        assert_eq!(replay.players[0].library, ordinary.players[0].library);
        assert_eq!(replay.rng_word_pos, ordinary.rng_word_pos);
        assert_eq!(replay.rng.get_word_pos(), ordinary.rng.get_word_pos());
        assert_eq!(replay_events, ordinary_events);
        assert_eq!(replay.revealed_cards, ordinary.revealed_cards);
        assert_eq!(replay.public_revealed_cards, ordinary.public_revealed_cards);
    }

    #[test]
    fn identity_shuffle_records_a_valid_zero_card_entropy_receipt() {
        let mut state = GameState::new_two_player(0x733);
        let mut events = Vec::new();
        crate::game::effects::change_zone::shuffle_library(&mut state, PlayerId(0), &mut events);
        let command = recorded_shuffle(&state);

        assert!(command.precondition_order.is_empty());
        assert!(command.resulting_order.is_empty());
        assert_eq!(command.pre_word_pos, command.post_word_pos);
        assert_eq!(state.rng_word_pos, command.post_word_pos);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn replay_rejects_wrong_predecessor_or_entropy_position_before_mutation() {
        let pre_state = state_with_library(0x733);
        let mut ordinary = pre_state.clone();
        crate::game::effects::change_zone::shuffle_library(
            &mut ordinary,
            PlayerId(0),
            &mut Vec::new(),
        );
        let command = recorded_shuffle(&ordinary);

        let mut wrong_order = pre_state.clone();
        wrong_order.players[0].library.swap(0, 1);
        let order_before = wrong_order.players[0].library.clone();
        assert!(matches!(
            apply_resolved_library_shuffle(&mut wrong_order, &command, &mut Vec::new()),
            Err(ResolvedLibraryShuffleReplayInvariantError::LibraryOrderPreconditionMismatch)
        ));
        assert_eq!(wrong_order.players[0].library, order_before);

        let mut wrong_entropy = pre_state;
        wrong_entropy.rng_word_pos = command.pre_word_pos.saturating_add(1);
        assert!(matches!(
            apply_resolved_library_shuffle(&mut wrong_entropy, &command, &mut Vec::new()),
            Err(
                ResolvedLibraryShuffleReplayInvariantError::RngWordPositionPreconditionMismatch { .. }
            )
        ));
        assert_eq!(
            wrong_entropy.players[0]
                .library
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            command.precondition_order
        );
    }

    #[test]
    fn two_shuffles_compose_before_a_scalar_edit() {
        let mut state = state_with_library(0x733);
        let mut events = Vec::new();
        crate::game::effects::change_zone::shuffle_library(&mut state, PlayerId(0), &mut events);
        let after_first = state.rng_word_pos;
        crate::game::effects::change_zone::shuffle_library(&mut state, PlayerId(0), &mut events);
        let after_second = state.rng_word_pos;
        state
            .resolve_and_apply_player_edit(PlayerId(0), ResolvedPlayerEdit::Life { delta: -2 })
            .unwrap();

        assert!(after_second >= after_first);
        assert_eq!(state.players[0].life, 18);
        assert_eq!(
            state
                .resolved_rules_journal
                .entries()
                .iter()
                .filter(|entry| {
                    matches!(
                        entry.command.as_ref(),
                        Some(ResolvedRulesCommand::LibraryShuffle(_))
                    )
                })
                .count(),
            2
        );
    }
}
