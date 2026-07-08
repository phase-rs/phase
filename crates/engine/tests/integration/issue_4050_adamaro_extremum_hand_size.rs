//! Regression for issue #4050: Adamaro's P/T CDA must parse the verbose
//! opponent-scoped hand-size extremum via existing `QuantityRef::HandSize` and
//! `PlayerScope::Opponent { aggregate: Max }`, not `Effect::Unimplemented`.
//!
//! https://github.com/phase-rs/phase/issues/4050

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AggregateFunction, ContinuousModification, Effect, PlayerScope, QuantityExpr, QuantityRef,
    StaticDefinition, TargetFilter,
};

use crate::support::shared_card_db as load_db;

const ADAMARO: &str = "Adamaro, First to Desire";
const ADAMARO_ORACLE: &str = "Adamaro's power and toughness are each equal to the number of cards in the hand of the opponent with the most cards in hand.";

fn expected_hand_size_qty() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::HandSize {
            player: PlayerScope::Opponent {
                aggregate: AggregateFunction::Max,
            },
        },
    }
}

fn assert_adamaro_cda_modifications(def: &StaticDefinition) {
    let qty = expected_hand_size_qty();
    assert!(def.characteristic_defining, "Adamaro P/T line is a CDA");
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.modifications,
        vec![
            ContinuousModification::SetDynamicPower { value: qty.clone() },
            ContinuousModification::SetDynamicToughness { value: qty },
        ]
    );
}

#[test]
fn adamaro_cda_parses_opponent_max_hand_size_extremum() {
    let parsed = parse_oracle_text(
        ADAMARO_ORACLE,
        ADAMARO,
        &[],
        &["Creature".to_string()],
        &["Spirit".to_string()],
    );

    assert!(
        !parsed
            .abilities
            .iter()
            .any(|a| { matches!(a.effect.as_ref(), Effect::Unimplemented { .. }) }),
        "Adamaro must not contain Effect::Unimplemented nodes"
    );

    let cda = parsed
        .statics
        .iter()
        .find(|s| s.characteristic_defining)
        .expect("Adamaro must parse a characteristic-defining static");
    assert_adamaro_cda_modifications(cda);
}

#[test]
fn adamaro_from_card_db_parses_opponent_max_hand_size_extremum() {
    let Some(db) = load_db() else {
        return;
    };

    let face = db
        .get_face_by_name(ADAMARO)
        .expect("Adamaro, First to Desire in card-data fixture");

    assert!(
        !face
            .abilities
            .iter()
            .any(|a| { matches!(a.effect.as_ref(), Effect::Unimplemented { .. }) }),
        "Adamaro card-data must not contain Unimplemented ability effects"
    );

    let cda = face
        .static_abilities
        .iter()
        .find(|s| s.characteristic_defining)
        .expect("Adamaro card-data must include a CDA static");

    assert_adamaro_cda_modifications(cda);
}
