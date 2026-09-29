//! CR 603.12 + CR 701.20a: "When you reveal a <filter> card this way, <body>"
//! reflexive gate after a reveal-until (Yuna's Whistle, Calibrated Blast).

use super::lower::parse_where_x_quantity_expression;
use super::*;
use crate::parser::oracle_target::parse_type_phrase_folding;
use crate::parser::parse_oracle_text;

const YUNAS_WHISTLE: &str = "Reveal cards from the top of your library until you reveal a creature card. Put that card into your hand and the rest on the bottom of your library in a random order. When you reveal a creature card this way, put X +1/+1 counters on target creature you control, where X is the mana value of that card.";
const CALIBRATED_BLAST: &str = "Reveal cards from the top of your library until you reveal a nonland card. Put the revealed cards on the bottom of your library in a random order. When you reveal a nonland card this way, Calibrated Blast deals damage equal to that card's mana value to any target.";
const REVEAL_UNTIL_CREATURE_HEAD: &str = "Reveal cards from the top of your library until you reveal a creature card. Put that card into your hand and the rest on the bottom of your library in a random order. ";

fn spell(text: &str, name: &str) -> AbilityDefinition {
    let parsed = parse_oracle_text(text, name, &[], &["Instant".to_string()], &[]);
    assert_eq!(parsed.abilities.len(), 1, "{name}: {:?}", parsed.abilities);
    parsed.abilities[0].clone()
}

fn reveal_until_filter(def: &AbilityDefinition) -> TargetFilter {
    let Effect::RevealUntil { filter, .. } = def.effect.as_ref() else {
        panic!("expected a RevealUntil root, got {:?}", def.effect);
    };
    filter.clone()
}

/// G is produced by the parent's own until-filter producer, over the gate's
/// filter phrase with its article removed.
fn gate_filter(filter_phrase: &str) -> TargetFilter {
    let (_, filter_text) = parse_reveal_until_active_filter_text(filter_phrase).unwrap();
    build_reveal_until_filter(filter_text)
}

fn creature() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature())
}

fn nonland() -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::default().with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
    )
}

fn gate_sub(def: &AbilityDefinition) -> &AbilityDefinition {
    def.sub_ability
        .as_deref()
        .expect("the gated body must be the reveal-until's sub-ability")
}

#[test]
fn gate_and_parent_filters_come_from_one_producer_for_both_verbatim_cards() {
    for (text, name, phrase, expected) in [
        (YUNAS_WHISTLE, "Yuna's Whistle", "creature card", creature()),
        (
            CALIBRATED_BLAST,
            "Calibrated Blast",
            "nonland card",
            nonland(),
        ),
    ] {
        let def = spell(text, name);
        let f = reveal_until_filter(&def);
        let g = gate_filter(phrase);
        // Read both BEFORE the equality the gate relies on.
        assert_eq!(f, expected, "{name}: parent until-filter F");
        assert_eq!(g, expected, "{name}: gate filter G");
        assert_eq!(
            gate_sub(&def).condition,
            Some(AbilityCondition::WhenYouDo),
            "{name}: the reveal gate must lower to the CR 603.12 creation gate"
        );
    }
    // Why the shared producer matters: the generic type-phrase reader keeps
    // the informational "card" noun for "nonland card", so a gate built from
    // it would never equal Calibrated Blast's parent filter.
    let (generic, _) = parse_type_phrase_folding("nonland card this way");
    assert_ne!(generic, nonland());
}

#[test]
fn yunas_whistle_lowers_to_a_reflexive_counter_placement_on_the_revealed_mana_value() {
    let def = spell(YUNAS_WHISTLE, "Yuna's Whistle");
    let sub = gate_sub(&def);
    assert_eq!(sub.condition, Some(AbilityCondition::WhenYouDo));
    let Effect::PutCounter {
        counter_type,
        count,
        target,
    } = sub.effect.as_ref()
    else {
        panic!("expected PutCounter, got {:?}", sub.effect);
    };
    assert_eq!(
        *counter_type,
        crate::types::counter::CounterType::Plus1Plus1
    );
    assert_eq!(
        *count,
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue {
                scope: ObjectScope::Demonstrative,
            },
        }
    );
    assert_eq!(
        *target,
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
    );
    assert!(
        !format!("{def:?}").contains("Unimplemented"),
        "no clause of Yuna's Whistle may stay unimplemented: {def:?}"
    );
}

