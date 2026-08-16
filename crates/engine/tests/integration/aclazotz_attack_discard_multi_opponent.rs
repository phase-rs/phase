//! Issue #1974 — Aclazotz, Deepest Betrayal attack trigger must discard for
//! every opponent, not only the first.
//!
//! Oracle (attack trigger body):
//!   "Each opponent discards a card. For each opponent who can't, you draw a card."
//!
//! Same mandatory-impossible decline-tail AST as Refurbished Familiar (see
//! `refurbished_familiar.rs` and parser test
//! `for_each_opponent_who_cant_lowers_to_if_current_scope_succeeded`). The
//! multiplayer pause/resume bug (remaining opponents dropped when the first
//! opponent's conditional rider stashed `pending_continuation`) is fixed by
//! marking continuation clauses `SequentialSibling` (#2297).

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ATTACK_BODY: &str =
    "Each opponent discards a card. For each opponent who can't, you draw a card.";

fn aclazotz_attack_discard_ability(controller: PlayerId, source_id: ObjectId) -> ResolvedAbility {
    let def = parse_effect_chain(ATTACK_BODY, AbilityKind::Spell);
    build_resolved_from_def(&def, source_id, controller)
}

fn add_hand_cards(state: &mut GameState, base_card_id: u64, player: PlayerId, n: usize) {
    for i in 0..n {
        create_object(
            state,
            CardId(base_card_id + i as u64),
            player,
            "Forest".to_string(),
            Zone::Hand,
        );
    }
}

fn hand_len(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

fn first_hand_object(state: &GameState, player: PlayerId) -> ObjectId {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .front()
        .copied()
        .expect("hand not empty")
}

/// Four-player game: three opponents each hold two cards. Every opponent must
/// make an interactive `DiscardChoice` (count 1 < hand size 2). Without
/// `SequentialSibling` on the stashed remaining-opponent chain (#2297), only
/// the first opponent would discard before resolution returned to priority.
#[test]
fn issue_1974_attack_discard_reaches_all_opponents_after_pause() {
    let mut state = GameState::new(FormatConfig::standard(), 4, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Aclazotz, Deepest Betrayal".to_string(),
        Zone::Battlefield,
    );

    add_hand_cards(&mut state, 100, PlayerId(1), 2);
    add_hand_cards(&mut state, 200, PlayerId(2), 2);
    add_hand_cards(&mut state, 300, PlayerId(3), 2);

    let ability = aclazotz_attack_discard_ability(PlayerId(0), source);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    let mut guard = 0;
    while let WaitingFor::DiscardChoice { player, .. } = state.waiting_for.clone() {
        guard += 1;
        assert!(
            guard <= 4,
            "expected at most three discard prompts, stuck at {:?}",
            state.waiting_for
        );
        let pick = first_hand_object(&state, player);
        apply(
            &mut state,
            player,
            GameAction::SelectCards { cards: vec![pick] },
        )
        .expect("discard choice should succeed");
    }

    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "all opponents finished discarding, got {:?}",
        state.waiting_for
    );
    for opp in [PlayerId(1), PlayerId(2), PlayerId(3)] {
        assert_eq!(
            hand_len(&state, opp),
            1,
            "opponent {opp:?} must discard exactly one of two cards"
        );
    }
}
