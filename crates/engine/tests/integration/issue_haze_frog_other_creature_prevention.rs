//! Haze Frog — ETB prevention must scope to other creature sources only.
//!
//! Oracle: "When this creature enters, prevent all combat damage that other
//! creatures would deal this turn."

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    Effect, FilterProp, PreventionAmount, PreventionScope, TargetFilter, TypeFilter,
};
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const HAZE_FROG_ORACLE: &str = "Flash\n\
When this creature enters, prevent all combat damage that other creatures would deal this turn.";

#[test]
fn haze_frog_etb_prevent_scopes_other_creature_sources() {
    let parsed = parse_oracle_text(
        HAZE_FROG_ORACLE,
        "Haze Frog",
        &["Flash".to_string()],
        &["Creature".to_string()],
        &["Frog".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ChangesZone && t.destination == Some(Zone::Battlefield))
        .expect("Haze Frog must have an ETB trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    let Effect::PreventDamage {
        amount,
        scope,
        damage_source_filter,
        ..
    } = execute.effect.as_ref()
    else {
        panic!("expected PreventDamage, got {:?}", execute.effect);
    };
    assert_eq!(*amount, PreventionAmount::All);
    assert_eq!(*scope, PreventionScope::CombatDamage);
    let TargetFilter::Typed(tf) = damage_source_filter
        .as_ref()
        .expect("Haze Frog must scope prevention to creature sources")
    else {
        panic!("expected Typed source filter, got {damage_source_filter:?}");
    };
    assert!(
        tf.type_filters
            .iter()
            .any(|t| matches!(t, TypeFilter::Creature)),
        "source filter must be creatures, got {:?}",
        tf.type_filters
    );
    assert!(
        tf.properties.contains(&FilterProp::Another),
        "other creatures must carry FilterProp::Another, got {:?}",
        tf.properties
    );
}
