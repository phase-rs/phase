//! Reach of the "you control a/an <prime|even|odd> number of <type>" gate
//! beyond Zimone's trigger.
//!
//! CR 603.4 + CR 608.2h: wherever the shared condition grammar is consumed, the
//! gate either lowers to a real conditional check or the line stays an honest
//! coverage gap. It must never yield an unconditional ability, replacement, or
//! restriction.
//!
//! - Effect level (spell "if", delayed-trigger intervening-if, a trigger body's
//!   "then if") lowers to `AbilityCondition::QuantityQuality`.
//! - Replacement (CR 614.1c) and activation/casting restriction surfaces have no
//!   quality mirror: they must fail closed. Each hostile face is paired with a
//!   verbatim real card on the same route proving the route is reached.

use std::ops::ControlFlow;

use engine::game::coverage::card_face_gaps;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, ActivationRestriction, CastingRestriction, ControllerRef,
    Effect, FilterProp, NumberQuality, Parity, QuantityExpr, QuantityRef, TargetFilter,
    TypedFilter,
};
use engine::types::ability_visit::visit_ability_def;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::zones::Zone;
use serde_json::json;

fn face(name: &str, oracle: &str, core: CoreType, subtypes: &[&str]) -> CardFace {
    let types = vec![format!("{core:?}")];
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &subtypes);
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            core_types: vec![core],
            subtypes,
            ..Default::default()
        },
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        casting_restrictions: parsed.casting_restrictions,
        ..Default::default()
    }
}

fn lands_you(quality: NumberQuality) -> AbilityCondition {
    AbilityCondition::QuantityQuality {
        quantity: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(
                    TypedFilter::land()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::InZone {
                            zone: Zone::Battlefield,
                        }]),
                ),
            },
        },
        quality,
    }
}

/// Reach-guard: every clause lowered (no `Unimplemented` anywhere in the tree).
fn assert_fully_lowered(def: &AbilityDefinition) {
    let _ = visit_ability_def(def, &mut |effect| {
        assert!(
            !matches!(effect, Effect::Unimplemented { .. }),
            "{effect:?}"
        );
        ControlFlow::Continue(())
    });
}

/// Every node of a definition chain (`sub_ability` / `else_ability`).
fn chain(def: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    let mut nodes = vec![def];
    let mut i = 0;
    while i < nodes.len() {
        let node = nodes[i];
        nodes.extend(node.sub_ability.as_deref());
        nodes.extend(node.else_ability.as_deref());
        i += 1;
    }
    nodes
}

