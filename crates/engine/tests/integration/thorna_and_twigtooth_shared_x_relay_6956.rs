//! Regression for the #6956 fix round: a co-anchored "where X is …" clause
//! group must not have its shared X redefined by a middle clause that produced
//! zero.
//!
//! Oracle (Thorna and Twigtooth, verbatim from card data):
//!   "Thorna and Twigtooth enters with two -1/-1 counters on it.
//!    Whenever Thorna and Twigtooth attacks, remove all counters from target
//!    creature you control. Each opponent loses X life, you gain X life, and the
//!    topmost creature card in your library perpetually gets +X/+X, where X is
//!    the number of counters removed this way."
//!
//! The trigger lowers to a chain-relative
//! `RemoveCounter -> LoseLife{PreviousEffectAmount} -> GainLife{PreviousEffectAmount}`
//! (the perpetual +X/+X clause does not parse at all today), so every clause
//! reads whatever the immediately preceding step left in `last_effect_amount`
//! rather than the anchored X.
//!
//! #6956's first pass made a genuine zero overwrite that slot. That is right for
//! a fresh producer, but the middle `LoseLife` here is a RELAY — its own amount
//! IS the shared X. With an opponent under CR 119.8 "can't lose life" the relay
//! totals zero, and claiming that zero made the gain clause read 0 instead of
//! the counters removed. The card was accidentally correct before #6956 (via
//! exactly the leak #6956 closed) and would have been silently wrong after.
//!
//! CR references:
//!   - CR 608.2c: the controller follows the instructions in the order written;
//!     "…X…, …X…, and …X…, where X is <definition>" names ONE value that every
//!     clause reads.
//!   - CR 119.8: "If an effect says that a player can't lose life, …" — Platinum
//!     Emperion's "Your life total can't change" blocks the middle clause.
//!   - CR 122.1: a counter is a marker placed on an object; X is the number of
//!     counters actually removed.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const THORNA_ORACLE: &str = "Thorna and Twigtooth enters with two -1/-1 counters on it.\n\
Whenever Thorna and Twigtooth attacks, remove all counters from target creature you control. \
Each opponent loses X life, you gain X life, and the topmost creature card in your library \
perpetually gets +X/+X, where X is the number of counters removed this way.";

const PLATINUM_EMPERION_ORACLE: &str = "Your life total can't change.";

/// Drive the attack trigger to resolution, answering trigger ordering and the
/// "target creature you control" selection with `target`.
fn resolve_attack_trigger_targeting(runner: &mut GameRunner, target: ObjectId) {
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let count = triggers.len();
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..count).collect(),
                    })
                    .expect("order triggers");
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![engine::types::ability::TargetRef::Object(target)],
                    })
                    .expect("select the trigger's target creature");
            }
            other => panic!("unexpected waiting state during the attack trigger: {other:?}"),
        }
    }
    panic!("the attack trigger never resolved");
}

/// P0 attacks with Thorna; the trigger removes two -1/-1 counters from it. P1
/// controls Platinum Emperion, so the middle "each opponent loses X life" clause
/// produces a ZERO total.
///
/// Discriminating assertion: P0 must still gain **2** life — the anchored X, the
/// number of counters removed. Reading the relay's own zero yields 0, which is
/// exactly what the un-guarded #6956 change produced.
#[test]
fn thorna_gain_clause_reads_the_anchored_x_when_the_life_loss_clause_totals_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let thorna = scenario
        .add_creature_from_oracle(P0, "Thorna and Twigtooth", 4, 4, THORNA_ORACLE)
        .id();
    // CR 119.8: P1 cannot lose life, so the middle clause totals zero.
    let _emperion = scenario
        .add_creature_from_oracle(P1, "Platinum Emperion", 8, 8, PLATINUM_EMPERION_ORACLE)
        .id();

    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&thorna)
        .expect("Thorna is on the battlefield")
        .counters
        .insert(CounterType::Minus1Minus1, 2);

    let life_before = runner.state().players[P0.0 as usize].life;
    let opponent_life_before = runner.state().players[P1.0 as usize].life;

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(thorna, AttackTarget::Player(P1))])
        .expect("DeclareAttackers must succeed");
    resolve_attack_trigger_targeting(&mut runner, thorna);

    // Reach guard: the removal actually happened, so X really is 2. Without this
    // the life assertion below could pass on a chain that never ran.
    assert_eq!(
        runner.state().objects.get(&thorna).map(|o| o
            .counters
            .get(&CounterType::Minus1Minus1)
            .copied()
            .unwrap_or(0)),
        Some(0),
        "reach guard: the trigger must have removed both -1/-1 counters (X = 2)"
    );
    // Reach guard: the relay clause really did total ZERO — this is the branch
    // under test, not the ordinary nonzero path.
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        opponent_life_before,
        "reach guard: CR 119.8 must have blocked the opponent's life loss entirely"
    );

    assert_eq!(
        runner.state().players[P0.0 as usize].life - life_before,
        2,
        "the gain clause must read the anchored X (2 counters removed), not the \
         relay life-loss clause's own zero"
    );
}

/// Reach guard for the test above: the SAME card with no "can't lose life"
/// permanent must still work, so the zero-branch assertion is not vacuous and
/// the guard has not disabled the ordinary path.
#[test]
fn thorna_gain_clause_still_matches_the_life_loss_on_the_ordinary_path() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let thorna = scenario
        .add_creature_from_oracle(P0, "Thorna and Twigtooth", 4, 4, THORNA_ORACLE)
        .id();

    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&thorna)
        .expect("Thorna is on the battlefield")
        .counters
        .insert(CounterType::Minus1Minus1, 2);

    let life_before = runner.state().players[P0.0 as usize].life;
    let opponent_life_before = runner.state().players[P1.0 as usize].life;

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(thorna, AttackTarget::Player(P1))])
        .expect("DeclareAttackers must succeed");
    resolve_attack_trigger_targeting(&mut runner, thorna);

    assert_eq!(
        opponent_life_before - runner.state().players[P1.0 as usize].life,
        2,
        "the sole opponent must lose X = 2 life"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life - life_before,
        2,
        "and the controller gains the same X"
    );
}
