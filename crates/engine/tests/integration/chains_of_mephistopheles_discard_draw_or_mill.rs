//! Runtime pipeline regression for issue #5653 — Chains of Mephistopheles /
//! Magus of the Chains.
//!
//! Oracle text: "If a player would draw a card except the first one they draw
//! in each of their draw steps, that player discards a card instead. If the
//! player discards a card this way, they draw a card. If the player doesn't
//! discard a card this way, they mill a card."
//!
//! The draw and mill branches are mutually exclusive on whether the discard
//! actually removed a card: a nonempty hand discards then draws (no mill); an
//! empty hand fails to discard, so the player mills instead of drawing (no
//! draw). Each scenario stocks the library with TWO cards so an unconditional
//! chain (both Draw and Mill firing regardless of the discard's outcome) is
//! distinguishable from the correct either/or behavior — with only one library
//! card, an unconditional Draw would exhaust the library before Mill could act,
//! making the bug invisible.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn card_names(
    state: &engine::types::game_state::GameState,
    ids: impl Iterator<Item = engine::types::identifiers::ObjectId>,
) -> Vec<String> {
    ids.filter_map(|id| state.objects.get(&id).map(|o| o.name.clone()))
        .collect()
}

/// CR 121.1 + CR 614.1a: with a card in hand, the replaced draw discards it,
/// then — because the discard succeeded — the player draws a replacement
/// card, and the mill branch must NOT also run. The library carries a second
/// card so an unconditional chain (Draw AND Mill both firing) would leave the
/// library empty and put a second card in the graveyard; only the correct
/// either/or behavior leaves "Sol Ring" untouched in the library.
#[test]
fn chains_nonempty_hand_discards_then_draws_no_mill() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Chains of Mephistopheles", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    // Library front-to-back: [Hill Giant, Sol Ring]. A correct single draw
    // takes only Hill Giant, leaving Sol Ring in the library.
    scenario.add_real_card(P0, "Hill Giant", Zone::Library, db);
    scenario.add_real_card(P0, "Sol Ring", Zone::Library, db);
    // P1 needs *some* library so SBAs don't fire.
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;

    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("debug draw must succeed");
    runner.advance_until_stack_empty();

    let hand_names = card_names(
        runner.state(),
        runner.state().players[0].hand.iter().copied(),
    );
    assert_eq!(
        hand_names,
        vec!["Hill Giant".to_string()],
        "the discard-then-draw branch must swap Grizzly Bears for the drawn \
         Hill Giant (net hand size unchanged); got {hand_names:?}"
    );

    let graveyard_names = card_names(
        runner.state(),
        runner.state().players[0].graveyard.iter().copied(),
    );
    assert_eq!(
        graveyard_names,
        vec!["Grizzly Bears".to_string()],
        "exactly the discarded card must be in the graveyard — a second card \
         here would mean the mill branch ran alongside the draw branch \
         instead of being mutually exclusive with it; got {graveyard_names:?}"
    );

    let library_names = card_names(
        runner.state(),
        runner.state().players[0].library.iter().copied(),
    );
    assert_eq!(
        library_names,
        vec!["Sol Ring".to_string()],
        "only Hill Giant may leave the library (the single draw) — Sol Ring \
         must remain; a missing Sol Ring would mean mill also fired and \
         consumed it; got {library_names:?}"
    );
}

/// CR 121.1 + CR 614.1a: with an empty hand, the replaced draw has nothing to
/// discard, so the discard fails — and because it failed, the player mills a
/// card instead of drawing. The library carries a second card so an
/// unconditional chain (Draw AND Mill both firing) would draw one card into
/// hand AND mill the other; only the correct either/or behavior leaves the
/// hand empty and "Sol Ring" untouched in the library.
#[test]
fn chains_empty_hand_fails_discard_then_mills_no_draw() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Chains of Mephistopheles", Zone::Battlefield, db);
    // Library front-to-back: [Hill Giant, Sol Ring]. A correct mill takes only
    // Hill Giant (the top card), leaving Sol Ring in the library.
    scenario.add_real_card(P0, "Hill Giant", Zone::Library, db);
    scenario.add_real_card(P0, "Sol Ring", Zone::Library, db);
    // P1 needs *some* library so SBAs don't fire.
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;

    assert!(
        runner.state().players[0].hand.is_empty(),
        "test setup must start with an empty P0 hand"
    );

    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("debug draw must succeed");
    runner.advance_until_stack_empty();

    let hand_names = card_names(
        runner.state(),
        runner.state().players[0].hand.iter().copied(),
    );
    assert!(
        hand_names.is_empty(),
        "an empty hand cannot discard, so the draw branch must NOT fire \
         either — the hand must stay empty; got {hand_names:?}"
    );

    let graveyard_names = card_names(
        runner.state(),
        runner.state().players[0].graveyard.iter().copied(),
    );
    assert_eq!(
        graveyard_names,
        vec!["Hill Giant".to_string()],
        "the mill branch must move exactly the top library card to the \
         graveyard; got {graveyard_names:?}"
    );

    let library_names = card_names(
        runner.state(),
        runner.state().players[0].library.iter().copied(),
    );
    assert_eq!(
        library_names,
        vec!["Sol Ring".to_string()],
        "only Hill Giant may leave the library (the single mill) — Sol Ring \
         must remain; a missing Sol Ring would mean the draw branch also \
         fired and consumed it; got {library_names:?}"
    );
}
