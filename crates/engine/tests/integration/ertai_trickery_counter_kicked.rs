//! Ertai's Trickery — "Counter target spell if it was kicked."
//!
//! Parser regression: the trailing intervening-if must lower to
//! `AdditionalCostPaid`, not remain as swallowed `Condition_If` text.

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCondition, Effect};

const ERTAI_TRICKERY_ORACLE: &str = "Counter target spell if it was kicked.";

#[test]
fn ertai_trickery_parses_counter_with_kicked_condition() {
    let parsed = parse_oracle_text(
        ERTAI_TRICKERY_ORACLE,
        "Ertai's Trickery",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let ability = parsed
        .abilities
        .first()
        .expect("Ertai's Trickery must parse a spell ability");
    assert!(matches!(ability.effect.as_ref(), Effect::Counter { .. }));
    assert!(matches!(
        ability.condition.as_ref(),
        Some(AbilityCondition::AdditionalCostPaid { .. })
    ));
}
