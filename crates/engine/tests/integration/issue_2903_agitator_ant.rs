//! Regression for issue #2903: Agitator Ant end-step counters and goad.
//!
//! https://github.com/phase-rs/phase/issues/2903
//!
//! Each player may put counters on their own creature; only creatures that
//! received counters this way are goaded.

use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{
    ControllerRef, Effect, PlayerFilter, QuantityExpr, TargetFilter, TypedFilter,
};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

const AGITATOR_ANT_ORACLE: &str = "\
At the beginning of your end step, each player may put two +1/+1 counters on a creature they control. Goad each creature that had counters put on it this way.";

fn parse_agitator_ant() -> ParsedAbilities {
    parse_oracle_text(
        AGITATOR_ANT_ORACLE,
        "Agitator Ant",
        &[],
        &["Creature".to_string()],
        &["Insect".to_string()],
    )
}

#[test]
fn agitator_ant_parsed_trigger_scopes_counters_and_goad() {
    let parsed = parse_agitator_ant();
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::End))
        .expect("Agitator Ant end step trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    assert!(execute.optional, "counter step must be optional");
    assert_eq!(execute.player_scope, Some(PlayerFilter::All));
    let Effect::PutCounter { target, count, .. } = execute.effect.as_ref() else {
        panic!("expected PutCounter, got {:?}", execute.effect);
    };
    assert_eq!(
        *target,
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::ScopedPlayer))
    );
    assert!(
        matches!(count, QuantityExpr::Fixed { value: 2 }),
        "expected two counters, got {count:?}"
    );
    let sub = execute.sub_ability.as_ref().expect("goad sub");
    let Effect::GoadAll { target } = sub.effect.as_ref() else {
        panic!("expected GoadAll, got {:?}", sub.effect);
    };
    assert!(matches!(target, TargetFilter::TrackedSetFiltered { .. }));
}

#[test]
fn agitator_ant_parsed_has_no_unimplemented_leaks() {
    fn has_unimplemented(effect: &Effect) -> bool {
        matches!(effect, Effect::Unimplemented { .. })
    }

    let parsed = parse_agitator_ant();
    let execute = parsed.triggers[0].execute.as_ref().expect("execute");
    assert!(
        !has_unimplemented(execute.effect.as_ref()),
        "primary effect leaked Unimplemented: {:?}",
        execute.effect
    );
    if let Some(sub) = &execute.sub_ability {
        assert!(
            !has_unimplemented(sub.effect.as_ref()),
            "goad sub leaked Unimplemented: {:?}",
            sub.effect
        );
    }
}
