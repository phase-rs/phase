use engine::game::deck_loading::PlayerDeckList;
use engine::types::format::FormatConfig;
use phase_ai::config::Platform;

use crate::types::*;

struct NoOpResolver;
impl DeckResolver for NoOpResolver {
    fn resolve(&self, _choice: &DeckChoice) -> Result<PlayerDeckList, String> {
        Ok(PlayerDeckList {
            main_deck: vec![],
            sideboard: vec![],
            commander: vec![],
            ..Default::default()
        })
    }
}

fn ctx() -> ReducerCtx<'static> {
    static RESOLVER: NoOpResolver = NoOpResolver;
    ReducerCtx {
        platform: Platform::Native,
        deck_resolver: &RESOLVER,
    }
}

fn two_player_full() -> SeatState {
    SeatState {
        seats: vec![
            SeatKind::HostHuman,
            SeatKind::Ai {
                difficulty: phase_ai::config::AiDifficulty::Medium,
                deck: DeckChoice::Random,
            },
        ],
        tokens: vec!["host-token".to_string(), String::new()],
        format: FormatConfig::standard(),
        game_started: false,
    }
}

fn two_player_waiting() -> SeatState {
    SeatState {
        seats: vec![SeatKind::HostHuman, SeatKind::WaitingHuman],
        tokens: vec!["host-token".to_string(), String::new()],
        format: FormatConfig::standard(),
        game_started: false,
    }
}

#[test]
fn start_on_full_room_succeeds() {
    let mut state = two_player_full();
    let delta = crate::apply(&mut state, SeatMutation::Start, &ctx()).unwrap();
    assert!(delta.now_started);
    assert!(state.game_started);
}

#[test]
fn start_on_not_full_room_fails() {
    let mut state = two_player_waiting();
    let err = crate::apply(&mut state, SeatMutation::Start, &ctx()).unwrap_err();
    assert_eq!(err, SeatError::NotFull);
    assert!(!state.game_started);
}

#[test]
fn start_after_game_started_fails() {
    let mut state = two_player_full();
    state.game_started = true;
    let err = crate::apply(&mut state, SeatMutation::Start, &ctx()).unwrap_err();
    assert_eq!(err, SeatError::GameStarted);
}

#[test]
fn is_full_requires_all_claimed() {
    let full = two_player_full();
    assert!(full.is_full());

    let waiting = two_player_waiting();
    assert!(!waiting.is_full());
}

#[test]
fn is_full_with_joined_human() {
    let state = SeatState {
        seats: vec![SeatKind::HostHuman, SeatKind::JoinedHuman],
        tokens: vec!["host".to_string(), "guest".to_string()],
        format: FormatConfig::standard(),
        game_started: false,
    };
    assert!(state.is_full());
}

#[test]
fn to_view_strips_tokens() {
    let state = two_player_full();
    let view = state.to_view();
    assert_eq!(view.seats, state.seats);
    assert!(view.team_info.is_empty());
    assert!(view.is_full);
    assert!(!view.game_started);
    // SeatView has no tokens field — the type itself enforces the invariant
}

#[test]
fn non_team_formats_omit_team_metadata() {
    for format in [FormatConfig::standard(), FormatConfig::commander()] {
        let state = SeatState {
            seats: vec![SeatKind::HostHuman, SeatKind::WaitingHuman],
            tokens: vec!["host".to_string(), String::new()],
            format,
            game_started: false,
        };
        let view = state.to_view();
        assert!(view.team_info.is_empty());

        let json = serde_json::to_value(&view).unwrap();
        assert!(json.get("teamInfo").is_none());
    }
}

#[test]
fn two_headed_giant_team_metadata_maps_seats() {
    let state = SeatState {
        seats: vec![
            SeatKind::HostHuman,
            SeatKind::WaitingHuman,
            SeatKind::WaitingHuman,
            SeatKind::WaitingHuman,
        ],
        tokens: vec![
            "host".to_string(),
            String::new(),
            String::new(),
            String::new(),
        ],
        format: FormatConfig::two_headed_giant(),
        game_started: false,
    };
    let view = state.to_view();
    let team_indices: Vec<u8> = view
        .team_info
        .iter()
        .map(|info| info.unwrap().team_index)
        .collect();
    let positions: Vec<u8> = view
        .team_info
        .iter()
        .map(|info| info.unwrap().position_in_team)
        .collect();

    assert_eq!(team_indices, vec![0, 0, 1, 1]);
    assert_eq!(positions, vec![0, 1, 0, 1]);
}

