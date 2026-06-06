//! Regression for issue #2360: The Rack must fire only on the chosen opponent's
//! upkeep and deal max(0, 3 − hand size) damage instead of granting life.
//!
//! https://github.com/phase-rs/phase/issues/2360

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    ChoiceType, Effect, PlayerScope, QuantityExpr, QuantityRef, TargetFilter,
};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

const RACK_ORACLE: &str = "As this artifact enters, choose an opponent.\nAt the beginning of the chosen player's upkeep, this artifact deals X damage to that player, where X is 3 minus the number of cards in their hand.";

#[test]
fn the_rack_full_oracle_parses_replacement_and_trigger() {
    let parsed = parse_oracle_text(RACK_ORACLE, "The Rack", &[], &["Artifact".to_string()], &[]);
    assert_eq!(parsed.replacements.len(), 1);
    let replacement = &parsed.replacements[0];
    let execute = replacement
        .execute
        .as_ref()
        .expect("ETB replacement must execute");
    assert!(matches!(
        execute.effect.as_ref(),
        Effect::Choose {
            choice_type: ChoiceType::Opponent,
            persist: true,
        }
    ));

    assert_eq!(parsed.triggers.len(), 1);
    let trigger = &parsed.triggers[0];
    assert_eq!(trigger.mode, TriggerMode::Phase);
    assert_eq!(trigger.phase, Some(Phase::Upkeep));
    assert_eq!(
        trigger.valid_target,
        Some(TargetFilter::SourceChosenPlayer),
        "chosen player's upkeep must bind valid_target to SourceChosenPlayer (#2360)"
    );

    let execute = trigger
        .execute
        .as_ref()
        .expect("upkeep trigger must execute");
    match execute.effect.as_ref() {
        Effect::DealDamage { amount, target, .. } => {
            assert_eq!(
                target,
                &TargetFilter::SourceChosenPlayer,
                "damage must hit the chosen opponent, not TriggeringPlayer"
            );
            assert!(
                matches!(
                    amount,
                    QuantityExpr::ClampMin {
                        minimum: 0,
                        inner,
                    } if matches!(
                        inner.as_ref(),
                        QuantityExpr::Offset { offset: 3, inner }
                            if matches!(
                                inner.as_ref(),
                                QuantityExpr::Multiply { factor: -1, inner }
                                    if matches!(
                                        inner.as_ref(),
                                        QuantityExpr::Ref {
                                            qty: QuantityRef::HandSize {
                                                player: PlayerScope::SourceChosenPlayer,
                                            },
                                        }
                                    )
                            )
                    )
                ),
                "X must be max(0, 3 − chosen player's hand size), got {amount:?}"
            );
        }
        other => panic!("expected DealDamage, got {other:?}"),
    }
}