/// (a) A spell's leading "if" gates the spell's effect.
#[test]
fn spell_if_prime_lands_is_conditional() {
    let parsed = parse_oracle_text(
        "If you control a prime number of lands, draw a card.",
        "Test Prime Draw",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let ability = parsed.abilities.first().expect("spell ability");
    assert_fully_lowered(ability);
    assert!(
        matches!(*ability.effect, Effect::Draw { .. }),
        "{ability:?}"
    );
    assert_eq!(ability.condition, Some(lands_you(NumberQuality::Prime)));
}

/// (b) CR 603.4 + CR 603.7a: a delayed trigger's intervening-if rides its body
/// (Fight for the Throne's shape with a prime gate) — never an unconditional
/// monarch grant.
#[test]
fn delayed_trigger_prime_gate_is_conditional() {
    let parsed = parse_oracle_text(
        "Put a +1/+1 counter on target creature you control. Then it fights target creature an opponent controls. When the creature an opponent controls dies this turn, if you control a prime number of lands, you become the monarch.",
        "Test Prime Throne",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let ability = parsed.abilities.first().expect("spell ability");
    assert_fully_lowered(ability);
    let mut delayed_body = None;
    let _ = visit_ability_def(ability, &mut |effect| {
        if let Effect::CreateDelayedTrigger { effect, .. } = effect {
            delayed_body = Some((**effect).clone());
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    });
    let body = delayed_body.expect("the third sentence lowers to a CreateDelayedTrigger");
    assert!(
        matches!(*body.effect, Effect::BecomeMonarch { .. }),
        "{body:?}"
    );
    assert_eq!(body.condition, Some(lands_you(NumberQuality::Prime)));
}

/// (c) A trigger body's "then if" sub-clause gates only that sub-clause.
#[test]
fn trigger_body_then_if_odd_lands_is_conditional() {
    let parsed = parse_oracle_text(
        "At the beginning of your upkeep, put a +1/+1 counter on this creature. Then if you control an odd number of lands, draw a card.",
        "Test Odd Upkeep",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed.triggers.first().expect("upkeep trigger");
    let execute = trigger.execute.as_deref().expect("trigger body");
    assert_fully_lowered(execute);
    let draw = chain(execute)
        .into_iter()
        .find(|node| matches!(*node.effect, Effect::Draw { .. }))
        .expect("the draw clause");
    assert_eq!(
        draw.condition,
        Some(lands_you(NumberQuality::Parity(Parity::Odd)))
    );
}

/// No replacement on the face is gate-less and no restriction carries an
/// unparsed (`None`) condition.
fn assert_no_gate_less_definition(face: &CardFace) {
    for replacement in &face.replacements {
        assert!(
            replacement.condition.is_some(),
            "{}: gate-less replacement {replacement:?}",
            face.name
        );
    }
    for ability in &face.abilities {
        for restriction in &ability.activation_restrictions {
            assert!(
                !matches!(
                    restriction,
                    ActivationRestriction::RequiresCondition { condition: None }
                ),
                "{}: {restriction:?}",
                face.name
            );
        }
    }
    for restriction in &face.casting_restrictions {
        assert!(
            !matches!(
                restriction,
                CastingRestriction::RequiresCondition { condition: None }
            ),
            "{}: {restriction:?}",
            face.name
        );
    }
}

/// Hostile: the quality gate on a surface that has no quality mirror.
fn assert_fails_closed(face: &CardFace) {
    let gaps = card_face_gaps(face);
    assert!(
        !gaps.is_empty(),
        "{}: an unmirrored quality gate must stay a coverage gap",
        face.name
    );
    assert_no_gate_less_definition(face);
}

fn replacement_conditions(face: &CardFace) -> Vec<serde_json::Value> {
    face.replacements
        .iter()
        .map(|r| serde_json::to_value(&r.condition).expect("condition serializes"))
        .collect()
}

/// CR 614.1c: "enters with … if <gate>" (route: only-if suffix).
#[test]
fn enters_with_only_if_quality_gate_fails_closed() {
    // Reach-guard: Frontier Mastodon, verbatim, on the same route.
    let mastodon = face(
        "Frontier Mastodon",
        "Ferocious — This creature enters with a +1/+1 counter on it if you control a creature with power 4 or greater.",
        CoreType::Creature,
        &["Elephant"],
    );
    assert!(card_face_gaps(&mastodon).is_empty());
    assert_eq!(
        replacement_conditions(&mastodon),
        vec![
            json!({"type":"OnlyIfQuantity","lhs":{"type":"Ref","qty":{"type":"ObjectCount","filter":{"type":"Typed","type_filters":["Creature"],"controller":"You","properties":[{"type":"PtComparison","stat":"Power","scope":"Current","comparator":"GE","value":{"type":"Fixed","value":4}},{"type":"InZone","zone":"Battlefield"}]}}},"comparator":"GE","rhs":{"type":"Fixed","value":1}})
        ]
    );
    assert_fails_closed(&face(
        "Test Prime Entry",
        "This creature enters with a +1/+1 counter on it if you control a prime number of lands.",
        CoreType::Creature,
        &[],
    ));
}

/// CR 614.1c: "enters with … unless <gate>" (route: unless suffix).
#[test]
fn enters_with_unless_quality_gate_fails_closed() {
    let exemplar = face(
        "Steel Exemplar",
        "Trample\nThis creature enters with two +1/+1 counters on it unless two or more colors of mana were spent to cast it.",
        CoreType::Creature,
        &["Wizard"],
    );
    assert_eq!(
        replacement_conditions(&exemplar),
        vec![
            json!({"type":"UnlessQuantity","lhs":{"type":"Ref","qty":{"type":"ManaSpentToCast","scope":"SelfObject","metric":{"type":"DistinctColors"}}},"comparator":"GE","rhs":{"type":"Fixed","value":2}})
        ]
    );
    assert_fails_closed(&face(
        "Test Even Entry",
        "This creature enters with two +1/+1 counters on it unless you control an even number of lands.",
        CoreType::Creature,
        &[],
    ));
}

/// CR 614.1c: "If <gate>, ~ enters with …" (route: leading conditional splitter).
#[test]
fn leading_if_enters_with_quality_gate_fails_closed() {
    let paladin = face(
        "Ardenvale Paladin",
        "Adamant — If at least three white mana was spent to cast this spell, this creature enters with a +1/+1 counter on it.",
        CoreType::Creature,
        &["Human", "Knight"],
    );
    assert_eq!(
        replacement_conditions(&paladin),
        vec![
            json!({"type":"OnlyIfQuantity","lhs":{"type":"Ref","qty":{"type":"ManaSpentToCast","scope":"SelfObject","metric":{"type":"OfColor","color":"White"}}},"comparator":"GE","rhs":{"type":"Fixed","value":3}})
        ]
    );
    assert_fails_closed(&face(
        "Test Odd Entry",
        "If you control an odd number of lands, this creature enters with a +1/+1 counter on it.",
        CoreType::Creature,
        &[],
    ));
}

/// CR 614.1c: "As long as <gate>, other permanents enter tapped" (route:
/// source-state external entry).
#[test]
fn as_long_as_external_entry_quality_gate_fails_closed() {
    let archelos = face(
        "Archelos, Lagoon Mystic",
        "As long as Archelos is tapped, other permanents enter tapped.\nAs long as Archelos is untapped, other permanents enter untapped.",
        CoreType::Creature,
        &["Turtle", "Shaman"],
    );
    assert_eq!(
        replacement_conditions(&archelos),
        vec![
            json!({"type":"SourceTappedState","tapped":true}),
            json!({"type":"SourceTappedState","tapped":false}),
        ]
    );
    assert_fails_closed(&face(
        "Test Prime Tapper",
        "As long as you control a prime number of lands, other permanents enter tapped.",
        CoreType::Creature,
        &[],
    ));
}

/// CR 602.5: "Activate only if <gate>" (route: restriction bridge).
#[test]
fn activation_restriction_quality_gate_fails_closed() {
    let caves = face(
        "Cryptic Caves",
        "{T}: Add {C}.\n{1}, {T}, Sacrifice this land: Draw a card. Activate only if you control five or more lands.",
        CoreType::Land,
        &[],
    );
    let restrictions: Vec<serde_json::Value> = caves
        .abilities
        .iter()
        .flat_map(|a| a.activation_restrictions.iter())
        .map(|r| serde_json::to_value(r).expect("restriction serializes"))
        .collect();
    assert_eq!(
        restrictions,
        vec![
            json!({"type":"RequiresCondition","data":{"condition":{"type":"QuantityComparison","lhs":{"type":"Ref","qty":{"type":"ObjectCount","filter":{"type":"Typed","type_filters":["Land"],"controller":"You","properties":[{"type":"InZone","zone":"Battlefield"}]}}},"comparator":"GE","rhs":{"type":"Fixed","value":5}}}})
        ]
    );
    assert_fails_closed(&face(
        "Test Even Caves",
        "{1}, {T}, Sacrifice this land: Draw a card. Activate only if you control an even number of lands.",
        CoreType::Land,
        &[],
    ));
}

/// CR 601.3: "Cast this spell only if <gate>" (route: casting restriction).
#[test]
fn casting_restriction_quality_gate_fails_closed() {
    let feast = face(
        "Feast of Blood",
        "Cast this spell only if you control two or more Vampires.\nDestroy target creature. You gain 4 life.",
        CoreType::Sorcery,
        &[],
    );
    let restrictions: Vec<serde_json::Value> = feast
        .casting_restrictions
        .iter()
        .map(|r| serde_json::to_value(r).expect("restriction serializes"))
        .collect();
    assert_eq!(
        restrictions,
        vec![
            json!({"type":"RequiresCondition","data":{"condition":{"type":"QuantityComparison","lhs":{"type":"Ref","qty":{"type":"ObjectCount","filter":{"type":"Typed","type_filters":[{"Subtype":"Vampire"}],"controller":"You","properties":[{"type":"InZone","zone":"Battlefield"}]}}},"comparator":"GE","rhs":{"type":"Fixed","value":2}}}})
        ]
    );
    assert_fails_closed(&face(
        "Test Prime Feast",
        "Cast this spell only if you control a prime number of lands.\nDestroy target creature. You gain 4 life.",
        CoreType::Sorcery,
        &[],
    ));
}

/// Whether any ability on the face carries an `Unimplemented` named `name`
/// (`None`: any name).
fn has_unimplemented(face: &CardFace, name: Option<&str>) -> bool {
    face.abilities.iter().any(|ability| {
        visit_ability_def(ability, &mut |effect| match effect {
            Effect::Unimplemented { name: found, .. }
                if name.is_none_or(|wanted| found == wanted) =>
            {
                ControlFlow::Break(())
            }
            _ => ControlFlow::Continue(()),
        })
        .is_break()
    })
}

/// CR 614.1d: "As long as <gate>, this creature enters tapped" (route: the
/// singular unconditional enters-tapped fallback). The shared quality parser
/// admits the gate, the specialized replacement conversion declines it, and the
/// fallback must decline too, so the line lands as an explicit gap instead of
/// an unconditional self replacement.
#[test]
fn as_long_as_singular_enters_tapped_quality_gate_fails_closed() {
    // Reach-guard: Diregraf Ghoul, verbatim, takes the same fallback and yields
    // its unconditional self replacement.
    let ghoul = face(
        "Diregraf Ghoul",
        "This creature enters tapped.",
        CoreType::Creature,
        &["Zombie"],
    );
    assert_eq!(ghoul.replacements.len(), 1);
    let replacement = &ghoul.replacements[0];
    assert!(replacement.condition.is_none());
    assert!(matches!(
        replacement
            .execute
            .as_deref()
            .map(|def| def.effect.as_ref()),
        Some(Effect::SetTapState {
            target: TargetFilter::SelfRef,
            ..
        })
    ));
    assert!(card_face_gaps(&ghoul).is_empty());

    // Leading form: the router's `as_long_as_gate` diagnostic.
    let leading = face(
        "Test Odd Tapper",
        "As long as you control an odd number of creatures, this creature enters tapped.",
        CoreType::Creature,
        &[],
    );
    assert_fails_closed(&leading);
    assert!(leading.replacements.is_empty());
    assert!(has_unimplemented(&leading, Some("as_long_as_gate")));

    // Trailing form: the router's replacement-structure gap.
    let trailing = face(
        "Test Odd Trailing Tapper",
        "This creature enters tapped as long as you control an odd number of creatures.",
        CoreType::Creature,
        &[],
    );
    assert_fails_closed(&trailing);
    assert!(trailing.replacements.is_empty());
    assert!(has_unimplemented(&trailing, None));
}

/// CR 614.1d: a quoted perpetual grant ("This permanent enters the battlefield
/// tapped.") inside a "for as long as" sentence is not the spell's own self
/// replacement.
#[test]
fn divine_purge_quoted_enters_tapped_is_not_a_self_replacement() {
    let purge = face(
        "Divine Purge",
        "Exile all artifacts and creatures with mana value 3 or less. They perpetually gain \"This spell costs {2} more to cast\" and \"This permanent enters the battlefield tapped.\" For as long as each of them remain exiled, its owner may play it.",
        CoreType::Sorcery,
        &[],
    );
    assert!(purge.replacements.is_empty());
    // Reach-guard: the spell's exile instruction is parsed.
    assert!(matches!(
        purge.abilities.first().map(|def| def.effect.as_ref()),
        Some(Effect::ChangeZoneAll {
            destination: Zone::Exile,
            ..
        })
    ));
    // The perpetual grant and the play permission stay unsupported: the card
    // remains an honest coverage gap.
    assert!(
        !card_face_gaps(&purge).is_empty(),
        "Divine Purge must stay a coverage gap"
    );
    assert!(has_unimplemented(&purge, None));
}
