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

/// The reveal gate's lowered condition: the CR 603.12 creation gate plus the
/// reveal-until-hit guard.
fn reveal_gate_condition() -> AbilityCondition {
    AbilityCondition::when_you_do_with_guard(AbilityCondition::EffectOutcome {
        signal: EffectOutcomeSignal::RevealUntilMatched,
    })
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
            Some(reveal_gate_condition()),
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
    assert_eq!(sub.condition, Some(reveal_gate_condition()));
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
    assert_eq!(sub.condition, Some(reveal_gate_condition()));
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
    assert_eq!(gate_sub(&control).condition, Some(reveal_gate_condition()));
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

const CAIT_SITH_TRIGGER: &str = "Lucky Slots — At the beginning of combat on your turn, scry 1, then exile the top card of your library. You may play that card this turn. When you exile a card this way, target creature you control gets +X/+0 until end of turn, where X is that card's mana value.";

fn trigger_json(text: &str, name: &str) -> serde_json::Value {
    let parsed = parse_oracle_text(text, name, &[], &["Creature".to_string()], &[]);
    serde_json::to_value(&parsed.triggers).unwrap()
}

#[test]
fn unmodeled_zone_change_reflexive_with_a_where_x_referent_fails_closed() {
    // Cait Sith, Fortune Teller (verbatim): the where-X referent no longer
    // misbinds the pump to the exiled card, and because the "When you exile …
    // this way" reflexive's own target timing is not modeled, the clause is
    // honestly unsupported instead of announcing its target with the trigger.
    let json = trigger_json(CAIT_SITH_TRIGGER, "Cait Sith, Fortune Teller");
    let mut gaps = Vec::new();
    collect_effects(&json, "Unimplemented", &mut gaps);
    assert!(
        gaps.iter()
            .any(|gap| gap["name"] == "zone_change_reflexive_target_timing"),
        "{json}"
    );
    let mut pumps = Vec::new();
    collect_effects(&json, "Pump", &mut pumps);
    assert!(pumps.is_empty(), "no misbound pump survives: {json}");
}

#[test]
fn where_x_definition_referent_does_not_rebind_an_explicit_recipient() {
    // Adjacent control for the strict-fail: the same clause under an "If …
    // this way" gate (a resolution-time condition, no reflexive timing) keeps
    // its explicit recipient; the where-X still binds the exiled card.
    let json = trigger_json(
        "At the beginning of combat on your turn, exile the top card of your library. If you exiled a card this way, target creature you control gets +X/+0 until end of turn, where X is that card's mana value.",
        "Probe",
    );
    let mut pumps = Vec::new();
    collect_effects(&json, "Pump", &mut pumps);
    assert_eq!(pumps.len(), 1, "reach guard: exactly one pump: {json}");
    assert_eq!(
        pumps[0]["target"],
        serde_json::to_value(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You)
        ))
        .unwrap()
    );
    assert_eq!(
        pumps[0]["power"]["value"]["qty"]["scope"]["type"], "Demonstrative",
        "the where-X still binds the exiled card"
    );
}

fn collect_effects(value: &serde_json::Value, kind: &str, out: &mut Vec<serde_json::Value>) {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("type").and_then(|t| t.as_str()) == Some(kind) {
                out.push(value.clone());
            }
            for child in map.values() {
                collect_effects(child, kind, out);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_effects(child, kind, out);
            }
        }
        _ => {}
    }
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

fn where_x_gap_present(json: &serde_json::Value) -> bool {
    let mut gaps = Vec::new();
    collect_effects(json, "Unimplemented", &mut gaps);
    gaps.iter().any(|gap| gap["name"] == "where_x_binding")
}

#[test]
fn kami_war_o_kagachi_where_x_has_no_proven_producer() {
    // The Kami War // O-Kagachi Made Manifest (back face, verbatim): "that card"
    // is the defending player's graveyard choice, which is still unimplemented,
    // so its where-X must stay an honest gap rather than a self-referential pump.
    let json = trigger_json(
        "O-Kagachi Made Manifest is all colors.\nFlying, trample\nWhenever this creature attacks, defending player chooses a nonland card in your graveyard. Return that card to your hand. This creature gets +X/+0 until end of turn, where X is the mana value of that card.",
        "O-Kagachi Made Manifest",
    );
    assert!(where_x_gap_present(&json), "{json}");
    let mut pumps = Vec::new();
    collect_effects(&json, "Pump", &mut pumps);
    assert!(pumps.is_empty(), "no unfounded pump: {json}");
}