#[test]
fn two_headed_giant_team_metadata_survives_seat_mutations() {
    let mut state = SeatState {
        seats: vec![
            SeatKind::HostHuman,
            SeatKind::WaitingHuman,
            SeatKind::JoinedHuman,
            SeatKind::WaitingHuman,
        ],
        tokens: vec![
            "host".to_string(),
            String::new(),
            "guest".to_string(),
            String::new(),
        ],
        format: FormatConfig::two_headed_giant(),
        game_started: false,
    };

    crate::apply(
        &mut state,
        SeatMutation::SetKind {
            seat_index: 1,
            expected_kind: None,
            kind: SeatKind::Ai {
                difficulty: phase_ai::config::AiDifficulty::Medium,
                deck: DeckChoice::Random,
            },
        },
        &ctx(),
    )
    .unwrap();

    let view = state.to_view();
    assert_eq!(
        view.team_info,
        vec![
            Some(SeatTeamInfo {
                team_index: 0,
                position_in_team: 0
            }),
            Some(SeatTeamInfo {
                team_index: 0,
                position_in_team: 1
            }),
            Some(SeatTeamInfo {
                team_index: 1,
                position_in_team: 0
            }),
            Some(SeatTeamInfo {
                team_index: 1,
                position_in_team: 1
            }),
        ]
    );
}

#[test]
fn is_pregame_reflects_game_started() {
    let mut state = two_player_full();
    assert!(state.is_pregame());
    state.game_started = true;
    assert!(!state.is_pregame());
}

#[test]
fn waiting_seat_can_become_ai() {
    let mut state = two_player_waiting();
    let delta = crate::apply(
        &mut state,
        SeatMutation::SetKind {
            seat_index: 1,
            expected_kind: None,
            kind: SeatKind::Ai {
                difficulty: phase_ai::config::AiDifficulty::Hard,
                deck: DeckChoice::Random,
            },
        },
        &ctx(),
    )
    .unwrap();
    assert_eq!(delta.new_ai.len(), 1);
    assert!(matches!(state.seats[1], SeatKind::Ai { .. }));
}

fn assignment(seat_index: u8, expected_kind: Option<SeatKind>) -> SeatMutation {
    SeatMutation::SetKind {
        seat_index,
        kind: SeatKind::Ai {
            difficulty: phase_ai::config::AiDifficulty::Hard,
            deck: DeckChoice::DeckList(Box::new(engine::starter_decks::DeckData {
                main_deck: vec!["Forest".to_string()],
                ..Default::default()
            })),
        },
        expected_kind,
    }
}

struct RecordingResolver(std::cell::Cell<usize>);

impl DeckResolver for RecordingResolver {
    fn resolve(&self, choice: &DeckChoice) -> Result<PlayerDeckList, String> {
        self.0.set(self.0.get() + 1);
        match choice {
            DeckChoice::DeckList(deck) => Ok(PlayerDeckList {
                main_deck: deck.main_deck.clone(),
                ..Default::default()
            }),
            DeckChoice::Named(_) | DeckChoice::Random => Err("unexpected deck".to_string()),
        }
    }
}

fn assert_empty_delta(delta: &SeatDelta) {
    assert!(delta.mutated_seats.is_empty());
    assert!(delta.invalidated_tokens.is_empty());
    assert!(delta.removed_ai.is_empty());
    assert!(delta.new_ai.is_empty());
    assert!(delta.renumbering.is_none());
    assert!(!delta.now_started);
}

#[test]
fn conditional_assignment_preserves_a_guest_who_joined_after_enqueue() {
    let mut state = two_player_waiting();
    let queued = assignment(1, Some(SeatKind::WaitingHuman));
    state.seats[1] = SeatKind::JoinedHuman;
    state.tokens[1] = "guest-token".to_string();
    let before = serde_json::to_value(&state).unwrap();
    let resolver = RecordingResolver(std::cell::Cell::new(0));
    let context = ReducerCtx {
        platform: Platform::Native,
        deck_resolver: &resolver,
    };

    let delta = crate::apply(&mut state, queued, &context).unwrap();

    assert_eq!(state.seats[1], SeatKind::JoinedHuman);
    assert_eq!(state.tokens[1], "guest-token");
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    assert_empty_delta(&delta);
    assert_eq!(resolver.0.get(), 0, "stale intent must not resolve a deck");

    let mut waiting = two_player_waiting();
    let delta = crate::apply(
        &mut waiting,
        assignment(1, Some(SeatKind::WaitingHuman)),
        &context,
    )
    .unwrap();
    assert_eq!(resolver.0.get(), 1);
    assert_eq!(delta.mutated_seats, vec![1]);
    assert_eq!(delta.new_ai[0].2.main_deck, vec!["Forest"]);
    assert!(matches!(waiting.seats[1], SeatKind::Ai { .. }));
}

