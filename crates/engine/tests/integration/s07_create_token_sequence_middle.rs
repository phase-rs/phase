//! S07 token-splitter class fix — runtime chain-walk coverage.
//!
//! CR 608.2c: "create A, a B, and a C token" is a do-ALL list in written order.
//! The old binary split dropped the MIDDLE token; the N-way split chains all N
//! via `sub_ability`. These tests drive the real cast pipeline
//! (`GameRunner::cast(..).resolve()`) so the runtime `sub_ability` chain driver
//! must resolve every node — reverting the parser fix drops the middle token and
//! the token delta falls from 3 to 2, failing each assertion below.

use engine::game::scenario::{GameScenario, P0};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::ObjectId;

/// Cast a no-target sorcery from its oracle text and return the names of the
/// token permanents on the battlefield after it resolves.
fn resolve_and_collect_token_names(oracle: &str) -> Vec<String> {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Ample mana so any mana cost the harness attaches is auto-payable; a
    // no-cost oracle spell simply leaves it unused.
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "S07 Token Sequence Probe", false, oracle)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();

    let state = outcome.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.is_token)
        .map(|obj| obj.name.clone())
        .collect()
}

#[test]
fn bestial_menace_creates_all_three_creature_tokens() {
    // Bestial Menace: Snake 1/1, Wolf 2/2 (the middle), Elephant 3/3.
    let names = resolve_and_collect_token_names(
        "Create a 1/1 green Snake creature token, a 2/2 green Wolf creature token, and a 3/3 green Elephant creature token.",
    );
    assert_eq!(
        names.len(),
        3,
        "expected 3 token creatures on the battlefield, got {names:?}"
    );
    for expected in ["Snake", "Wolf", "Elephant"] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing {expected} token; battlefield tokens: {names:?}"
        );
    }
}

#[test]
fn fae_offering_creates_all_three_predefined_artifact_tokens() {
    // Fae Offering's create clause (predefined-artifact token class): Clue,
    // Food (the middle), Treasure. Driven as a sorcery so the identical parsed
    // sub_ability chain resolves through the real stack.
    let names =
        resolve_and_collect_token_names("Create a Clue token, a Food token, and a Treasure token.");
    assert_eq!(
        names.len(),
        3,
        "expected Clue + Food + Treasure, got {names:?}"
    );
    for expected in ["Clue", "Food", "Treasure"] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing {expected} token; battlefield tokens: {names:?}"
        );
    }
}