#[test]
fn where_x_that_card_after_a_non_card_producer_stays_unsupported() {
    let def = spell(
        "Gain 3 life. Target creature gets +X/+0 until end of turn, where X is the mana value of that card.",
        "Probe",
    );
    assert!(
        matches!(def.effect.as_ref(), Effect::GainLife { .. }),
        "reach guard: the life gain parses: {def:?}"
    );
    let json = serde_json::to_value(&def).unwrap();
    assert!(where_x_gap_present(&json), "{json}");
}

#[test]
fn where_x_that_card_after_a_multi_hit_reveal_until_stays_unsupported() {
    let def = spell(
        "Reveal cards from the top of your library until you reveal two creature cards. Put those cards into your hand and the rest on the bottom of your library in a random order. Target creature gets +X/+0 until end of turn, where X is the mana value of that card.",
        "Probe",
    );
    assert!(
        matches!(
            def.effect.as_ref(),
            Effect::RevealUntil {
                count: QuantityExpr::Fixed { value: 2 },
                ..
            }
        ),
        "reach guard: the parent is a count-2 reveal-until: {:?}",
        def.effect
    );
    let json = serde_json::to_value(&def).unwrap();
    assert!(where_x_gap_present(&json), "{json}");
}

const GOBLIN_CHARBELCHER: &str = "{3}, {T}: Reveal cards from the top of your library until you reveal a land card. ~ deals damage equal to the number of nonland cards revealed this way to any target. If the revealed land card was a Mountain, ~ deals double that damage instead. Put the revealed cards on the bottom of your library in any order.";

/// CR 701.20a + CR 608.2c: the whole revealed pile — the land included — goes to
/// the library bottom AFTER the damage (no card is kept for a hand), the damage counts the
/// nonland cards of the revealed set, and the Mountain override replaces that
/// damage with a doubled copy of the same count.
#[test]
fn goblin_charbelcher_lowers_to_typed_damage_override_and_whole_pile_bottom() {
    let parsed = parse_oracle_text(
        GOBLIN_CHARBELCHER,
        "Goblin Charbelcher",
        &[],
        &["Artifact".to_string()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1, "{:?}", parsed.abilities);
    let reveal = &parsed.abilities[0];
    // CR 608.2c: the reveal only reveals; the cards stay in the library until the
    // placement instruction, which follows the damage.
    let Effect::RevealUntil {
        matched_disposition,
        ..
    } = reveal.effect.as_ref()
    else {
        panic!("expected a RevealUntil root, got {:?}", reveal.effect);
    };
    assert_eq!(*matched_disposition, RevealUntilDisposition::RevealOnly);

    let damage = gate_sub(reveal);
    let nonland_revealed = QuantityExpr::Ref {
        qty: QuantityRef::FilteredTrackedSetSize {
            filter: Box::new(nonland_card()),
            caused_by: None,
        },
    };
    let Effect::DealDamage { amount, target, .. } = damage.effect.as_ref() else {
        panic!("expected DealDamage, got {:?}", damage.effect);
    };
    assert_eq!(amount, &nonland_revealed);
    assert_eq!(target, &TargetFilter::Any);

    let doubled = gate_sub(damage);
    assert_eq!(
        doubled.effect.as_ref(),
        &Effect::DealDamage {
            amount: QuantityExpr::Multiply {
                factor: 2,
                inner: Box::new(nonland_revealed),
            },
            target: TargetFilter::ParentTarget,
            damage_source: None,
            excess: None,
        }
    );
    assert_eq!(
        doubled.condition,
        Some(AbilityCondition::ConditionInstead {
            inner: Box::new(AbilityCondition::RevealedHasCardType {
                card_types: vec![CoreType::Land],
                additional_filter: None,
                subtype_filter: Some(Box::new(TargetFilter::Typed(
                    TypedFilter::default().subtype("Mountain".to_string())
                ))),
            }),
        })
    );
    let placement = gate_sub(doubled);
    assert_eq!(placement.sub_link, SubAbilityLink::SequentialSibling);
    let Effect::ChangeZoneAll {
        origin,
        destination,
        target,
        library_position,
        random_order,
        ..
    } = placement.effect.as_ref()
    else {
        panic!("expected the pile placement, got {:?}", placement.effect);
    };
    assert_eq!(*origin, Some(Zone::Library));
    assert_eq!(*destination, Zone::Library);
    assert_eq!(*target, TargetFilter::LastRevealed);
    assert_eq!(*library_position, Some(LibraryPosition::Bottom));
    assert!(!random_order, "\"in any order\" is the owner's arrangement");
}

fn nonland_card() -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::default()
            .with_type(TypeFilter::Card)
            .with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
    )
}
