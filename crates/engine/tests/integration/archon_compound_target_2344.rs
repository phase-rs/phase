//! Runtime regression for #2344 — Archon of Cruelty's compound player target.
//!
//! "target opponent sacrifices a creature or planeswalker of their choice,
//! discards a card, and loses 3 life" has a SINGLE instance of the word
//! "target" (CR 601.2c): the opponent is chosen once at announcement and every
//! conjugated verb applies to that same player. The bug surfaced THREE target
//! slots (one per verb), so the player was prompted to pick the opponent three
//! times. The parser fix makes the continuations inherit the announced target
//! via `TargetFilter::ParentTarget`.
//!
//! This drives the ability end-to-end and asserts the runtime symptom is gone:
//! exactly one target slot, and all three effects land on that one opponent.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const ARCHON_EFFECT: &str = "Target opponent sacrifices a creature or planeswalker \
     of their choice, discards a card, and loses 3 life. You draw a card and gain 3 life.";

fn hand_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .expect("player exists")
}

#[test]
fn compound_target_opponent_chosen_once_and_all_three_effects_apply() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 20);
    // The one opponent: a creature to sacrifice and a card to discard.
    scenario.add_vanilla(P1, 2, 2);
    scenario.with_cards_in_hand(P1, &["Discard Fodder"]);
    // P0 needs a library to satisfy the "you draw a card" rider.
    scenario.add_card_to_library_top(P0, "Draw Card");

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Archon Effect", false, ARCHON_EFFECT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;

    let p1_life_before = runner.life(P1);
    let p1_bf_before = runner.battlefield_count(P1);
    let p1_hand_before = hand_count(&runner, P1);

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the free sorcery must succeed");

    // #2344: exactly ONE target slot — the opponent is chosen a single time,
    // not once per verb. (Before the fix this was three slots.)
    match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { target_slots, .. } => {
            assert_eq!(
                target_slots.len(),
                1,
                "one 'target opponent' governs all three verbs — expected exactly one \
                 target slot, got {}",
                target_slots.len()
            );
            assert!(
                target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Player(P1)),
                "the opponent must be a legal target"
            );
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Player(P1)],
                })
                .expect("targeting the single opponent must succeed");
        }
        other => panic!("expected a single TargetSelection prompt, got {other:?}"),
    }

    // Resolve, answering only the chosen opponent's sacrifice/discard choices.
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 16,
            "too many prompts; stuck at {:?}",
            runner.state().waiting_for
        );
        match runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                assert_eq!(player, P1, "only the chosen opponent sacrifices");
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cards[0]],
                    })
                    .expect("sacrifice choice");
            }
            WaitingFor::DiscardChoice { player, cards, .. } => {
                assert_eq!(player, P1, "only the chosen opponent discards");
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cards[0]],
                    })
                    .expect("discard choice");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    runner.advance_until_stack_empty();

    // All three effects landed on the one opponent.
    assert_eq!(
        runner.battlefield_count(P1),
        p1_bf_before - 1,
        "opponent must sacrifice exactly one creature"
    );
    assert_eq!(
        hand_count(&runner, P1),
        p1_hand_before - 1,
        "opponent must discard exactly one card"
    );
    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "opponent must lose exactly 3 life"
    );
}
