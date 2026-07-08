//! Incremental Growth — multi-target +1/+1 counter chain.
//!
//! https://github.com/phase-rs/phase/issues/4503

use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{
    AbilityDefinition, Effect, FilterProp, QuantityExpr, TargetFilter, TypeFilter,
};
use engine::types::counter::CounterType;

const INCREMENTAL_GROWTH_ORACLE: &str = "Put a +1/+1 counter on target creature, two +1/+1 \
counters on another target creature, and three +1/+1 counters on a third target creature.";

fn parse_incremental_growth() -> ParsedAbilities {
    parse_oracle_text(
        INCREMENTAL_GROWTH_ORACLE,
        "Incremental Growth",
        &[],
        &["Sorcery".to_string()],
        &[],
    )
}

fn typed_creature(filter: &TargetFilter) -> Option<&engine::types::ability::TypedFilter> {
    match filter {
        TargetFilter::Typed(tf) => Some(tf),
        TargetFilter::And { filters } => filters.iter().find_map(typed_creature),
        _ => None,
    }
}

fn assert_plus_counter_node(
    def: &AbilityDefinition,
    count: i32,
    another: bool,
) -> Option<&AbilityDefinition> {
    match def.effect.as_ref() {
        Effect::PutCounter {
            counter_type,
            count: QuantityExpr::Fixed { value },
            target,
        } => {
            assert_eq!(*counter_type, CounterType::Plus1Plus1);
            assert_eq!(*value, count);
            let tf = typed_creature(target).expect("counter target should be typed creature");
            assert!(tf
                .type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Creature)),);
            assert_eq!(
                tf.properties.contains(&FilterProp::Another),
                another,
                "unexpected Another property on {tf:?}",
            );
        }
        other => panic!("expected PutCounter, got {other:?}"),
    }
    def.sub_ability.as_deref()
}

#[test]
fn incremental_growth_spell_parses_three_leg_put_counter_chain() {
    let parsed = parse_incremental_growth();
    let def = parsed
        .abilities
        .first()
        .expect("Incremental Growth should parse to a spell ability");

    let second = assert_plus_counter_node(def, 1, false).expect("second counter node");
    let third = assert_plus_counter_node(second, 2, true).expect("third counter node");
    assert!(
        assert_plus_counter_node(third, 3, true).is_none(),
        "counter chain should contain exactly three nodes",
    );
}

#[test]
fn incremental_growth_parsed_has_no_unimplemented_leaks() {
    let parsed = parse_incremental_growth();
    let def = parsed.abilities.first().expect("spell ability");
    assert!(
        !matches!(def.effect.as_ref(), Effect::Unimplemented { .. }),
        "primary effect leaked Unimplemented: {:?}",
        def.effect
    );
}
