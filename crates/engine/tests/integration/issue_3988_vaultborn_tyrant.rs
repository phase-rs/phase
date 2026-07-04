//! Regression for issue #3988: Vaultborn Tyrant's dies trigger must copy itself
//! when it's not a token.
//!
//! https://github.com/phase-rs/phase/issues/3988

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, FilterProp, TargetFilter, TriggerCondition};
use engine::types::zones::Zone;

const VAULTBORN_TYRANT_ORACLE: &str = "Trample\n\
Whenever a creature you control enters, if its power is 4 or greater, you gain 3 life and draw a card.\n\
Whenever Vaultborn Tyrant or another creature you control enters, if it's not a token, each other player creates a token that's a copy of it.\n\
Whenever Vaultborn Tyrant or another creature you control dies, if it's not a token, you create a token that's a copy of it.";

#[test]
fn vaultborn_tyrant_dies_intervening_if_matches_graveyard_destination() {
    let parsed = parse_oracle_text(
        VAULTBORN_TYRANT_ORACLE,
        "Vaultborn Tyrant",
        &["Trample".to_string()],
        &["Creature".to_string()],
        &["Dinosaur".to_string()],
    );
    let dies = parsed
        .triggers
        .iter()
        .find(|t| {
            t.destination == Some(Zone::Graveyard)
                && matches!(
                    t.execute.as_ref().map(|e| e.effect.as_ref()),
                    Some(Effect::CopyTokenOf { .. })
                )
        })
        .expect("Vaultborn Tyrant must parse a dies CopyTokenOf trigger");
    let Some(TriggerCondition::ZoneChangeObjectMatchesFilter {
        destination,
        filter,
        ..
    }) = dies.condition.as_ref()
    else {
        panic!(
            "dies trigger must carry NonToken intervening-if, got {:?}",
            dies.condition
        );
    };
    assert_eq!(
        *destination,
        Zone::Graveyard,
        "NonToken intervening-if on dies must match graveyard destination"
    );
    let TargetFilter::Typed(typed) = filter else {
        panic!("expected typed filter, got {filter:?}");
    };
    assert!(
        typed.properties.contains(&FilterProp::NonToken),
        "expected NonToken property, got {:?}",
        typed.properties
    );
}
