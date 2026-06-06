//! Regression for issue #2371: Triskaidekaphile must only win the game at
//! upkeep when the controller has exactly 13 cards in hand.
//!
//! https://github.com/phase-rs/phase/issues/2371

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    Comparator, Effect, PlayerScope, QuantityExpr, QuantityRef, TriggerCondition,
};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

const TRISKAIDEKAPHILE_ORACLE: &str = "You have no maximum hand size.\nAt the beginning of your upkeep, if you have exactly thirteen cards in your hand, you win the game.\n{3}{U}: Draw a card.";

#[test]
fn triskaidekaphile_upkeep_win_requires_exactly_thirteen_cards_in_hand() {
    let parsed = parse_oracle_text(
        TRISKAIDEKAPHILE_ORACLE,
        "Triskaidekaphile",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Wizard".to_string()],
    );
    let trigger = parsed
        .triggers
        .first()
        .expect("Triskaidekaphile must have an upkeep trigger");
    assert_eq!(trigger.mode, TriggerMode::Phase);
    assert_eq!(trigger.phase, Some(Phase::Upkeep));
    match trigger.condition.as_ref() {
        Some(TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        }) => {
            assert_eq!(
                lhs,
                &QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                }
            );
            assert_eq!(*comparator, Comparator::EQ);
            assert_eq!(rhs, &QuantityExpr::Fixed { value: 13 });
        }
        other => {
            panic!("upkeep trigger must require exactly 13 cards in hand (#2371), got {other:?}")
        }
    }
    match trigger
        .execute
        .as_ref()
        .map(|ability| ability.effect.as_ref())
    {
        Some(Effect::WinTheGame { .. }) => {}
        other => panic!("expected WinTheGame effect, got {other:?}"),
    }
}
