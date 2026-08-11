//! Regression for GitHub issue #431 — Gemstone Caverns' opening-hand ability
//! silently dropped part of its text.
//!
//! Oracle text:
//!   "If this card is in your opening hand and you're not the starting player,
//!    you may begin the game with Gemstone Caverns on the battlefield with a
//!    luck counter on it. If you do, exile a card from your hand."
//!
//! The exported card data must retain the full `BeginGame` ability: Gemstone
//! Caverns enters with its luck counter, then its `IfYouDo` rider exiles a card
//! from its controller's hand.
//!
//! These tests drive the real begin-game / mulligan flow through `apply`:
//!   - accept the opt-in: Gemstone Caverns enters with a luck counter and an
//!     exile prompt is surfaced.
//!   - decline the opt-in: no exile prompt is surfaced (the `IfYouDo` gate
//!     evaluates false).
//!
//! No synthetic events — every step goes through `apply` / the public
//! game-start entry point.

use engine::database::card_db::CardDatabase;
use engine::game::deck_loading::create_object_from_card_face;
use engine::game::mana_abilities::is_mana_ability;
use engine::game::{apply, start_game_with_starting_player};
use engine::types::ability::AbilityKind;
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::counter::CounterType;
use engine::types::game_state::{
    GameState, ManaChoice, ManaChoiceContext, ManaChoicePrompt, WaitingFor,
};
use engine::types::mana::ManaType;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::collections::HashSet;

use crate::support::shared_card_db as load_db;

/// Build a 2-player game where the non-starting player (P1) has a 7-card
/// library consisting of Gemstone Caverns plus six basic lands. After the
/// opening-hand draw the entire library becomes P1's opening hand regardless of
/// shuffle order, so Gemstone Caverns is guaranteed to be in the opening hand.
///
/// Returns the state with the game started, the mulligan flow active, and the
/// Gemstone mana ability's exported definition index.
fn setup_game_with_gemstone_owner(
    db: &CardDatabase,
    gemstone_owner: PlayerId,
) -> (GameState, usize) {
    let mut state = GameState::new_two_player(42);

    let gemstone = db
        .get_face_by_name("Gemstone Caverns")
        .expect("Gemstone Caverns must be in the card database");
    let forest = db
        .get_face_by_name("Forest")
        .expect("Forest must be in the card database");
    let gemstone_mana_ability_index = gemstone
        .abilities
        .iter()
        .position(|ability| ability.kind == AbilityKind::Activated && is_mana_ability(ability))
        .expect("exported Gemstone Caverns must include an activated mana ability");

    for player in [PlayerId(0), PlayerId(1)] {
        if player == gemstone_owner {
            let gemstone_id = create_object_from_card_face(&mut state, gemstone, player);
            assert_eq!(
                state.objects[&gemstone_id].abilities[gemstone_mana_ability_index],
                gemstone.abilities[gemstone_mana_ability_index],
                "the runtime Gemstone Caverns object must retain its exported mana ability"
            );
            for _ in 0..6 {
                create_object_from_card_face(&mut state, forest, player);
            }
        } else {
            for _ in 0..7 {
                create_object_from_card_face(&mut state, forest, player);
            }
        }
    }

    // P0 starts → P1 is the non-starting player, matching Gemstone Caverns'
    // flavor condition.
    let result = start_game_with_starting_player(&mut state, PlayerId(0));
    state.waiting_for = result.waiting_for;
    (state, gemstone_mana_ability_index)
}

fn setup_game(db: &CardDatabase) -> (GameState, usize) {
    setup_game_with_gemstone_owner(db, PlayerId(1))
}

/// Drive both players to `Keep` through `apply`, leaving the game at the
/// begin-game opt-in prompt for Gemstone Caverns.
fn keep_both_hands(state: &mut GameState) {
    // Both players keep their opening hands. Drive the actual pending player so
    // the helper remains correct when a begin-game ability belongs to either
    // seat.
    while let WaitingFor::MulliganDecision { pending, .. } = &state.waiting_for {
        let Some(entry) = pending.first() else {
            break;
        };
        let result = apply(
            state,
            entry.player,
            GameAction::MulliganDecision {
                choice: MulliganChoice::Keep,
            },
        )
        .expect("Keep decision must succeed");
        state.waiting_for = result.waiting_for;
    }
}

