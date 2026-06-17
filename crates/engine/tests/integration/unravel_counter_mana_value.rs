//! Unravel — "Counter target spell. If the amount of mana spent to cast that
//! spell was less than its mana value, you draw a card."
//!
//! Parser regression: the intervening-if on the draw rider must lower to a
//! `QuantityCheck` over targeted-spell mana spent vs mana value.

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, CastManaObjectScope, CastManaSpentMetric, Comparator, Effect, ObjectScope,
    QuantityExpr, QuantityRef,
};

const UNRAVEL_ORACLE: &str = "Counter target spell. If the amount of mana spent to cast that spell was less than its mana value, you draw a card.";

#[test]
fn unravel_parses_counter_and_conditional_draw() {
    let parsed = parse_oracle_text(UNRAVEL_ORACLE, "Unravel", &[], &["Instant".to_string()], &[]);
    let counter = parsed
        .abilities
        .first()
        .expect("Unravel must parse a counter ability");
    assert!(matches!(counter.effect.as_ref(), Effect::Counter { .. }));
    assert!(counter.condition.is_none());

    let draw = counter
        .sub_ability
        .as_ref()
        .expect("Unravel must chain a conditional draw sub-ability");
    assert!(matches!(draw.effect.as_ref(), Effect::Draw { .. }));
    assert!(matches!(
        draw.condition.as_ref(),
        Some(AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::AbilityTarget,
                    metric: CastManaSpentMetric::Total,
                },
            },
            comparator: Comparator::LT,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Target,
                },
            },
        })
    ));
}
