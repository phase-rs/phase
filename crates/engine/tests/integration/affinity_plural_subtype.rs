//! Regression coverage for plural creature subtypes in `Affinity for <subtype>`.
//!
//! MTGJSON provides a bare `Affinity` keyword while the Oracle line supplies
//! its parameter. These tests take that production synthesis path for the two
//! real irregular-plural cards in the current corpus.

use engine::game::casting::effective_spell_cost;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CANTANKEROUS_KEEPERS: &str = "Affinity for Elves (This spell costs {1} less to cast for each Elf you control.)\nWhen this creature enters, mill four cards, then put all Elf cards from among them into your hand.";
const ALLIES_AT_LAST: &str = "Affinity for Allies (This spell costs {1} less to cast for each Ally you control.)\nUp to two target creatures you control each deal damage equal to their power to target creature an opponent controls.";

#[test]
fn affinity_for_elves_canonicalizes_and_pays_for_cantankerous_keepers() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );

    let keepers = {
        let mut builder = scenario.add_creature_to_hand(P0, "Cantankerous Keepers", 4, 4);
        builder
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 5,
            })
            .from_oracle_text_with_keywords(&["Affinity", "Mill"], CANTANKEROUS_KEEPERS);
        builder.id()
    };

    for i in 0..3 {
        scenario
            .add_creature(P0, &format!("Elf {i}"), 1, 1)
            .with_subtypes(vec!["Elf"]);
    }
    scenario
        .add_creature(P0, "Non-Elf", 1, 1)
        .with_subtypes(vec!["Human"]);
    scenario
        .add_creature(P1, "Opponent Elf", 1, 1)
        .with_subtypes(vec!["Elf"]);

    let mut runner = scenario.build();
    let cost = effective_spell_cost(runner.state(), P0, keepers)
        .expect("Cantankerous Keepers cost should compute");
    assert_eq!(
        cost,
        ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        },
        "CR 702.41a: exactly three controlled Elves reduce {{5}}{{G}} to {{2}}{{G}}"
    );

    let outcome = runner.cast(keepers).resolve();
    outcome.assert_zone(&[keepers], Zone::Battlefield);
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "the reduced cost spends all three mana"
    );
}

#[test]
fn affinity_for_allies_survives_name_normalization_and_pays_for_allies_at_last() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![])],
    );

    let source_a = scenario
        .add_creature(P0, "Ally A", 4, 4)
        .with_subtypes(vec!["Ally"])
        .id();
    let source_b = scenario
        .add_creature(P0, "Ally B", 4, 4)
        .with_subtypes(vec!["Ally"])
        .id();
    let recipient = scenario.add_creature(P1, "Recipient", 2, 7).id();

    let spell = {
        let mut builder = scenario.add_spell_to_hand(P0, "Allies at Last", false);
        builder
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 2,
            })
            .from_oracle_text_with_keywords(&["Affinity"], ALLIES_AT_LAST);
        builder.id()
    };

    let mut runner = scenario.build();
    assert_eq!(
        effective_spell_cost(runner.state(), P0, spell),
        Some(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        }),
        "CR 702.41a: two controlled Allies reduce {{2}}{{G}} to {{G}}"
    );

    let outcome = runner
        .cast(spell)
        .target_objects(&[source_a, source_b, recipient])
        .resolve();
    outcome.assert_zone(&[spell, recipient], Zone::Graveyard);
    outcome.assert_zone(&[source_a, source_b], Zone::Battlefield);
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "the reduced cost spends the only mana"
    );
}
