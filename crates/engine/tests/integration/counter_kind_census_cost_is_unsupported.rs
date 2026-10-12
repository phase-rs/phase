//! CR 122.1 + CR 601.2f: a cost whose "for each" multiplier is a counter-kind
//! census the cost route cannot bind — the census of a pronoun ("kind of
//! counter on it/them"), its "different" and "of the kinds of" forms, or a
//! census of a targeted object — is reported UNSUPPORTED through the public
//! coverage authority (`card_face_gaps`). It never becomes a cost that counts
//! every object (`ObjectCount` over an empty type filter), a flat cost with the
//! multiplier dropped, or a reduction claimed as supported. Synthetic class
//! cards (no printed card yet); each is paired with a control that the same
//! route still reads a census it can bind.

use engine::game::coverage::card_face_gaps;
use engine::parser::parse_oracle_text;
use engine::types::ability::{AbilityCost, QuantityRef, StaticDefinition, TargetFilter};
use engine::types::card::CardFace;
use engine::types::statics::StaticMode;

fn face(name: &str, oracle: &str, keywords: &[&str]) -> CardFace {
    let keywords: Vec<String> = keywords.iter().map(|k| (*k).to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &keywords, &["Creature".to_string()], &[]);
    CardFace {
        name: name.to_string(),
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        keywords: parsed.extracted_keywords,
        ..Default::default()
    }
}

/// The cost-modification statics on the face, with their dynamic counts.
fn cost_counts(face: &CardFace) -> Vec<Option<QuantityRef>> {
    face.static_abilities
        .iter()
        .filter_map(|def: &StaticDefinition| match &def.mode {
            StaticMode::ModifyCost { dynamic_count, .. } => Some(dynamic_count.clone()),
            _ => None,
        })
        .collect()
}

/// CR 601.2f: a spell-cost (or opponents'-spell-cost) modification scaled by an
/// unbound census is unsupported and carries no cost modification at all —
/// in particular none counting every object.
#[test]
fn spell_cost_scaled_by_an_unbound_counter_kind_census_is_unsupported() {
    // Control: the census over a named population reads on the same route.
    let control = face(
        "Census Control",
        "This spell costs {1} less to cast for each kind of counter among permanents you \
         control.",
        &[],
    );
    assert!(
        matches!(
            cost_counts(&control).as_slice(),
            [Some(QuantityRef::DistinctCounterKindsAmong {
                filter: TargetFilter::Typed(_)
            })]
        ),
        "reach guard: {:?}",
        control.static_abilities
    );
    assert_eq!(card_face_gaps(&control), Vec::<String>::new());

    for text in [
        "This spell costs {1} less to cast for each kind of counter on it.",
        "This spell costs {1} less to cast for each kind of counter on them.",
        "This spell costs {1} less to cast for each different kind of counter on it.",
        "This spell costs {1} less to cast for each of the kinds of counters on it.",
        "This spell costs {1} less to cast for each kind of counter on this creature.",
        "This spell costs {1} less to cast for each kind of counter on target creature.",
        "Spells your opponents cast cost {1} more to cast for each kind of counter on it.",
        "Creature spells you cast cost {1} less to cast for each kind of counter on it.",
    ] {
        let face = face("Census Cost", text, &[]);
        let counts = cost_counts(&face);
        assert!(
            counts.is_empty(),
            "{text:?}: no cost modification may be claimed, got {counts:?}"
        );
        let gaps = card_face_gaps(&face);
        assert!(!gaps.is_empty(), "{text:?}: the card must be unsupported");
    }
}

/// CR 119.4 + CR 601.2f: "Pay N life for each <unbound census>" is an
/// unimplemented cost — never the flat "Pay N life" — and the card is
/// unsupported; a readable multiplier (the shape of Hand of Vecna's equip
/// cost) is the control.
#[test]
fn pay_life_scaled_by_an_unbound_counter_kind_census_is_unsupported() {
    let control = face(
        "Census Life Control",
        "{T}, Pay 1 life for each card in your hand: Draw a card.",
        &[],
    );
    let control_cost = control.abilities[0].cost.clone();
    assert!(
        matches!(
            control_cost,
            Some(AbilityCost::Composite { ref costs })
                if costs.iter().any(|c| matches!(c, AbilityCost::PayLife { .. }))
        ),
        "reach guard: {control_cost:?}"
    );
    assert_eq!(card_face_gaps(&control), Vec::<String>::new());

    let census = face(
        "Census Life",
        "{T}, Pay 1 life for each kind of counter on it: Draw a card.",
        &[],
    );
    let cost = census.abilities[0].cost.clone();
    assert!(
        matches!(
            cost,
            Some(AbilityCost::Composite { ref costs })
                if costs.iter().any(|c| matches!(c, AbilityCost::Unimplemented { .. }))
                    && !costs.iter().any(|c| matches!(c, AbilityCost::PayLife { .. }))
        ),
        "the census multiplier must not collapse to a flat life payment: {cost:?}"
    );
    assert!(!card_face_gaps(&census).is_empty());
}

/// CR 702.168d + CR 122.1: a disguise turn-face-up reduction scaled by an
/// unbound census is unsupported, never a reduction-less disguise claimed as
/// supported.
#[test]
fn disguise_reduction_scaled_by_an_unbound_counter_kind_census_is_unsupported() {
    for text in [
        "Disguise {2}{G}. This cost is reduced by {1} for each kind of counter on it.",
        "Disguise {2}{G}. This cost is reduced by {1} for each different kind of counter on it.",
    ] {
        let face = face("Census Disguise", text, &["Disguise"]);
        assert!(!card_face_gaps(&face).is_empty(), "{text:?}");
    }
}
