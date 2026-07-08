//! Regression for issue #3871: Summoner's Pact must install a delayed trigger
//! on resolution, not a printed battlefield upkeep trigger.
//!
//! https://github.com/phase-rs/phase/issues/3871

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{DelayedTriggerCondition, Effect};
use engine::types::phase::Phase;

const SUMMONERS_PACT_ORACLE: &str = "Search your library for a green creature card, reveal it, put it into your hand, then shuffle.\n\
At the beginning of your next upkeep, pay {2}{G}{G}. If you don't, you lose the game.";

#[test]
fn summoners_pact_parses_upkeep_clause_as_delayed_trigger() {
    let parsed = parse_oracle_text(
        SUMMONERS_PACT_ORACLE,
        "Summoner's Pact",
        &[],
        &["Instant".to_string()],
        &[],
    );
    assert!(
        parsed.triggers.is_empty(),
        "upkeep clause must not be a printed trigger, got {:?}",
        parsed.triggers
    );
    let delayed = parsed
        .abilities
        .iter()
        .find(|a| matches!(a.effect.as_ref(), Effect::CreateDelayedTrigger { .. }));
    let Some(ability) = delayed else {
        panic!(
            "expected CreateDelayedTrigger ability, got {:?}",
            parsed
                .abilities
                .iter()
                .map(|a| &*a.effect)
                .collect::<Vec<_>>()
        );
    };
    let Effect::CreateDelayedTrigger { condition, .. } = ability.effect.as_ref() else {
        unreachable!();
    };
    assert!(
        matches!(
            condition,
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::Upkeep,
                ..
            }
        ),
        "expected controller's next upkeep delayed trigger, got {condition:?}"
    );
}
