//! Issue #3324 — Haunted One must buff only the tapped commander and other
//! creatures you control that share a creature type with it.

use engine::parser::oracle_effect::parse_effect_chain;
use engine::parser::oracle_static::parse_static_line;
use engine::types::ability::{
    AbilityKind, ContinuousModification, Effect, FilterProp, SharedQuality, TargetFilter,
};

const HAUNTED_ONE_ORACLE: &str = "Commander creatures you own have \"Whenever this creature becomes tapped, it and other creatures you control that share a creature type with it each get +2/+0 and gain undying until end of turn.\" (When a creature with undying dies, if it had no +1/+1 counters on it, return it to the battlefield under its owner's control with a +1/+1 counter on it.)";

const GRANTED_BODY: &str =
    "it and other creatures you control that share a creature type with it each get +2/+0 and gain undying until end of turn";

#[test]
fn haunted_one_granted_trigger_body_is_not_unfiltered_any() {
    let def = parse_effect_chain(GRANTED_BODY, AbilityKind::Spell);
    assert!(
        !matches!(
            def.effect.as_ref(),
            Effect::GenericEffect {
                target: Some(TargetFilter::Any),
                ..
            }
        ),
        "Haunted One body must not broadcast to Any, got {:?}",
        def.effect
    );
}

#[test]
fn haunted_one_static_grants_compound_subject_trigger() {
    let def = parse_static_line(HAUNTED_ONE_ORACLE).expect("Haunted One static must parse");
    let grant = def
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::GrantTrigger { trigger } => Some(trigger),
            _ => None,
        })
        .expect("Haunted One must parse as GrantTrigger");

    let execute = grant.execute.as_ref().expect("grant must have execute");
    assert!(
        !matches!(
            execute.effect.as_ref(),
            Effect::GenericEffect {
                target: Some(TargetFilter::Any),
                ..
            }
        ),
        "granted execute must not use unfiltered Any, got {:?}",
        execute.effect
    );

    let tail = execute.sub_ability.as_ref().expect("compound-subject tail");
    let tail_target = match &*tail.effect {
        Effect::GenericEffect {
            target: Some(t), ..
        }
        | Effect::Pump { target: t, .. } => t,
        other => panic!("expected tail with recipient filter, got {other:?}"),
    };
    let TargetFilter::Typed(tf) = tail_target else {
        panic!("expected typed tail filter, got {tail_target:?}");
    };
    assert!(tf.properties.contains(&FilterProp::Another));
    assert!(tf.properties.iter().any(|p| matches!(
        p,
        FilterProp::SharesQuality {
            quality: SharedQuality::CreatureType,
            reference: Some(reference),
            ..
        } if matches!(reference.as_ref(), TargetFilter::TriggeringSource)
    )));
    assert_eq!(
        tf.controller,
        Some(engine::types::ability::ControllerRef::You)
    );
    assert!(tf
        .type_filters
        .contains(&engine::types::ability::TypeFilter::Creature));
}