#[test]
fn calibrated_blast_keeps_its_reveal_gate_as_a_reflexive_trigger() {
    let def = spell(CALIBRATED_BLAST, "Calibrated Blast");
    let sub = gate_sub(&def);
    assert!(matches!(sub.effect.as_ref(), Effect::DealDamage { .. }));
    assert_eq!(sub.condition, Some(AbilityCondition::WhenYouDo));
}

fn gate_outcome(text: &str) -> AbilityDefinition {
    spell(text, "Probe")
}

fn is_reveal_gate_gap(def: &AbilityDefinition) -> bool {
    def.sub_ability.as_deref().is_some_and(|sub| {
        matches!(
            sub.effect.as_ref(),
            Effect::Unimplemented { name, .. } if name == "reveal_this_way_reflexive_gate"
        )
    })
}

#[test]
fn reveal_gate_fails_closed_unless_it_restates_the_parent_until_condition() {
    // Positive reach guard: the matching gate lowers to WhenYouDo.
    let control = gate_outcome(&format!(
        "{REVEAL_UNTIL_CREATURE_HEAD}When you reveal a creature card this way, you gain 2 life."
    ));
    assert_eq!(
        gate_sub(&control).condition,
        Some(AbilityCondition::WhenYouDo)
    );
    assert!(!is_reveal_gate_gap(&control));

    // Filter mismatch.
    let mismatch = gate_outcome(&format!(
        "{REVEAL_UNTIL_CREATURE_HEAD}When you reveal a land card this way, you gain 2 life."
    ));
    assert!(is_reveal_gate_gap(&mismatch), "{mismatch:?}");

    // Plural quantifier: not the single until-hit event.
    let plural = gate_outcome(&format!(
        "{REVEAL_UNTIL_CREATURE_HEAD}When you reveal one or more creature cards this way, you gain 2 life."
    ));
    assert!(is_reveal_gate_gap(&plural), "{plural:?}");

    // Non-reveal-until parent.
    let reveal_top = gate_outcome(
        "Reveal the top card of your library. When you reveal a creature card this way, you gain 2 life.",
    );
    assert!(
        !matches!(reveal_top.effect.as_ref(), Effect::RevealUntil { .. }),
        "reach guard: this fixture's parent must not be a reveal-until"
    );
    assert!(is_reveal_gate_gap(&reveal_top), "{reveal_top:?}");

    // Multi-match parent (count 2).
    let two = gate_outcome(
        "Reveal cards from the top of your library until you reveal two creature cards. Put those cards into your hand and the rest on the bottom of your library in a random order. When you reveal a creature card this way, you gain 2 life.",
    );
    assert!(
        matches!(
            two.effect.as_ref(),
            Effect::RevealUntil {
                count: QuantityExpr::Fixed { value: 2 },
                ..
            }
        ),
        "reach guard: this fixture's parent must be a count-2 reveal-until, got {:?}",
        two.effect
    );
    assert!(is_reveal_gate_gap(&two), "{two:?}");
}

#[test]
fn where_x_mana_value_of_that_card_binds_the_demonstrative_referent() {
    let demonstrative = Some(QuantityExpr::Ref {
        qty: QuantityRef::ObjectManaValue {
            scope: ObjectScope::Demonstrative,
        },
    });
    for phrase in [
        "the mana value of that card",
        "the converted mana cost of that card",
        "the mana value of that card.",
    ] {
        assert_eq!(
            parse_where_x_quantity_expression(phrase),
            demonstrative,
            "{phrase}"
        );
    }
    // Control: the possessive twin keeps its existing binding.
    assert_eq!(
        parse_where_x_quantity_expression("that card's mana value"),
        demonstrative
    );
}
