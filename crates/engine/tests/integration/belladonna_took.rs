//! Belladonna Took's ordinal token-entry trigger resolves each printed clause
//! at its matching resolution count.

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::phase::Phase;

const BELLADONNA_TOOK_ORACLE: &str = "Whenever a token you control enters, you gain 1 life if this is the first time this ability has resolved this turn. If it's the second time, draw a card. If it's the third time, put a +1/+1 counter on each creature you control.";
const RAISE_THE_ALARM_ORACLE: &str = "Create two 1/1 white Soldier creature tokens.";

/// CR 603.2 + CR 608.2c: Each token entry triggers Belladonna Took separately,
/// and each resolving trigger follows only the matching ordinal instruction.
#[test]
fn belladonna_took_resolves_ordinal_token_entry_clauses_in_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let belladonna = scenario
        .add_creature_from_oracle(P0, "Belladonna Took", 1, 2, BELLADONNA_TOOK_ORACLE)
        .id();
    let witness = scenario.add_creature(P0, "Counter witness", 1, 1).id();
    scenario.with_library_top(P0, &["Draw witness"]);
    let first_raise = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM_ORACLE)
        .id();
    let second_raise = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM_ORACLE)
        .id();
    let mut runner = scenario.build();

    let first = runner.cast(first_raise).resolve();
    first.assert_life_delta(P0, 1);
    first.assert_hand_drawn(P0, 1);
    assert_eq!(
        first.counters(witness, CounterType::Plus1Plus1),
        0,
        "reach guard: the third-resolution counter clause must not fire during entries one and two"
    );

    let second = runner.cast(second_raise).resolve();
    second.assert_life_delta(P0, 0);
    second.assert_hand_drawn(P0, 0);
    assert_eq!(
        second.counters(witness, CounterType::Plus1Plus1),
        1,
        "the third token entry must place exactly one +1/+1 counter on the witness"
    );
    assert_eq!(
        second
            .state()
            .ability_resolutions_this_turn
            .get(&(belladonna, 0)),
        Some(&4),
        "two two-token spells must resolve Belladonna Took's trigger four times"
    );
}
