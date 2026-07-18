//! Integration tests for issue #6015 — Learn's "if you didn't discard a
//! card" branch was a no-op: declining to discard (or having no card to
//! discard) dropped the Lesson-search offer entirely instead of letting the
//! player reveal a Lesson card from their sideboard and put it into hand.
//!
//! Oracle text (Eyetwitch, verified via Scryfall):
//!   Flying
//!   When this creature dies, learn. (You may reveal a Lesson card you own
//!   from outside the game and put it into your hand, or discard a card to
//!   draw a card.)
//!
//! CR 701.48a: "Learn" means "You may discard a card. If you do, draw a
//! card. If you didn't discard a card, you may reveal a Lesson card you own
//! from outside the game and put it into your hand."
//!
//! These tests drive the real resolution pipeline: Eyetwitch's dies trigger
//! is parsed from its actual Oracle text into a `ResolvedAbility` and
//! resolved via `resolve_ability_chain`; the Learn/outside-game decisions are
//! submitted as `GameAction`s through `apply`, mirroring the pattern used by
//! `braids_arisen_nightmare_decline.rs`.

use std::sync::Arc;

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::deck_loading::DeckEntry;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::actions::{GameAction, LearnOption, OutsideGameSelection};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, PlayerDeckPool, WaitingFor};
use engine::types::identifiers::CardId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const EYETWITCH_ORACLE: &str = "Flying\nWhen this creature dies, learn.";

fn face(name: &str, core_type: CoreType, subtypes: &[&str]) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            core_types: vec![core_type],
            subtypes: subtypes.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn state_with_sideboard(sideboard: Vec<DeckEntry>) -> GameState {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    state.deck_pools = vec![PlayerDeckPool {
        player: PlayerId(0),
        current_sideboard: Arc::new(sideboard),
        ..Default::default()
    }];
    state
}

/// Build Eyetwitch's "dies" trigger execute chain as a `ResolvedAbility`.
fn eyetwitch_learn_ability(state: &mut GameState) -> engine::types::ability::ResolvedAbility {
    let source = create_object(
        state,
        CardId(100),
        PlayerId(0),
        "Eyetwitch".to_string(),
        Zone::Graveyard,
    );
    let parsed = parse_oracle_text(
        EYETWITCH_ORACLE,
        "Eyetwitch",
        &[],
        &["Creature".to_string()],
        &["Eye".to_string(), "Bat".to_string()],
    );
    let trigger = parsed
        .triggers
        .first()
        .expect("Eyetwitch has a dies trigger");
    let execute = trigger
        .execute
        .as_deref()
        .expect("Eyetwitch's dies trigger has an execute chain");
    build_resolved_from_def(execute, source, PlayerId(0))
}

fn hand_names(state: &GameState, player: PlayerId) -> Vec<String> {
    state.players[player.0 as usize]
        .hand
        .iter()
        .filter_map(|id| state.objects.get(id).map(|obj| obj.name.clone()))
        .collect()
}

/// Declining to discard must offer the Lesson search — not silently no-op.
/// Choosing the Lesson card must put it into hand from the sideboard.
#[test]
fn eyetwitch_learn_decline_discard_reveals_lesson_into_hand() {
    let mut state = state_with_sideboard(vec![
        DeckEntry {
            card: face(
                "Introduction to Annihilation",
                CoreType::Sorcery,
                &["Lesson"],
            ),
            count: 1,
        },
        DeckEntry {
            card: face("Lightning Bolt", CoreType::Instant, &[]),
            count: 1,
        },
    ]);
    // A card in hand so the "may discard" choice is genuinely offered.
    create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Hand,
    );

    let ability = eyetwitch_learn_ability(&mut state);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    let player = match &state.waiting_for {
        WaitingFor::LearnChoice { player, .. } => *player,
        other => panic!("expected LearnChoice, got {other:?}"),
    };

    apply(
        &mut state,
        player,
        GameAction::LearnDecision {
            choice: LearnOption::Skip,
        },
    )
    .expect("declining to discard should be a legal Learn decision");

    // The non-Lesson sideboard card must not be offered — only the Lesson.
    match &state.waiting_for {
        WaitingFor::OutsideGameChoice { choices, .. } => {
            assert_eq!(
                choices.len(),
                1,
                "only the Lesson card should be offered, got {choices:?}"
            );
            assert_eq!(choices[0].name, "Introduction to Annihilation");
        }
        other => panic!("declining to discard must offer the Lesson search, got {other:?}"),
    }

    apply(
        &mut state,
        player,
        GameAction::ChooseOutsideGameCards {
            selections: vec![OutsideGameSelection::Sideboard { sideboard_index: 0 }],
        },
    )
    .expect("choosing the offered Lesson card should succeed");

    assert_eq!(
        hand_names(&state, player),
        vec!["Forest", "Introduction to Annihilation"],
        "the chosen Lesson card must join the undiscarded hand card"
    );
    assert_eq!(
        state.deck_pools[0].current_sideboard[0].count, 1,
        "the sideboard entry itself is unchanged (only usage is tracked)"
    );
}

