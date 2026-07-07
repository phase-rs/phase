//! Runtime coverage for source-defined X/X token P/T.
//!
//! Slime Molding's Oracle text defines the token as X/X. CR 107.3a and
//! CR 601.2b make X the value announced while casting the spell, so the token's
//! power and toughness must resolve from the cast ability's chosen X.

use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SLIME_MOLDING_ORACLE: &str = "Create an X/X green Ooze creature token.";

fn slime_molding_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::Green],
        generic: 0,
    }
}

fn green_mana(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn slime_molding_x_four_creates_four_four_ooze() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, green_mana(5));
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Slime Molding", false, SLIME_MOLDING_ORACLE);
    builder.with_mana_cost(slime_molding_cost());
    let spell = builder.id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).x(4).resolve();

    let tokens: Vec<_> = outcome
        .state()
        .objects
        .values()
        .filter(|object| object.is_token && object.zone == Zone::Battlefield)
        .collect();
    assert_eq!(
        tokens.len(),
        1,
        "Slime Molding must create exactly one token"
    );
    let token = tokens[0];
    assert_eq!(token.name, "Ooze");
    assert_eq!(token.power, Some(4));
    assert_eq!(token.toughness, Some(4));
    assert!(
        token
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Ooze"),
        "token must carry the Ooze subtype, got {:?}",
        token.card_types.subtypes
    );
}
