//! Issue #1338 — Urge to Feed: "each of those Vampires" must not put +1/+1
//! counters on every permanent (empty TypedFilter fallback).

use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, Effect, EffectOutcomeSignal, EffectScope, TapStateChange, TargetFilter,
    TypeFilter,
};
use engine::types::counter::CounterType;
use engine::types::identifiers::TrackedSetId;

const URGE_TO_FEED: &str = "Target creature gets -3/-3 until end of turn. You may tap any \
number of untapped Vampire creatures you control. If you do, put a +1/+1 counter on each \
of those Vampires.";

fn parsed_spell() -> engine::types::ability::AbilityDefinition {
    let parsed = parse_oracle_text(
        URGE_TO_FEED,
        "Urge to Feed",
        &[],
        &["Instant".to_string()],
        &[],
    );
    parsed
        .abilities
        .into_iter()
        .next()
        .expect("Urge to Feed should parse as a spell ability")
}

#[test]
fn urge_to_feed_each_of_those_vampires_is_tracked_set_filtered() {
    let def = parsed_spell();
    let tap = def
        .sub_ability
        .as_ref()
        .expect("tap clause should be sub_ability after -3/-3");
    assert!(
        matches!(
            tap.effect.as_ref(),
            Effect::SetTapState {
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
                ..
            }
        ),
        "expected Tap after PT layer, got {:?}",
        tap.effect
    );

    let counter = tap
        .sub_ability
        .as_ref()
        .expect("counter clause should follow tap");
    assert!(
        matches!(
            counter.condition,
            Some(AbilityCondition::EffectOutcome {
                signal: EffectOutcomeSignal::OptionalEffectPerformed,
            })
        ),
        "counter should be gated on accepting the tap, got {:?}",
        counter.condition
    );

    match counter.effect.as_ref() {
        Effect::PutCounterAll {
            target,
            counter_type,
            ..
        } => {
            assert_eq!(*counter_type, CounterType::Plus1Plus1);
            match target {
                TargetFilter::TrackedSetFiltered { id, filter, .. } => {
                    assert_eq!(*id, TrackedSetId(0));
                    match filter.as_ref() {
                        TargetFilter::Typed(tf) => {
                            assert!(tf
                                .type_filters
                                .contains(&TypeFilter::Subtype("Vampire".into())));
                        }
                        other => panic!("expected Typed Vampire filter, got {other:?}"),
                    }
                }
                other => panic!("expected TrackedSetFiltered target, got {other:?}"),
            }
        }
        other => panic!("expected PutCounterAll, got {other:?}"),
    }
}
