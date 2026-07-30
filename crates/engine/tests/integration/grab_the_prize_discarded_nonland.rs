//! CR 601.2b + CR 120.3 + CR 608.2c: Grab the Prize — "As an additional cost to
//! cast this spell, discard a card. Draw two cards. If the discarded card wasn't
//! a land card, Grab the Prize deals 2 damage to each opponent."
//!
//! Exercises the cost-paid-object negation wiring: "the discarded card wasn't a
//! land card" parses to `Not(CostPaidObjectMatchesFilter { Land })` (the new
//! `wasn't`/`isn't` copula-negation arm + the trailing `" card"` class-word
//! strip). Reverting the negation arm makes the filter match the discarded
//! LAND (no `Not`), so a discarded land would (wrongly) deal 2 damage and a
//! discarded nonland would not — inverting both assertions.

use engine::game::scenario::GameScenario;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const GRAB: &str = "As an additional cost to cast this spell, discard a card.\n\
Draw two cards. If the discarded card wasn't a land card, Grab the Prize deals \
2 damage to each opponent.";

/// P1 life delta after casting Grab the Prize discarding either a land or a
/// nonland card.
fn run(discard_land: bool) -> i32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Library for the "draw two".
    scenario.add_card_to_library_top(P0, "Lib A");
    scenario.add_card_to_library_top(P0, "Lib B");
    // The single discardable card: a land or a nonland creature.
    let discard_target = if discard_land {
        scenario.add_land_to_hand(P0, "Forest").id()
    } else {
        scenario.add_creature_to_hand(P0, "Bear", 2, 2).id()
    };
    let grab = scenario
        .add_spell_to_hand_from_oracle(P0, "Grab the Prize", false, GRAB)
        .id();
    let mut runner = scenario.build();
    let p1_before = runner.state().players[P1.0 as usize].life;

    let card_id = runner.state().objects[&grab].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: grab,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Grab the Prize");

    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("finalize mana");
            }
            WaitingFor::PayCost { .. } | WaitingFor::DiscardChoice { .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![discard_target],
                    })
                    .expect("pay discard cost");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("resolve");
            }
            other => panic!("unexpected window: {other:?}"),
        }
    }
    runner.state().players[P1.0 as usize].life - p1_before
}

#[test]
fn discarding_a_nonland_deals_two_to_each_opponent() {
    let land_delta = run(true);
    let nonland_delta = run(false);
    assert_eq!(land_delta, 0, "discarding a LAND deals no damage");
    assert_eq!(
        nonland_delta, -2,
        "CR 120.3: discarding a nonland → 2 damage to each opponent"
    );
}
