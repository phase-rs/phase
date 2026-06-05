//! Regression for GitHub issue #455 — Ponder.
//!
//! Ponder reads: "Look at the top three cards of your library, then put them
//! back in any order. You may shuffle.\nDraw a card." The "You may shuffle."
//! clause is optional; "Draw a card." is a separate, mandatory sentence.
//!
//! CR 608.2c: instructions resolve "in the order written" — declining the
//! optional shuffle must NOT skip the mandatory draw. The parser stamps the
//! `Draw` sub-ability with `SubAbilityLink::SequentialSibling` (it follows a
//! sentence boundary), and the optional-decline resolver force-resolves a
//! `SequentialSibling` sub regardless of the optional decision.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Cast Ponder, answer its `DigChoice`, and stop at the optional shuffle prompt.
/// Returns the runner parked on `WaitingFor::OptionalEffectChoice`.
fn cast_ponder_to_shuffle_prompt(db: &'static CardDatabase) -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ponder = scenario.add_real_card(P0, "Ponder", Zone::Hand, db);
    // Four distinct named cards on the library so a draw is unambiguous.
    for name in ["Forest", "Island", "Mountain", "Plains"] {
        scenario.add_real_card(P0, name, Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    // Ponder costs {U}.
    runner.state_mut().players[0]
        .mana_pool
        .add(engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Blue,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ));

    let card_id = runner.state().objects[&ponder].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: ponder,
            card_id,
            targets: vec![],
        })
        .expect("Ponder cast should succeed");
    runner.advance_until_stack_empty();

    // Ponder's Dig opens a DigChoice — keep all three cards on top in
    // whatever order they were presented.
    let kept = match &runner.state().waiting_for {
        WaitingFor::DigChoice { cards, .. } => cards.clone(),
        other => panic!("expected DigChoice after Ponder resolves, got {other:?}"),
    };
    runner
        .act(GameAction::SelectCards { cards: kept })
        .expect("answering Ponder's DigChoice should advance to the shuffle prompt");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "Ponder must prompt for the optional shuffle, got {:?}",
        runner.state().waiting_for,
    );
    runner
}

#[test]
fn ponder_decline_shuffle_still_draws() {
    let Some(db) = load_db() else {
        return;
    };
    let mut runner = cast_ponder_to_shuffle_prompt(db);

    let result = runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining Ponder's optional shuffle should resolve");

    // CR 608.2c: the mandatory "Draw a card." sentence still resolves. Ponder
    // left the hand on cast, so the post-resolution hand holds exactly the one
    // drawn card.
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        1,
        "declining the shuffle must NOT skip the mandatory draw",
    );
    assert!(
        result.events.iter().any(|e| matches!(
            e,
            GameEvent::CardDrawn { .. } | GameEvent::CardsDrawn { .. }
        )),
        "a draw event must be emitted on the decline path",
    );
    // The optional shuffle was declined — no Shuffle effect resolved.
    assert!(
        !result.events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Shuffle,
                ..
            }
        )),
        "declining the optional shuffle must not shuffle the library",
    );
}

#[test]
fn ponder_accept_shuffle_draws_exactly_once() {
    let Some(db) = load_db() else {
        return;
    };
    let mut runner = cast_ponder_to_shuffle_prompt(db);

    let result = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting Ponder's optional shuffle should resolve");

    // Exactly one card drawn — the SequentialSibling draw must not double up
    // with the accept path's own continuation.
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        1,
        "accepting the shuffle must still draw exactly one card",
    );
    let draw_count: usize = result
        .events
        .iter()
        .map(|e| match e {
            GameEvent::CardDrawn { .. } => 1,
            GameEvent::CardsDrawn { count, .. } => *count as usize,
            _ => 0,
        })
        .sum();
    assert_eq!(draw_count, 1, "exactly one draw event expected");
    // Accepting performs the shuffle.
    assert!(
        result.events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Shuffle,
                ..
            }
        )),
        "accepting the optional shuffle must shuffle the library",
    );
}
