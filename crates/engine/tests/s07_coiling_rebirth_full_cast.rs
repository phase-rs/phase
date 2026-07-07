//! CR 601.2b + CR 702.174b + CR 205.4a: Coiling Rebirth — full cast→resolve
//! hardening test (folds into S07 Batch 3).
//!
//! "Gift a card ... Return target creature card from your graveyard to the
//! battlefield. Then if the gift was promised and that creature isn't legendary,
//! create a token that's a copy of that creature, except it's 1/1."
//!
//! Coiling's gift-gated token copy was previously verified only via a
//! condition-eval unit test. This drives the real cast pipeline and pins:
//!   (1) the returned creature's ObjectId survives the graveyard→battlefield
//!       return so `Not(Legendary)` reads its LIVE supertypes at resolution;
//!   (2) the resolver gates the CopyTokenOf on `And([AdditionalCostPaid,
//!       Not(Legendary)])`.
//! Reverting the gate makes the token appear in both negative arms.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::ObjectId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const COILING: &str = "Gift a card (You may promise an opponent a gift as you cast this spell. \
If you do, they draw a card before its other effects.)\n\
Return target creature card from your graveyard to the battlefield. Then if the gift was promised \
and that creature isn't legendary, create a token that's a copy of that creature, except it's 1/1.";

/// Returns the number of token permanents on the battlefield after resolution.
fn run_coiling(promise_gift: bool, legendary: bool) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // {3}{B}{B}: five black mana cover the two B pips and three generic.
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    // Opponent needs a library card to draw the promised gift.
    scenario.add_card_to_library_top(P1, "Gift Card");
    let mut fodder = scenario.add_creature_to_graveyard(P0, "Fodder", 3, 3);
    if legendary {
        fodder.as_legendary();
    }
    let fodder = fodder.id();
    let coiling = scenario
        .add_spell_to_hand_from_oracle(P0, "Coiling Rebirth", false, COILING)
        .id();
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&coiling].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: coiling,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Coiling Rebirth");

    for _ in 0..40 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("mana");
            }
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: promise_gift })
                    .expect("decide gift");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(fodder)),
                    })
                    .expect("choose graveyard creature");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("resolve");
            }
            other => panic!("coiling unexpected window: {other:?}"),
        }
    }
    token_count(&runner)
}

fn token_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.zone == engine::types::zones::Zone::Battlefield)
        .count()
}

#[test]
fn coiling_rebirth_token_only_when_gift_promised_and_nonlegendary() {
    // Positive: gift promised AND returned creature non-legendary → copy token.
    assert_eq!(
        run_coiling(true, false),
        1,
        "gift promised + nonlegendary → copy token created"
    );
    // Negative A: returned creature IS legendary → Not(Legendary) fails → no token.
    assert_eq!(
        run_coiling(true, true),
        0,
        "returned creature legendary → no copy token"
    );
    // Negative B: gift NOT promised → AdditionalCostPaid fails → no token.
    assert_eq!(
        run_coiling(false, false),
        0,
        "gift not promised → no copy token"
    );
}