#[test]
fn conditional_assignment_retains_validation_precedence_and_transition_legality() {
    for (seat_index, expected) in [
        (0, SeatKind::HostHuman),
        (0, SeatKind::WaitingHuman),
        (2, SeatKind::WaitingHuman),
    ] {
        let mut state = two_player_waiting();
        let before = serde_json::to_value(&state).unwrap();
        assert_eq!(
            crate::apply(&mut state, assignment(seat_index, Some(expected)), &ctx()).unwrap_err(),
            SeatError::SeatImmutable
        );
        assert_eq!(serde_json::to_value(&state).unwrap(), before);
    }
    let mut state = two_player_waiting();
    let before = serde_json::to_value(&state).unwrap();
    assert_eq!(
        crate::apply(
            &mut state,
            SeatMutation::SetKind {
                seat_index: 1,
                kind: SeatKind::JoinedHuman,
                expected_kind: Some(SeatKind::WaitingHuman),
            },
            &ctx(),
        )
        .unwrap_err(),
        SeatError::InvalidTransition
    );
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    state.game_started = true;
    for seat_index in [0, 1, 2] {
        assert_eq!(
            crate::apply(
                &mut state,
                assignment(seat_index, Some(SeatKind::JoinedHuman)),
                &ctx(),
            )
            .unwrap_err(),
            SeatError::GameStarted
        );
    }
}

#[test]
fn conditional_assignment_mismatch_preserves_other_occupants_and_ai_decks() {
    let mut state = two_player_waiting();
    state
        .seats
        .extend([SeatKind::JoinedHuman, SeatKind::JoinedHuman]);
    state
        .tokens
        .extend(["guest-two".to_string(), "guest-three".to_string()]);
    state.format = FormatConfig::commander();
    let resolver = RecordingResolver(std::cell::Cell::new(0));
    let context = ReducerCtx {
        platform: Platform::Native,
        deck_resolver: &resolver,
    };
    crate::apply(
        &mut state,
        assignment(1, Some(SeatKind::WaitingHuman)),
        &context,
    )
    .unwrap();
    let before = serde_json::to_value(&state).unwrap();
    for seat_index in [1, 2] {
        let delta = crate::apply(
            &mut state,
            assignment(seat_index, Some(SeatKind::WaitingHuman)),
            &context,
        )
        .unwrap();
        assert_empty_delta(&delta);
        assert_eq!(serde_json::to_value(&state).unwrap(), before);
    }
    assert_eq!(resolver.0.get(), 1);

    // Full typed equality, not just the AI discriminator: a different deck
    // expectation cannot authorize an edit to the installed AI.
    let delta = crate::apply(
        &mut state,
        SeatMutation::SetKind {
            seat_index: 1,
            kind: SeatKind::WaitingHuman,
            expected_kind: Some(SeatKind::Ai {
                difficulty: phase_ai::config::AiDifficulty::Hard,
                deck: DeckChoice::Random,
            }),
        },
        &context,
    )
    .unwrap();
    assert_empty_delta(&delta);
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
}

#[test]
fn unconditional_kick_and_replace_invalidate_only_the_current_guest() {
    for kind in [
        SeatKind::WaitingHuman,
        match assignment(1, None) {
            SeatMutation::SetKind { kind, .. } => kind,
            SeatMutation::Remove { .. } | SeatMutation::Start => unreachable!(),
        },
    ] {
        let mut state = two_player_waiting();
        state.seats[1] = SeatKind::JoinedHuman;
        state.tokens[1] = "guest-token".to_string();
        state.seats.push(SeatKind::JoinedHuman);
        state.tokens.push("other-guest".to_string());
        state.format = FormatConfig::commander();
        let delta = crate::apply(
            &mut state,
            SeatMutation::SetKind {
                seat_index: 1,
                kind: kind.clone(),
                expected_kind: None,
            },
            &ctx(),
        )
        .unwrap();
        assert_eq!(delta.invalidated_tokens, vec!["guest-token"]);
        assert_eq!(state.seats[1], kind);
        assert_eq!(state.tokens, vec!["host-token", "", "other-guest"]);
        assert_eq!(state.seats[2], SeatKind::JoinedHuman);
    }
}

#[test]
fn remove_waiting_seat_respects_min_players() {
    let mut state = SeatState {
        seats: vec![
            SeatKind::HostHuman,
            SeatKind::WaitingHuman,
            SeatKind::WaitingHuman,
        ],
        tokens: vec!["host".to_string(), String::new(), String::new()],
        format: FormatConfig::commander(),
        game_started: false,
    };
    let delta = crate::apply(&mut state, SeatMutation::Remove { seat_index: 2 }, &ctx()).unwrap();
    assert_eq!(state.seats.len(), 2);
    assert!(delta.renumbering.is_some());
}

#[test]
fn remove_rejects_claimed_human_seat() {
    let mut state = SeatState {
        seats: vec![
            SeatKind::HostHuman,
            SeatKind::JoinedHuman,
            SeatKind::WaitingHuman,
        ],
        tokens: vec!["host".to_string(), "guest".to_string(), String::new()],
        format: FormatConfig::commander(),
        game_started: false,
    };
    let err = crate::apply(&mut state, SeatMutation::Remove { seat_index: 1 }, &ctx()).unwrap_err();
    assert_eq!(err, SeatError::SeatClaimed);
}