/// Locate Gemstone Caverns in P1's hand.
fn gemstone_in_hand(state: &GameState) -> engine::types::identifiers::ObjectId {
    gemstone_in_player_hand(state, PlayerId(1))
}

fn gemstone_in_player_hand(
    state: &GameState,
    player: PlayerId,
) -> engine::types::identifiers::ObjectId {
    *state.players[player.0 as usize]
        .hand
        .iter()
        .find(|id| state.objects[id].name == "Gemstone Caverns")
        .expect("Gemstone Caverns must be in the player's opening hand")
}

#[test]
fn gemstone_caverns_accept_enters_with_luck_counter_and_prompts_exile() {
    let Some(db) = load_db() else {
        return;
    };
    let (mut state, gemstone_mana_ability_index) = setup_game(db);
    keep_both_hands(&mut state);

    let gemstone_id = gemstone_in_hand(&state);
    let expected_exile_candidates = state.players[1]
        .hand
        .iter()
        .copied()
        .filter(|id| *id != gemstone_id)
        .collect::<Vec<_>>();

    // The begin-game opt-in for Gemstone Caverns must be surfaced to P1.
    let WaitingFor::OptionalEffectChoice { player, .. } = &state.waiting_for else {
        panic!(
            "expected begin-game OptionalEffectChoice prompt, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(*player, PlayerId(1), "the prompt must be for P1");

    // Accept the begin-game opt-in.
    let result = apply(
        &mut state,
        PlayerId(1),
        GameAction::DecideOptionalEffect { accept: true },
    )
    .expect("accepting the begin-game opt-in must succeed");
    state.waiting_for = result.waiting_for;

    // CR 103.6a: Gemstone Caverns is now on the battlefield.
    assert_eq!(
        state.objects[&gemstone_id].zone,
        Zone::Battlefield,
        "Gemstone Caverns must enter the battlefield after accepting",
    );

    // CR 122.1: it entered with exactly one luck counter — without this the
    // {T} ability would only ever tap for {C}.
    let luck = CounterType::Generic("luck".to_string());
    assert_eq!(
        state.objects[&gemstone_id].counters.get(&luck).copied(),
        Some(1),
        "Gemstone Caverns must enter with one luck counter, got counters {:?}",
        state.objects[&gemstone_id].counters,
    );

    let WaitingFor::EffectZoneChoice {
        player,
        cards,
        count,
        zone,
        destination,
        ..
    } = &state.waiting_for
    else {
        panic!(
            "accepting must surface a hand-to-exile EffectZoneChoice, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(*player, PlayerId(1), "P1 must choose the exiled card");
    assert_eq!(*count, 1, "exactly one card must be exiled");
    assert_eq!(*zone, Zone::Hand, "the choice must come from P1's hand");
    assert_eq!(
        *destination,
        Some(Zone::Exile),
        "the selected card must be exiled"
    );
    assert_eq!(
        cards.iter().copied().collect::<HashSet<_>>(),
        expected_exile_candidates
            .into_iter()
            .collect::<HashSet<_>>(),
        "only P1's non-Gemstone opening-hand cards are legal exile candidates"
    );
    assert!(
        cards.iter().all(|id| state.players[1].hand.contains(id)),
        "every exile candidate must still be in P1's hand; candidates={cards:?}"
    );
    assert!(
        cards.iter().all(|id| !state.players[0].hand.contains(id)),
        "P0's cards must never be exile candidates; candidates={cards:?}"
    );

    let exiled_card = cards[0];
    let result = apply(
        &mut state,
        PlayerId(1),
        GameAction::SelectCards {
            cards: vec![exiled_card],
        },
    )
    .expect("selecting a legal P1 hand card to exile must succeed");
    state.waiting_for = result.waiting_for;

    assert_eq!(
        state.objects[&exiled_card].zone,
        Zone::Exile,
        "the selected card must leave P1's hand for exile"
    );
    assert!(
        !state.players[1].hand.contains(&exiled_card),
        "the selected card must no longer be in P1's hand"
    );
    assert_eq!(
        state.objects[&gemstone_id].zone,
        Zone::Battlefield,
        "Gemstone Caverns remains on the battlefield after the exile rider"
    );
    assert_eq!(
        state.objects[&gemstone_id].counters.get(&luck).copied(),
        Some(1),
        "Gemstone Caverns retains its luck counter after the exile rider"
    );
    assert!(
        matches!(&state.waiting_for, WaitingFor::Priority { player } if *player == PlayerId(0)),
        "begin-game resolution must drain to P0 priority, got {:?}",
        state.waiting_for
    );

    let result = apply(&mut state, PlayerId(0), GameAction::PassPriority)
        .expect("P0 must be able to pass priority to P1");
    state.waiting_for = result.waiting_for;
    assert!(
        matches!(&state.waiting_for, WaitingFor::Priority { player } if *player == PlayerId(1)),
        "P1 must receive priority to activate Gemstone Caverns, got {:?}",
        state.waiting_for
    );

    let runtime_mana_ability = &state.objects[&gemstone_id].abilities[gemstone_mana_ability_index];
    assert_eq!(runtime_mana_ability.kind, AbilityKind::Activated);
    assert!(
        is_mana_ability(runtime_mana_ability),
        "the exported Gemstone mana ability must be present on its runtime object"
    );
    let result = apply(
        &mut state,
        PlayerId(1),
        GameAction::ActivateAbility {
            source_id: gemstone_id,
            ability_index: gemstone_mana_ability_index,
        },
    )
    .expect("P1 must be able to activate Gemstone Caverns' exported mana ability");
    state.waiting_for = result.waiting_for;

    let WaitingFor::ChooseManaColor {
        player,
        choice: ManaChoicePrompt::SingleColor { options },
        context: ManaChoiceContext::ManaAbility(pending),
    } = &state.waiting_for
    else {
        panic!(
            "Gemstone Caverns must surface its mana-color choice, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(*player, PlayerId(1), "P1 chooses the mana color");
    assert_eq!(pending.player, PlayerId(1));
    assert_eq!(pending.source_id, gemstone_id);
    assert_eq!(pending.ability_index, Some(gemstone_mana_ability_index));
    assert_eq!(
        options,
        &vec![
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ],
        "a luck counter lets Gemstone Caverns produce one mana of any color"
    );

    let result = apply(
        &mut state,
        PlayerId(1),
        GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        },
    )
    .expect("P1 must be able to choose blue mana");
    state.waiting_for = result.waiting_for;

    let mana_pool = &state.players[1].mana_pool;
    assert_eq!(mana_pool.count_color(ManaType::Blue), 1);
    assert_eq!(mana_pool.count_color(ManaType::Colorless), 0);
    assert_eq!(mana_pool.total(), 1);
    assert!(state.objects[&gemstone_id].tapped);
}

#[test]
fn gemstone_caverns_decline_surfaces_no_exile_prompt() {
    let Some(db) = load_db() else {
        return;
    };
    let (mut state, _) = setup_game(db);
    keep_both_hands(&mut state);

    let gemstone_id = gemstone_in_hand(&state);
    let hand_size_before = state.players[1].hand.len();

    let WaitingFor::OptionalEffectChoice { player, .. } = &state.waiting_for else {
        panic!(
            "expected begin-game OptionalEffectChoice prompt, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(*player, PlayerId(1));

    // Decline the begin-game opt-in.
    let result = apply(
        &mut state,
        PlayerId(1),
        GameAction::DecideOptionalEffect { accept: false },
    )
    .expect("declining the begin-game opt-in must succeed");
    state.waiting_for = result.waiting_for;

    // Gemstone Caverns stays in hand — it was never put onto the battlefield.
    assert_eq!(
        state.objects[&gemstone_id].zone,
        Zone::Hand,
        "declining must leave Gemstone Caverns in hand",
    );

    // The `IfYouDo` gate evaluates false: no exile prompt is surfaced and the
    // game proceeds to Priority. The hand is intact — nothing was exiled.
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "declining must surface no exile prompt — the game proceeds to Priority: {:?}",
        state.waiting_for,
    );
    assert_eq!(
        state.players[1].hand.len(),
        hand_size_before,
        "declining must not exile any card from hand",
    );
}

#[test]
fn gemstone_caverns_starting_player_gets_no_begin_game_prompt() {
    let Some(db) = load_db() else {
        return;
    };
    let (mut state, _) = setup_game_with_gemstone_owner(db, PlayerId(0));
    keep_both_hands(&mut state);

    let gemstone_id = gemstone_in_player_hand(&state, PlayerId(0));

    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "starting player must not receive Gemstone Caverns begin-game prompt: {:?}",
        state.waiting_for,
    );
    assert_eq!(
        state.objects[&gemstone_id].zone,
        Zone::Hand,
        "Gemstone Caverns must stay in the starting player's hand",
    );
}
