//! Regression for issue #2871: Currency Converter's {T} ability must not create
//! a token when no card is exiled with it.
//!
//! https://github.com/phase-rs/phase/issues/2871

use engine::game::scenario::{GameScenario, P0};
use engine::types::phase::Phase;

#[test]
fn issue_2871_currency_converter_tap_creates_no_token_without_exiled_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let converter = scenario
        .add_creature(P0, "Currency Converter", 0, 0)
        .as_artifact()
        .from_oracle_text(
            "{T}: Put a card exiled with this artifact into its owner's graveyard. \
             If it's a land card, create a Treasure token. \
             If it's a nonland card, create a 2/2 black Rogue creature token.",
        )
        .id();

    let mut runner = scenario.build();

    let treasure_before = count_battlefield_tokens(runner.state(), "Treasure");
    let rogue_before = count_battlefield_tokens(runner.state(), "Rogue");

    runner.activate(converter, 0).resolve();

    let treasure_after = count_battlefield_tokens(runner.state(), "Treasure");
    let rogue_after = count_battlefield_tokens(runner.state(), "Rogue");

    assert_eq!(
        treasure_after, treasure_before,
        "must not create Treasure with no exiled card"
    );
    assert_eq!(
        rogue_after, rogue_before,
        "must not create Rogue with no exiled card"
    );
}

fn count_battlefield_tokens(state: &engine::types::game_state::GameState, subtype: &str) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            obj.card_types
                .subtypes
                .iter()
                .any(|s| s.eq_ignore_ascii_case(subtype))
        })
        .count()
}