/// An empty hand means the player *can't* discard, so CR 701.48a's "if you
/// didn't discard a card" branch must apply unconditionally rather than
/// silently skipping the ability.
#[test]
fn eyetwitch_learn_with_empty_hand_offers_lesson_directly() {
    let mut state = state_with_sideboard(vec![DeckEntry {
        card: face(
            "Introduction to Annihilation",
            CoreType::Sorcery,
            &["Lesson"],
        ),
        count: 1,
    }]);

    let ability = eyetwitch_learn_ability(&mut state);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    match &state.waiting_for {
        WaitingFor::OutsideGameChoice { choices, .. } => {
            assert_eq!(choices.len(), 1);
            assert_eq!(choices[0].name, "Introduction to Annihilation");
        }
        other => panic!("an empty hand must go straight to the Lesson search, got {other:?}"),
    }

    apply(
        &mut state,
        PlayerId(0),
        GameAction::ChooseOutsideGameCards {
            selections: vec![OutsideGameSelection::Sideboard { sideboard_index: 0 }],
        },
    )
    .expect("choosing the offered Lesson card should succeed");

    assert_eq!(
        hand_names(&state, PlayerId(0)),
        vec!["Introduction to Annihilation"]
    );
}

/// Rummage (discard→draw) must remain unaffected by the Lesson-search fix —
/// no Lesson prompt should appear when the player actually discarded.
#[test]
fn eyetwitch_learn_rummage_does_not_offer_lesson() {
    let mut state = state_with_sideboard(vec![DeckEntry {
        card: face(
            "Introduction to Annihilation",
            CoreType::Sorcery,
            &["Lesson"],
        ),
        count: 1,
    }]);
    let discard_target = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Hand,
    );
    let library_card = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Island".to_string(),
        Zone::Library,
    );
    state.players[0].library.push_back(library_card);

    let ability = eyetwitch_learn_ability(&mut state);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    let player = match &state.waiting_for {
        WaitingFor::LearnChoice { player, .. } => *player,
        other => panic!("expected LearnChoice, got {other:?}"),
    };

    apply(
        &mut state,
        player,
        GameAction::LearnDecision {
            choice: LearnOption::Rummage {
                card_id: discard_target,
            },
        },
    )
    .expect("rummaging should be a legal Learn decision");

    assert!(
        !matches!(state.waiting_for, WaitingFor::OutsideGameChoice { .. }),
        "having discarded a card must not offer the Lesson search"
    );
    assert_eq!(
        hand_names(&state, player),
        vec!["Island"],
        "rummage draws the seeded library card after discarding"
    );
}

/// Declining to discard with no Lesson card anywhere in the sideboard must
/// still settle the ability (priority returns) instead of leaving Learn
/// stuck waiting on an offer that will never be made.
#[test]
fn eyetwitch_learn_decline_with_no_lesson_in_sideboard_settles_priority() {
    let mut state = state_with_sideboard(vec![DeckEntry {
        card: face("Lightning Bolt", CoreType::Instant, &[]),
        count: 1,
    }]);
    create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Hand,
    );

    let ability = eyetwitch_learn_ability(&mut state);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    let player = match &state.waiting_for {
        WaitingFor::LearnChoice { player, .. } => *player,
        other => panic!("expected LearnChoice, got {other:?}"),
    };

    apply(
        &mut state,
        player,
        GameAction::LearnDecision {
            choice: LearnOption::Skip,
        },
    )
    .expect("declining to discard should be a legal Learn decision");

    assert!(
        !matches!(state.waiting_for, WaitingFor::OutsideGameChoice { .. }),
        "no Lesson card is available to offer"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "resolution must settle back to priority instead of stalling, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        hand_names(&state, player),
        vec!["Forest"],
        "the undiscarded card stays in hand and nothing is added"
    );
}
