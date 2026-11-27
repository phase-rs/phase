//! Visitor from Planet Q — "Whenever you cast another spell with two or more
//! card types, you may draw a card, then discard a card." The type-count
//! condition must gate the trigger (CR 205.2a + CR 603.2): a two-type spell
//! loots, a one-type spell does not prompt at all. Draw resolves before the
//! discard choice (Visitor ruling: no draw, no discard). The Instant half of
//! Visitor's own typeline is unstaged (no builder composes Instant +
//! Creature) — no assertion reads Visitor's own types.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

/// Verbatim Oracle text of Visitor from Planet Q.
const VISITOR_ORACLE: &str = "All creature cards you own with flash are instants in addition to their other types.\nWhenever you cast another spell with two or more card types, you may draw a card, then discard a card.";
const ORNITHOPTER_ORACLE: &str = "Flying";
const DIVINATION_ORACLE: &str = "Draw two cards.";

/// Pass priority (answering trigger ordering) until the game stops asking for
/// it: an empty stack or any other prompt ends the drive.
fn settle_to_prompt_or_empty(runner: &mut GameRunner) {
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass accepted");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("trigger order accepted");
            }
            _ => return,
        }
    }
    panic!("priority did not settle");
}

/// Drive to an empty stack, failing on any player prompt (proves no trigger).
fn drive_quiet(runner: &mut GameRunner) {
    settle_to_prompt_or_empty(runner);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no prompt expected, got {:?}",
        runner.state().waiting_for
    );
}

/// A two-type spell loots: the loot prompt appears, the draw resolves first,
/// then the discard choice is driven explicitly to completion.
#[test]
fn visitor_loots_for_two_type_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island", "Island", "Island"]);
    // Two discard-fodder cards: the discard must be a real choice (CR 701.9b),
    // never a singleton the engine could auto-answer.
    scenario.with_cards_in_hand(P0, &["Grizzly Bears", "Forest"]);
    let visitor = scenario
        .add_creature_to_hand(P0, "Visitor from Planet Q", 0, 4)
        .with_subtypes(vec!["Alien"])
        .from_oracle_text(VISITOR_ORACLE)
        .id();
    let thopter = scenario
        .add_creature_to_hand(P0, "Ornithopter", 0, 2)
        .as_artifact_creature()
        .with_subtypes(vec!["Thopter"])
        .from_oracle_text_with_keywords(&["Flying"], ORNITHOPTER_ORACLE)
        .id();
    let mut runner = scenario.build();
    assert_eq!(runner.state().players[0].library.len(), 3);
    runner.cast(visitor).commit();
    drive_quiet(&mut runner);
    assert!(runner.state().battlefield.contains(&visitor));

    runner.cast(thopter).commit();
    settle_to_prompt_or_empty(&mut runner);
    match &runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(*player, P0, "Visitor's controller loots");
        }
        other => panic!("expected the loot prompt, got {other:?}"),
    }
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the loot");
    settle_to_prompt_or_empty(&mut runner);
    // Draw first: three cards in hand before the discard choice.
    assert_eq!(runner.state().players[0].hand.len(), 3);
    let drawn = *runner.state().players[0]
        .hand
        .iter()
        .find(|id| runner.state().objects[id].name == "Island")
        .expect("the drawn Island is in hand");
    match &runner.state().waiting_for {
        WaitingFor::DiscardChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "Visitor's controller discards");
            assert!(cards.contains(&drawn));
        }
        other => panic!("expected a DiscardChoice prompt, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards { cards: vec![drawn] })
        .expect("discard the drawn card");
    settle_to_prompt_or_empty(&mut runner);
    let state = runner.state();
    assert!(state.stack.is_empty(), "the trigger resolved");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "priority returns after the loot"
    );
    assert_eq!(state.players[0].hand.len(), 2);
    assert!(state.players[0].graveyard.contains(&drawn));
    assert!(state.battlefield.contains(&thopter));
}

/// A one-type spell resolves with no loot prompt at all (at base this
/// over-triggers — the negative is the discriminator). Divination is a
/// non-flash non-creature, so Visitor's own static could not affect it even
/// if it applied on the stack.
#[test]
fn visitor_silent_for_single_type_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island", "Island", "Island"]);
    let visitor = scenario
        .add_creature_to_hand(P0, "Visitor from Planet Q", 0, 4)
        .with_subtypes(vec!["Alien"])
        .from_oracle_text(VISITOR_ORACLE)
        .id();
    let divination = scenario
        .add_spell_to_hand(P0, "Divination", false)
        .from_oracle_text(DIVINATION_ORACLE)
        .id();
    let mut runner = scenario.build();
    runner.cast(visitor).commit();
    drive_quiet(&mut runner);
    assert!(runner.state().battlefield.contains(&visitor));

    runner.cast(divination).commit();
    drive_quiet(&mut runner);
    // Reach-guard: Divination resolved (cast one, drew two).
    let state = runner.state();
    assert!(state.stack.is_empty());
    assert_eq!(state.players[0].hand.len(), 2);
    assert!(state.players[0].graveyard.contains(&divination));
}
