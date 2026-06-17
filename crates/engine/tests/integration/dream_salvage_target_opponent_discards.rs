//! Runtime pipeline regression — Dream Salvage (Cluster C, player-axis count).
//!
//! Oracle: "Draw cards equal to the number of cards target opponent discarded
//! this turn."
//!
//! The count references a TARGET opponent's discard history. The fix adds the
//! parser arm mapping "target opponent discarded this turn" →
//! `CardsDiscardedThisTurn { player: Target }` and surfaces a count-derived
//! Opponent-scoped target slot so the per-player discard count resolves against
//! the chosen opponent (not summed across all opponents).
//!
//! DISCRIMINATING: with the fix reverted the phrase fails to parse to a Target
//! scope (no count slot) → zero cards drawn. A second opponent who discarded a
//! DIFFERENT number proves the count is per-targeted-player, not an
//! all-opponents sum (3, not 3+5=8).

use engine::game::scenario::GameScenario;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const SALVAGE_TEXT: &str =
    "Draw cards equal to the number of cards target opponent discarded this turn.";

#[test]
fn dream_salvage_draws_target_opponents_discards_not_all_opponents() {
    let p0 = PlayerId(0);
    let p1 = PlayerId(1);
    let p2 = PlayerId(2);

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(p0, "Dream Salvage", true, SALVAGE_TEXT)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    for name in ["Plains", "Island", "Swamp", "Mountain", "Forest"] {
        scenario.add_card_to_library_top(p0, name);
        scenario.add_card_to_library_top(p1, name);
        scenario.add_card_to_library_top(p2, name);
    }

    scenario.with_mana_pool(
        p0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, spell, false, Vec::new()))
            .collect(),
    );

    let mut runner = scenario.build();
    // Seed per-player discard history: target opponent P1 discarded 3, the other
    // opponent P2 discarded 5. The targeted-opponent count must be exactly 3.
    runner
        .state_mut()
        .cards_discarded_this_turn_by_player
        .insert(p1, 3);
    runner
        .state_mut()
        .cards_discarded_this_turn_by_player
        .insert(p2, 5);

    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin Dream Salvage cast");

    let mut targeted = false;
    let mut committed = false;
    let mut hand_after_commit = 0usize;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { target_slots, .. } => {
                assert_eq!(
                    target_slots.len(),
                    1,
                    "exactly one count-derived opponent slot must be offered; got {target_slots:?}",
                );
                let legal = &target_slots[0].legal_targets;
                assert!(
                    legal.contains(&TargetRef::Player(p1))
                        && legal.contains(&TargetRef::Player(p2)),
                    "both opponents must be legal targets; got {legal:?}",
                );
                assert!(
                    !legal.contains(&TargetRef::Player(p0)),
                    "the caster must NOT be a legal target (opponent-scoped slot)",
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(p1)],
                    })
                    .expect("targeting opponent P1 must succeed");
                targeted = true;
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay mana from pool");
            }
            WaitingFor::Priority { .. } => {
                hand_after_commit = runner.state().players[0].hand.len();
                committed = true;
                break;
            }
            other => panic!("unexpected waiting state during Dream Salvage cast: {other:?}"),
        }
    }
    assert!(targeted, "the count-derived opponent slot must be selected");
    assert!(
        committed,
        "the spell must reach a post-commit priority window"
    );

    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve Dream Salvage");
    }

    // The spell card itself left the hand for the stack; library cards are drawn
    // back in. Measure delta from the post-commit baseline.
    let drawn = runner.state().players[0].hand.len() as i64 - hand_after_commit as i64;
    assert_eq!(
        drawn, 3,
        "Dream Salvage must draw exactly the targeted opponent's discards (3), \
         not the all-opponents sum (8)",
    );
}
