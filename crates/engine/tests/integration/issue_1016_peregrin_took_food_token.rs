//! Issue #1016 — Peregrin Took must add an extra Food token whenever you create
//! tokens (CR 614.1a), including Food from Samwise Gamgee's ETB trigger.

use engine::game::scenario::{GameScenario, P0};
use engine::types::phase::Phase;

const PEREGRIN_ORACLE: &str = "If one or more tokens would be created under your control, those tokens plus an additional Food token are created instead.";
const SAMWISE_ORACLE: &str =
    "Whenever another nontoken creature enters the battlefield under your control, create a Food token.";

fn food_tokens(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == "Food"))
        })
        .count()
}

#[test]
fn peregrin_took_adds_food_when_samwise_creates_food() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Peregrin Took", 2, 2, PEREGRIN_ORACLE);
    scenario.add_creature_from_oracle(P0, "Samwise Gamgee", 2, 2, SAMWISE_ORACLE);
    let visitor = scenario
        .add_creature_to_hand(P0, "Visiting Hobbit", 1, 1)
        .with_mana_cost(engine::types::mana::ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    assert_eq!(food_tokens(&runner), 0, "no Food tokens before the ETB");

    runner.cast(visitor).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        food_tokens(&runner),
        2,
        "Samwise Food plus Peregrin replacement Food; waiting_for={:?}",
        runner.state().waiting_for
    );
}
