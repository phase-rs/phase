//! Runtime pipeline regression — Morbid Curiosity.
//!
//! Oracle: "As an additional cost to cast this spell, sacrifice an artifact or
//! creature. Draw cards equal to the mana value of the sacrificed permanent."
//!
//! The fix added the PREPOSITIONAL cost-paid mana-value parser front-form
//! ("the mana value of the sacrificed permanent") alongside the pre-existing
//! possessive form. Before the fix the draw clause fell to `Unimplemented` and
//! nothing was drawn. The cost-wiring (sacrifice → `cost_paid_object`) and the
//! resolution (`QuantityRef::ObjectManaValue { CostPaidObject }`, CR 202.3 +
//! CR 608.2k) already existed; this test drives the whole pipeline:
//! cast → pay the sacrifice cost (choosing the mana-value-3 artifact over a
//! mana-value-5 decoy) → resolve → assert exactly 3 cards drawn.
//!
//! DISCRIMINATING: the assertion is "exactly 3 drawn equal to the MV of the
//! sacrificed permanent". If the parser fix is reverted the draw clause is an
//! `Unimplemented` no-op and zero cards are drawn — the test fails. The MV-5
//! decoy proves the count tracks the *sacrificed* object, not just any
//! permanent.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

#[test]
fn morbid_curiosity_draws_equal_to_sacrificed_permanents_mana_value() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database unavailable");
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario.add_real_card(P0, "Morbid Curiosity", Zone::Hand, db);
    // The sacrifice target: a mana-value-3 artifact → expect exactly 3 drawn.
    let relic = scenario.add_real_card(P0, "Coalition Relic", Zone::Battlefield, db);
    // A different-mana-value (5) artifact decoy — must NOT be the one chosen,
    // proving the count tracks the sacrificed object rather than any permanent.
    let _decoy = scenario.add_real_card(P0, "Gilded Lotus", Zone::Battlefield, db);

    // Library cards to draw (need at least 3).
    for name in ["Plains", "Island", "Swamp", "Mountain", "Forest"] {
        scenario.add_real_card(P0, name, Zone::Library, db);
    }
    // P1 needs a library so SBAs don't end the game.
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }

    // Fund {1}{B}{B}: three black mana covers both pips and the generic.
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, spell, false, Vec::new()))
            .collect(),
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let card_id = runner.state().objects[&spell].card_id;
    let hand_before = runner.state().players[0].hand.len();

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin Morbid Curiosity cast");

    let mut paid_sacrifice = false;
    let mut committed = false;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            // CR 601.2f: the mandatory sacrifice additional cost.
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards { cards: vec![relic] })
                    .expect("sacrifice the mana-value-3 artifact");
                paid_sacrifice = true;
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay mana from pool");
            }
            WaitingFor::Priority { .. } => {
                committed = true;
                break;
            }
            other => panic!("unexpected waiting state during cast: {other:?}"),
        }
    }
    assert!(paid_sacrifice, "the sacrifice additional cost must be paid");
    assert!(
        committed,
        "the spell must reach a post-commit priority window"
    );

    // Snapshot hand after the spell left for the stack (CR 601.2a) — this is
    // the baseline against which the draw delta is measured.
    let hand_after_commit = runner.state().players[0].hand.len();

    // Resolve the spell off the stack.
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve Morbid Curiosity");
    }

    let hand_after_resolve = runner.state().players[0].hand.len();
    let drawn = hand_after_resolve as i64 - hand_after_commit as i64;

    assert_eq!(
        drawn, 3,
        "Morbid Curiosity must draw cards equal to the sacrificed Coalition Relic's mana value (3); \
         hand_before={hand_before}, after_commit={hand_after_commit}, after_resolve={hand_after_resolve}"
    );
    assert_eq!(
        runner.state().objects[&relic].zone,
        Zone::Graveyard,
        "the chosen artifact must have been sacrificed"
    );
}
