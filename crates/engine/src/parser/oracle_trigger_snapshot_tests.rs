use crate::parser::oracle_nom::quantity::parse_quantity_ref;
use crate::types::ability::AbilityCondition;

use super::*;

fn parse_trigger_line(text: &str, card_name: &str) -> TriggerDefinition {
    parse_trigger_line_with_index(text, card_name, None, &mut ParseContext::default())
}

#[test]
fn trigger_simple_etb_self() {
    let def = parse_trigger_line(
        "When Test Card enters the battlefield, draw a card.",
        "Test Card",
    );
    insta::assert_json_snapshot!(def);
}

/// CR 208.3 + CR 608.2k (issue #2009): "that spell's power/toughness" in a
/// `Whenever you cast a [creature] spell` trigger must gate the chained
/// sub-effects on the triggering spell's power. Before the fix the
/// `"that spell's power"` quantity ref was unrecognized, so
/// `parse_condition_text` returned `None` and `strip_leading_general_conditional`
/// silently dropped each `If that spell's power is N or greater, …` head —
/// the draw and damage fired on every creature spell regardless of power.
/// Asserts each chained sub-ability carries the right
/// `QuantityCheck { Power(CostPaidObject) GE N }` gate (Eshki, Temur's Roar).
#[test]
fn eshki_that_spell_power_gates_chained_effects() {
    let power_ge = |n: i32| AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::CostPaidObject,
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: n },
    };

    let def = parse_trigger_line(
            "Whenever you cast a creature spell, put a +1/+1 counter on Eshki. If that spell's power is 4 or greater, draw a card. If that spell's power is 6 or greater, Eshki deals damage equal to Eshki's power to each opponent.",
            "Eshki, Temur's Roar",
        );

    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));

    let execute = def.execute.as_ref().expect("execute ability present");
    // First instruction (put a +1/+1 counter) is unconditional.
    assert_eq!(execute.condition, None);

    // "If that spell's power is 4 or greater, draw a card."
    let draw = execute
        .sub_ability
        .as_ref()
        .expect("draw sub-ability present");
    assert_eq!(draw.condition.as_ref(), Some(&power_ge(4)));

    // "If that spell's power is 6 or greater, Eshki deals damage …"
    let damage = draw
        .sub_ability
        .as_ref()
        .expect("damage sub-ability present");
    assert_eq!(damage.condition.as_ref(), Some(&power_ge(6)));
}

/// CR 208.3 + CR 608.2k (issue #2009): building-block coverage — the bare
/// "that spell's power"/"toughness" quantity refs resolve through the same
/// `parse_quantity_ref` path as "that creature's power" and "that spell's
/// mana value", scoped to the trigger-condition referent (CostPaidObject).
#[test]
fn that_spell_power_toughness_quantity_refs() {
    assert_eq!(
        parse_quantity_ref("that spell's power"),
        Ok((
            "",
            QuantityRef::Power {
                scope: ObjectScope::CostPaidObject,
            }
        ))
    );
    assert_eq!(
        parse_quantity_ref("that spell's toughness"),
        Ok((
            "",
            QuantityRef::Toughness {
                scope: ObjectScope::CostPaidObject,
            }
        ))
    );
}

/// CR 608.2c (issue #1584): Kathril's "Repeat this process for <10 keywords>"
/// must replicate the conditional keyword-counter placement once per keyword
/// — each placing that keyword's counter gated on a creature card in the
/// graveyard with the SAME keyword — instead of silently dropping the list.
#[test]
fn kathril_repeats_keyword_counters_across_graveyard_keywords() {
    use crate::types::ability::{Effect, FilterProp, QuantityExpr, QuantityRef, TargetFilter};
    use crate::types::counter::CounterType;
    use crate::types::keywords::{Keyword, KeywordKind};

    fn condition_keyword(cond: &AbilityCondition) -> Option<Keyword> {
        let AbilityCondition::QuantityCheck { lhs, .. } = cond else {
            return None;
        };
        let QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } = lhs
        else {
            return None;
        };
        let TargetFilter::Typed(typed) = filter else {
            return None;
        };
        typed.properties.iter().find_map(|prop| match prop {
            FilterProp::WithKeyword { value } => Some(value.clone()),
            _ => None,
        })
    }

    let def = parse_trigger_line(
        "When Kathril enters, put a flying counter on any creature you control \
             if a creature card in your graveyard has flying. Repeat this process \
             for first strike, double strike, deathtouch, hexproof, indestructible, \
             lifelink, menace, reach, trample, and vigilance. Then put a +1/+1 \
             counter on Kathril for each counter put on a creature this way.",
        "Kathril, Aspect Warper",
    );

    // Walk the execute chain, collecting each keyword counter with the
    // keyword its graveyard gate checks.
    let mut node = def.execute.as_deref();
    let mut keyword_counters: Vec<(KeywordKind, KeywordKind)> = Vec::new();
    let mut saw_p1p1 = false;
    while let Some(d) = node {
        if let Effect::PutCounter { counter_type, .. } = &*d.effect {
            match counter_type {
                CounterType::Keyword(kind) => {
                    let cond_kw = d
                        .condition
                        .as_ref()
                        .and_then(condition_keyword)
                        .expect("keyword counter keeps its graveyard-keyword gate");
                    keyword_counters.push((*kind, cond_kw.kind()));
                }
                CounterType::Plus1Plus1 => saw_p1p1 = true,
                other => panic!("unexpected counter type {other:?}"),
            }
        }
        node = d.sub_ability.as_deref();
    }

    let expected = [
        KeywordKind::Flying,
        KeywordKind::FirstStrike,
        KeywordKind::DoubleStrike,
        KeywordKind::Deathtouch,
        KeywordKind::Hexproof,
        KeywordKind::Indestructible,
        KeywordKind::Lifelink,
        KeywordKind::Menace,
        KeywordKind::Reach,
        KeywordKind::Trample,
        KeywordKind::Vigilance,
    ];
    assert_eq!(
        keyword_counters.len(),
        expected.len(),
        "all 11 keyword counters present (flying + the 10 repeated)"
    );
    for ((counter_kw, gate_kw), exp) in keyword_counters.iter().zip(expected) {
        assert_eq!(*counter_kw, exp, "placed counter keyword matches the list");
        assert_eq!(
            *gate_kw, exp,
            "the graveyard gate checks the SAME keyword as the placed counter"
        );
    }
    assert!(
        saw_p1p1,
        "the +1/+1-on-Kathril follow-up survives the expansion"
    );
}

#[test]
fn trigger_conditional_upkeep() {
    let def = parse_trigger_line(
        "At the beginning of your upkeep, if you control no creatures, sacrifice Test Card.",
        "Test Card",
    );
    insta::assert_json_snapshot!(def);
}

#[test]
fn trigger_compound_and_when_etb_half() {
    let defs = parse_trigger_lines_at_index(
        "When Test Card enters the battlefield and whenever a creature dies, draw a card.",
        "Test Card",
        None,
        &mut ParseContext::default(),
    );
    assert_eq!(defs.len(), 2, "compound trigger should split into 2");
    insta::assert_json_snapshot!(defs[0]);
}

#[test]
fn trigger_compound_enters_and_upkeep_splits() {
    let defs = parse_trigger_lines_at_index(
            "When this artifact enters and at the beginning of your upkeep, look at the top card of your library. If it's a card of the chosen type, you may reveal it and put it into your hand.",
            "Gathering Stone",
            None,
            &mut ParseContext::default(),
        );
    assert_eq!(
        defs.len(),
        2,
        "enters+upkeep compound must split into 2 triggers"
    );
    assert!(
        matches!(defs[0].mode, TriggerMode::ChangesZone),
        "first half should be ETB, got {:?}",
        defs[0].mode
    );
    assert!(
        matches!(defs[1].mode, TriggerMode::Phase),
        "second half should be upkeep phase, got {:?}",
        defs[1].mode
    );
    let expected_condition = Some(AbilityCondition::RevealedHasCardType {
        card_types: vec![],
        additional_filter: Some(FilterProp::IsChosenCreatureType),
        subtype_filter: None,
    });
    for (idx, def) in defs.iter().enumerate() {
        let execute = def
            .execute
            .as_deref()
            .unwrap_or_else(|| panic!("trigger {idx} should keep the shared effect"));
        assert!(
            matches!(&*execute.effect, Effect::Dig { .. }),
            "trigger {idx} should look at the top card, got {:?}",
            execute.effect
        );
        let reveal = execute
            .sub_ability
            .as_deref()
            .unwrap_or_else(|| panic!("trigger {idx} should keep the chosen-type reveal gate"));
        assert_eq!(
            reveal.condition,
            expected_condition.clone(),
            "trigger {idx} should keep the chosen-type gate"
        );
    }
}

#[test]
fn trigger_compound_and_when_dies_half() {
    let defs = parse_trigger_lines_at_index(
        "When Test Card enters the battlefield and whenever a creature dies, draw a card.",
        "Test Card",
        None,
        &mut ParseContext::default(),
    );
    assert_eq!(defs.len(), 2, "compound trigger should split into 2");
    insta::assert_json_snapshot!(defs[1]);
}

#[test]
fn trigger_optional_you_may() {
    let def = parse_trigger_line(
        "When Test Card enters the battlefield, you may draw a card.",
        "Test Card",
    );
    insta::assert_json_snapshot!(def);
}

#[test]
fn trigger_once_per_turn() {
    let def = parse_trigger_line(
            "Whenever a creature enters the battlefield under your control for the first time each turn, draw a card.",
            "Test Card",
        );
    insta::assert_json_snapshot!(def);
}

#[test]
fn trigger_unless_pay() {
    let def = parse_trigger_line(
        "At the beginning of your upkeep, sacrifice Test Card unless you pay {2}.",
        "Test Card",
    );
    insta::assert_json_snapshot!(def);
}

/// CR 608.2k + CR 603.7c + CR 701.9: Tergrid, God of Fright's discard
/// branch. The compound trigger splitter produces a `Discarded` trigger
/// (controller-scoped to `Opponent`), and the top-level
/// `ChangeZone { target: ParentTarget }` produced by the effect parser
/// is lifted to `TriggeringSource` by `lower_trigger_ir` so the runtime
/// resolves "that card" against the just-discarded object.
#[test]
fn trigger_discarded_lifts_that_card_to_triggering_source() {
    let def = parse_trigger_line(
            "Whenever an opponent discards a permanent card, you may put that card from a graveyard onto the battlefield under your control.",
            "Tergrid, God of Fright",
        );
    assert_eq!(def.mode, TriggerMode::Discarded);
    let execute = def.execute.as_deref().expect("trigger has execute");
    match execute.effect.as_ref() {
        Effect::ChangeZone {
            origin,
            destination,
            target,
            enters_under,
            ..
        } => {
            assert_eq!(*origin, Some(Zone::Graveyard));
            assert_eq!(*destination, Zone::Battlefield);
            assert!(matches!(target, TargetFilter::TriggeringSource));
            assert_eq!(*enters_under, Some(ControllerRef::You));
        }
        other => panic!("expected ChangeZone, got {other:?}"),
    }
}

/// CR 608.2k + CR 603.7c: The `ParentTarget` → `TriggeringSource` lift
/// must descend through chained `sub_ability`s, not just the top-level
/// effect. A Tergrid-shape trigger like "...exile that card, then
/// create a token" carries the "that card" anaphor on the FIRST
/// sub-ability's effect. Without sub_ability descent, the second link
/// would silently bind to the trigger source instead of the
/// just-discarded object.
///
/// Synthetic Oracle string: no shipping card today has this exact
/// shape, but the building block must work for the whole punisher-class.
#[test]
fn trigger_discarded_lift_descends_through_sub_ability() {
    let def = parse_trigger_line(
            "Whenever an opponent discards a permanent card, exile that card, then exile that card from a graveyard.",
            "Test Punisher",
        );
    assert_eq!(def.mode, TriggerMode::Discarded);
    let execute = def.execute.as_deref().expect("trigger has execute");

    // Walk the chain: collect every ChangeZone's target filter encountered
    // through the top-level effect and any sub_ability descent. Every
    // `ParentTarget` should have been lifted to `TriggeringSource`.
    let mut targets = Vec::new();
    if let Effect::ChangeZone { target, .. } = execute.effect.as_ref() {
        targets.push(target.clone());
    }
    let mut next = execute.sub_ability.as_deref();
    while let Some(child) = next {
        if let Effect::ChangeZone { target, .. } = child.effect.as_ref() {
            targets.push(target.clone());
        }
        next = child.sub_ability.as_deref();
    }

    assert!(
        !targets.is_empty(),
        "expected at least one ChangeZone in the chain (top + sub_ability), got none"
    );
    for (i, t) in targets.iter().enumerate() {
        assert!(
            !matches!(t, TargetFilter::ParentTarget),
            "chain link {i} still has ParentTarget — sub_ability lift did not descend: {t:?}",
        );
        assert!(
            matches!(t, TargetFilter::TriggeringSource),
            "chain link {i} should be TriggeringSource, got {t:?}",
        );
    }
}

/// CR 603.2 + CR 608.2c: Coiling Oracle's self-ETB trigger reveals the top
/// card and conditionally puts a land onto the battlefield.  The `ChangeZone`
/// sub-ability that follows `RevealTop` must keep `target: ParentTarget`
/// (resolved at runtime to `state.last_revealed_ids[0]`, i.e., the revealed
/// land card) — NOT `TriggeringSource` (the Coiling Oracle object itself).
///
/// Before the fix, `introduces_chosen_object_target` returned `false` for
/// `RevealTop`, so `lift_parent_target_to_triggering_source_in_ability`
/// descended past the RevealTop and rewrote the sub-ability's `ChangeZone`
/// target to `TriggeringSource`.  At runtime that caused `move_to_zone` to
/// be called with Coiling Oracle's own ID, which removed and re-added it to
/// the battlefield, emitted a new `ZoneChanged` ETB event, and looped
/// indefinitely.
///
/// This test locks the correct `ParentTarget` parse output so the anaphor
/// binds to the revealed card, not the trigger source.  Covers the
/// Coiling-Oracle / Explore / Animist's Awakening class of self-ETB RevealTop
/// triggers.
#[test]
fn reveal_top_sub_ability_target_stays_parent_not_triggering_source() {
    let def = parse_trigger_line(
            "When this creature enters, reveal the top card of your library. If it's a land card, put it onto the battlefield. Otherwise, put that card into your hand.",
            "Coiling Oracle",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));

    let execute = def.execute.as_deref().expect("trigger has execute body");

    // Top-level effect must be RevealTop.
    assert!(
        matches!(execute.effect.as_ref(), Effect::RevealTop { .. }),
        "expected RevealTop as top-level effect, got {:?}",
        execute.effect
    );

    // Walk sub_ability chain to find the ChangeZone (land → battlefield).
    let mut found_change_zone = false;
    let mut next = execute.sub_ability.as_deref();
    while let Some(child) = next {
        if let Effect::ChangeZone {
            destination,
            target,
            ..
        } = child.effect.as_ref()
        {
            if *destination == Zone::Battlefield {
                assert!(
                    matches!(target, TargetFilter::ParentTarget),
                    "ChangeZone(→Battlefield) sub-ability target must be ParentTarget \
                         (resolved to the revealed card at runtime via last_revealed_ids), \
                         but got {target:?}. \
                         TriggeringSource here would put Coiling Oracle itself onto the \
                         battlefield again, causing an infinite ETB loop (issue #2395).",
                );
                found_change_zone = true;
            }
        }
        next = child.sub_ability.as_deref();
    }

    assert!(
        found_change_zone,
        "expected a ChangeZone(→Battlefield) sub-ability in the trigger chain"
    );
}

// CR 701.3d: "Whenever ~ becomes unattached from a permanent" trigger
// Covers Grafted Exoskeleton, Stitcher's Graft, Grafted Wargear, etc.
#[test]
fn trigger_becomes_unattached_from_permanent() {
    let def = parse_trigger_line(
        "Whenever this Equipment becomes unattached from a permanent, sacrifice that permanent.",
        "Grafted Exoskeleton",
    );
    assert_eq!(def.mode, TriggerMode::Unattach);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(TypedFilter::permanent()))
    );
    assert_eq!(
        def.trigger_zones,
        vec![Zone::Battlefield, Zone::Graveyard, Zone::Exile]
    );
    let execute = def.execute.as_deref().expect("trigger has execute");
    let Effect::Sacrifice { target, .. } = execute.effect.as_ref() else {
        panic!("expected Sacrifice, got {:?}", execute.effect);
    };
    assert_eq!(*target, TargetFilter::TriggeringSource);
}

// CR 701.21a: Morkrut Necropod's
// "Whenever ~ attacks or blocks, sacrifice another creature or land" — the
// source-exclusion "another" must land `FilterProp::Another` on the CREATURE
// leg only. A creature is never a land, so distributing the exclusion to the
// land leg is meaningless; more importantly, WITHOUT the exclusion on the
// creature leg the source could sacrifice ITSELF, a functional bug (#4513).
// Parsed end-to-end through the production trigger entry to lock in that the
// sacrifice imperative re-applies the exclusion the count word consumed.
#[test]
fn morkrut_necropod_sacrifice_another_creature_or_land_trigger() {
    let def = parse_trigger_line(
        "Whenever Morkrut Necropod attacks or blocks, sacrifice another creature or land.",
        "Morkrut Necropod",
    );
    let execute = def.execute.as_deref().expect("trigger has execute");
    let Effect::Sacrifice { target, .. } = execute.effect.as_ref() else {
        panic!("expected Sacrifice, got {:?}", execute.effect);
    };
    let TargetFilter::Or { filters } = target else {
        panic!("expected an Or target for 'creature or land', got {target:?}");
    };
    assert_eq!(filters.len(), 2, "expected two legs, got {filters:?}");
    // Leg 0: creature — the source-exclusion MUST be present.
    let TargetFilter::Typed(creature_leg) = &filters[0] else {
        panic!("expected a typed creature leg, got {:?}", filters[0]);
    };
    assert!(
        creature_leg.type_filters.contains(&TypeFilter::Creature),
        "first leg should be the creature leg, got {creature_leg:?}"
    );
    assert!(
        creature_leg.properties.contains(&FilterProp::Another),
        "creature leg must carry FilterProp::Another so the source can't \
             sacrifice itself (#4513), got {creature_leg:?}"
    );
    // Leg 1: land — the source-exclusion is present here too. "Another" is
    // applied to every leg: it is vacuous on the land leg (a creature source
    // is never a land) but the uniform rule is what keeps a source that
    // matches a non-first leg (e.g. an artifact creature in "creature or
    // artifact") from sacrificing itself.
    let TargetFilter::Typed(land_leg) = &filters[1] else {
        panic!("expected a typed land leg, got {:?}", filters[1]);
    };
    assert!(
        land_leg.type_filters.contains(&TypeFilter::Land),
        "second leg should be the land leg, got {land_leg:?}"
    );
    assert!(
        land_leg.properties.contains(&FilterProp::Another),
        "land leg should carry FilterProp::Another (vacuous but uniform), got {land_leg:?}"
    );
}

// CR 701.3d: shorter form "becomes unattached" (future-proofing)
#[test]
fn trigger_becomes_unattached_short_form() {
    let def = parse_trigger_line(
        "Whenever ~ becomes unattached, sacrifice the permanent it was attached to.",
        "Test Equipment",
    );
    assert_eq!(
        def.mode,
        TriggerMode::Unattach,
        "Expected TriggerMode::Unattach for short form, got {:?}",
        def.mode
    );
    assert_eq!(def.valid_target, None);
}

// Regression: "Whenever ~ becomes unattached from a permanent, sacrifice that permanent."
// should NOT be parsed as TriggerMode::Unknown.
#[test]
fn trigger_becomes_unattached_not_unknown() {
    let def = parse_trigger_line(
        "Whenever this Equipment becomes unattached from a permanent, sacrifice that permanent.",
        "Stitcher's Graft",
    );
    assert!(
        !matches!(def.mode, TriggerMode::Unknown(_)),
        "Trigger should not fall through to Unknown; got {:?}",
        def.mode
    );
}

// CR 701.3a: "Whenever ~ becomes attached to a creature" trigger
// Covers Inchblade Companion, Assimilation Aegis, Enormous Energy Blade, Killer Cosplay.
#[test]
fn trigger_becomes_attached_to_a_creature() {
    let def = parse_trigger_line(
        "Whenever ~ becomes attached to a creature, that creature gets +1/+1 until end of turn.",
        "Inchblade Companion",
    );
    assert_eq!(
        def.mode,
        TriggerMode::Attached,
        "Expected TriggerMode::Attached, got {:?}",
        def.mode
    );
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::SelfRef),
        "Expected valid_card = SelfRef (Equipment self-reference), got {:?}",
        def.valid_card
    );
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(TypedFilter::creature())),
        "Expected valid_target = creature, got {:?}",
        def.valid_target
    );
}

// CR 701.3a: "becomes attached to a permanent" variant
#[test]
fn trigger_becomes_attached_to_a_permanent() {
    let def = parse_trigger_line(
        "Whenever ~ becomes attached to a permanent, draw a card.",
        "Assimilation Aegis",
    );
    assert_eq!(
        def.mode,
        TriggerMode::Attached,
        "Expected TriggerMode::Attached for 'attached to a permanent', got {:?}",
        def.mode
    );
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(TypedFilter::permanent())),
        "Expected valid_target = permanent, got {:?}",
        def.valid_target
    );
}

// Regression: "Whenever ~ becomes attached to a creature" should NOT be TriggerMode::Unknown.
#[test]
fn trigger_becomes_attached_not_unknown() {
    let def = parse_trigger_line(
        "Whenever ~ becomes attached to a creature, that creature gets +2/+2 until end of turn.",
        "Enormous Energy Blade",
    );
    assert!(
        !matches!(def.mode, TriggerMode::Unknown(_)),
        "Trigger should not fall through to Unknown; got {:?}",
        def.mode
    );
}

// Regression: non-self attached subjects are not supported by
// EffectResolved's source-only payload and must not parse as a broad
// TriggerMode::Attached trigger.
#[test]
fn trigger_becomes_attached_non_self_subject_stays_unknown() {
    let def = parse_trigger_line(
        "Whenever an Aura you control becomes attached to a creature you control, draw a card.",
        "Siona, Captain of the Pyleas",
    );
    assert!(
        matches!(def.mode, TriggerMode::Unknown(_)),
        "non-self attachment trigger should stay Unknown, got {:?}",
        def.mode
    );
}

/// CR 701.43a: "Whenever you exert a creature" — actor-side exert trigger.
/// Trueheart Twins, Battlefield Scavenger, Rohirrim Chargers, Vizier of the
/// True, and Resolute Survivors all share this exact trigger phrasing.
/// The parser must emit TriggerMode::Exerted with valid_card set to the
/// subject (a creature the controller exerts, i.e. a generic creature filter
/// scoped to the controller).
#[test]
fn trigger_you_exert_a_creature() {
    let def = parse_trigger_line(
            "Whenever you exert a creature, that creature gets +1/+0 and gains first strike until end of turn.",
            "Trueheart Twins",
        );
    assert_eq!(def.mode, TriggerMode::Exerted);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    let Some(TargetFilter::Typed(tf)) = def.valid_card else {
        panic!(
            "expected typed exerted creature filter, got {:?}",
            def.valid_card
        );
    };
    assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
}

/// CR 701.43a: Self-reference is the exerted permanent, not a degraded
/// generic filter.
#[test]
fn trigger_you_exert_self_ref() {
    let def = parse_trigger_line("Whenever you exert ~, untap it.", "Vizier of the True");
    assert_eq!(def.mode, TriggerMode::Exerted);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

/// CR 701.43a: Opponent actor scope must be recorded on valid_target while
/// the exerted creature remains the valid_card filter.
#[test]
fn trigger_opponent_exerts_a_creature() {
    let def = parse_trigger_line(
        "Whenever an opponent exerts a creature, you gain 1 life.",
        "Exertion Watcher",
    );
    assert_eq!(def.mode, TriggerMode::Exerted);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
    let Some(TargetFilter::Typed(tf)) = def.valid_card else {
        panic!(
            "expected typed exerted creature filter, got {:?}",
            def.valid_card
        );
    };
    assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
}

/// CR 701.43a: Resolute Survivors — "Whenever you exert a creature" with a
/// life-gain effect. Ensures the trigger body is parsed independently of
/// the trigger condition.
#[test]
fn trigger_exert_resolute_survivors() {
    let def = parse_trigger_line(
        "Whenever you exert a creature, you gain 1 life.",
        "Resolute Survivors",
    );
    assert_eq!(def.mode, TriggerMode::Exerted);
    assert!(def.valid_card.is_some());
}

/// CR 701.3a Pattern 2: "Whenever an Aura becomes attached to ~" —
/// the subject is the Aura (not self-ref), the host is the trigger source.
/// Cards: Bramble Elemental, Brood Keeper.
#[test]
fn trigger_an_aura_becomes_attached_to_self() {
    let def = parse_trigger_line(
        "Whenever an Aura becomes attached to ~, create two 1/1 green Saproling creature tokens.",
        "Bramble Elemental",
    );
    assert_eq!(def.mode, TriggerMode::Attached);
    let Some(TargetFilter::Typed(tf)) = &def.valid_card else {
        panic!("expected typed Aura filter, got {:?}", def.valid_card);
    };
    assert!(
        tf.type_filters
            .contains(&crate::types::ability::TypeFilter::Subtype(
                "Aura".to_string()
            )),
        "filter must restrict to Aura subtype, got {:?}",
        tf.type_filters
    );
    assert_eq!(def.valid_target, Some(TargetFilter::SelfRef));
}

/// CR 701.3a Pattern 2: Brood Keeper uses the same phrase.
#[test]
fn trigger_an_aura_becomes_attached_to_self_brood_keeper() {
    let def = parse_trigger_line(
            "Whenever an Aura becomes attached to ~, create a 2/2 red Dragon creature token with flying. It has \"{R}: This creature gets +1/+0 until end of turn.\"",
            "Brood Keeper",
        );
    assert_eq!(def.mode, TriggerMode::Attached);
    assert!(def.valid_card.is_some(), "Aura filter must be set");
    assert_eq!(def.valid_target, Some(TargetFilter::SelfRef));
}

/// CR 104.3a: "Whenever a player loses the game" — Withengar Unbound,
/// Ramses Assassin Lord, Blood Tyrant. The explicit player subject is
/// represented as a player filter so the trigger's player axis is typed.
#[test]
fn trigger_a_player_loses_the_game() {
    let def = parse_trigger_line(
        "Whenever a player loses the game, put thirteen +1/+1 counters on this creature.",
        "Withengar Unbound",
    );
    assert_eq!(
        def.mode,
        TriggerMode::LosesGame,
        "expected LosesGame mode, got {:?}",
        def.mode
    );
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Player),
        "valid_target must represent the player-loss subject"
    );
}

/// CR 104.3a: "Whenever an opponent loses the game" — scopes to opponent
/// players only via a controller filter.
#[test]
fn trigger_an_opponent_loses_the_game() {
    let def = parse_trigger_line(
        "Whenever an opponent loses the game, you win the game.",
        "SomeCard",
    );
    assert_eq!(
        def.mode,
        TriggerMode::LosesGame,
        "expected LosesGame mode, got {:?}",
        def.mode
    );
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        )),
        "valid_target must restrict to opponents"
    );
}

/// CR 104.3a: "Whenever you lose the game" uses the same parser family and
/// scopes the losing player to the trigger controller.
#[test]
fn trigger_you_lose_the_game() {
    let def = parse_trigger_line(
        "When you lose the game, draw a card.",
        "Platinum Contraption",
    );
    assert_eq!(def.mode, TriggerMode::LosesGame);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

/// CR 701.38: "Whenever players finish voting" triggers once per vote resolution.
/// Cards: Model of Unity, Erestor of the Council, Grudge Keeper.
#[test]
fn trigger_players_finish_voting_whenever() {
    let def = parse_trigger_line(
            "Whenever players finish voting, you and each opponent who voted for a choice you voted for may scry 2.",
            "Model of Unity",
        );
    assert_eq!(def.mode, TriggerMode::Vote);
    assert_eq!(def.valid_target, None);
}

/// CR 701.38: "When players finish voting" accepts the alternate trigger prefix.
#[test]
fn trigger_players_finish_voting_when_prefix() {
    let def = parse_trigger_line(
            "When players finish voting, each opponent who voted for a choice you didn't vote for loses 2 life.",
            "Grudge Keeper",
        );
    assert_eq!(def.mode, TriggerMode::Vote);
    assert_eq!(def.valid_target, None);
}

/// CR 701.30b-c: "Whenever you clash" scopes to the trigger controller as
/// either player participating in the clash.
/// Cards: Entangling Trap, Rebellion of the Flamekin.
#[test]
fn trigger_you_clash_whenever() {
    let def = parse_trigger_line(
            "Whenever you clash, tap target creature an opponent controls. If you won, that creature doesn't untap during its controller's next untap step.",
            "Entangling Trap",
        );
    assert_eq!(def.mode, TriggerMode::Clashed);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    let Some(execute) = def.execute.as_deref() else {
        panic!("expected trigger body");
    };
    let Some(tail) = execute.sub_ability.as_deref() else {
        panic!("expected if-you-won tail");
    };
    assert_eq!(tail.condition, Some(AbilityCondition::EventOutcomeWon));
}

/// CR 701.30b-c: "When you clash" accepts the alternate trigger prefix.
#[test]
fn trigger_you_clash_when_prefix() {
    let def = parse_trigger_line(
            "When you clash, you may pay {1}. If you do, create a 3/1 red Elemental Shaman creature token. If you won, that token gains haste until end of turn.",
            "Rebellion of the Flamekin",
        );
    assert_eq!(def.mode, TriggerMode::Clashed);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    let Some(pay_cost) = def.execute.as_deref() else {
        panic!("expected trigger body");
    };
    let Some(token) = pay_cost.sub_ability.as_deref() else {
        panic!("expected if-you-do token tail");
    };
    assert_eq!(token.condition, Some(AbilityCondition::effect_performed()));
    let Some(haste) = token.sub_ability.as_deref() else {
        panic!("expected if-you-won haste tail");
    };
    assert_eq!(haste.condition, Some(AbilityCondition::EventOutcomeWon));
}

/// CR 701.30b-c: "Whenever a player clashes" is not scoped to a specific player.
#[test]
fn trigger_a_player_clashes() {
    let def = parse_trigger_line("Whenever a player clashes, draw a card.", "Clash Watcher");
    assert_eq!(def.mode, TriggerMode::Clashed);
    assert_eq!(def.valid_target, None);
}

/// CR 602.1 + CR 605.1a: Passive form — "whenever an ability of equipped creature
/// is activated" routes to AbilityActivated with valid_card = AttachedTo.
/// Cards: Battlemage's Bracers, Illusionist's Bracers.
#[test]
fn trigger_ability_of_equipped_creature_is_activated_bracers() {
    let def = parse_trigger_line(
            "Whenever an ability of equipped creature is activated, if it isn't a mana ability, you may pay {1}. If you do, copy that ability. You may choose new targets for the copy.",
            "Battlemage's Bracers",
        );
    assert_eq!(def.mode, TriggerMode::AbilityActivated);
    assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    assert_eq!(def.valid_target, None);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::ActivatedAbilityIsNonMana)
    );
    assert!(matches!(
        def.execute
            .as_deref()
            .map(|ability| ability.effect.as_ref()),
        Some(Effect::PayCost { .. })
    ));
}

/// CR 602.1 + CR 605.1a: Illusionist's Bracers uses the same passive trigger phrase.
#[test]
fn trigger_ability_of_equipped_creature_is_activated_illusionists() {
    let def = parse_trigger_line(
            "Whenever an ability of equipped creature is activated, if it isn't a mana ability, copy that ability. You may choose new targets for the copy.",
            "Illusionist's Bracers",
        );
    assert_eq!(def.mode, TriggerMode::AbilityActivated);
    assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::ActivatedAbilityIsNonMana)
    );
    assert!(matches!(
        def.execute
            .as_deref()
            .map(|ability| ability.effect.as_ref()),
        Some(Effect::CopySpell { .. })
    ));
}

/// CR 602.1: "Whenever you activate an ability of an artifact or creature
/// that isn't a mana ability" — source-object filter with type disjunction.
/// Cards: Crackdown Construct, Ashnod the Uncaring.
#[test]
fn trigger_activate_ability_of_artifact_or_creature() {
    let def = parse_trigger_line(
            "Whenever you activate an ability of an artifact or creature that isn't a mana ability, ~ gets +1/+1 until end of turn.",
            "Crackdown Construct",
        );
    assert_eq!(def.mode, TriggerMode::AbilityActivated);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::ActivatedAbilityIsNonMana)
    );
    // Source-object filter: artifact or creature.
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::AnyOf(vec![
                TypeFilter::Artifact,
                TypeFilter::Creature,
            ])],
            properties: vec![FilterProp::InZone {
                zone: Zone::Battlefield,
            }],
            ..TypedFilter::default()
        }))
    );
}

/// CR 109.2: "an ability of an Elemental" describes an Elemental permanent
/// source on the battlefield, not an Elemental card in another zone.
#[test]
fn trigger_activate_ability_of_subtype_source() {
    let def = parse_trigger_line(
        "Whenever you activate an ability of an Elemental, ~ gets +1/+0 until end of turn.",
        "Ceaseless Searblades",
    );
    assert_eq!(def.mode, TriggerMode::AbilityActivated);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Subtype("Elemental".to_string())],
            properties: vec![FilterProp::InZone {
                zone: Zone::Battlefield,
            }],
            ..TypedFilter::default()
        }))
    );
}

#[test]
fn twilight_diviner_second_trigger_has_graveyard_origin_condition() {
    let def = parse_trigger_line(
            "Whenever one or more other creatures you control enter, if they entered or were cast from a graveyard, create a token that's a copy of one of them. This ability triggers only once each turn.",
            "Twilight Diviner",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert!(def.batched, "should be batched");
    assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
    match &def.condition {
        Some(TriggerCondition::Or { conditions }) => {
            assert_eq!(conditions.len(), 2);
            assert!(
                matches!(
                    &conditions[0],
                    TriggerCondition::ZoneChangeObjectMatchesFilter {
                        origin: Some(Zone::Graveyard),
                        destination: Zone::Battlefield,
                        ..
                    }
                ),
                "first disjunct should be ZoneChangeObjectMatchesFilter(Graveyard→Battlefield)"
            );
            assert_eq!(
                conditions[1],
                TriggerCondition::WasCast {
                    zone: Some(Zone::Graveyard),
                    controller: None,
                    owner: None,
                }
            );
        }
        other => panic!("expected Or condition, got: {other:?}"),
    }
}

#[test]
fn entered_or_cast_from_graveyard_singular_form() {
    let def = parse_trigger_line(
            "Whenever another creature enters under your control, if it entered or was cast from a graveyard, draw a card.",
            "Test Card",
        );
    match &def.condition {
        Some(TriggerCondition::Or { conditions }) => {
            assert_eq!(conditions.len(), 2);
            assert!(matches!(
                &conditions[0],
                TriggerCondition::ZoneChangeObjectMatchesFilter {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    ..
                }
            ));
            assert_eq!(
                conditions[1],
                TriggerCondition::WasCast {
                    zone: Some(Zone::Graveyard),
                    controller: None,
                    owner: None,
                }
            );
        }
        other => panic!("expected Or condition, got: {other:?}"),
    }
}

// ---- DamageDone recipient-gating (WHO misparse cluster #2) ----

/// CR 120.3: building-block coverage for `parse_object_recipient_filter` —
/// bare object recipients yield the typed object filter, but the disjunctive
/// "creature or player"/"creature or opponent" recipients and bare player
/// recipients must decline (terminator guard) so they fall through to the
/// player axis.
#[test]
fn parse_object_recipient_filter_typed_and_declines() {
    // Accepts: bare object recipients.
    assert_eq!(
        parse_object_recipient_filter("to a creature").map(|(_, f)| f),
        Ok(TargetFilter::Typed(TypedFilter::creature()))
    );
    assert_eq!(
        parse_object_recipient_filter("to a creature, destroy it").map(|(_, f)| f),
        Ok(TargetFilter::Typed(TypedFilter::creature())),
        "comma terminator after the type phrase is accepted"
    );
    assert_eq!(
        parse_object_recipient_filter("to a permanent").map(|(_, f)| f),
        Ok(TargetFilter::Typed(TypedFilter::permanent()))
    );
    assert_eq!(
        parse_object_recipient_filter("to a planeswalker").map(|(_, f)| f),
        Ok(TargetFilter::Typed(TypedFilter::new(
            TypeFilter::Planeswalker
        )))
    );
    // Declines: disjunctive and bare-player recipients (terminator guard).
    assert!(
        parse_object_recipient_filter("to a creature or player").is_err(),
        "'creature or player' must decline so the recipient stays unscoped"
    );
    assert!(
        parse_object_recipient_filter("to a creature or opponent").is_err(),
        "'creature or opponent' must decline so the recipient stays unscoped"
    );
    assert!(parse_object_recipient_filter("to a player").is_err());
    assert!(parse_object_recipient_filter("to an opponent").is_err());
    assert!(parse_object_recipient_filter("to you").is_err());
}

/// CR 120.1 + CR 108.3: `parse_damage_to_its_owner` accepts the exact
/// relational phrase and rejects word-boundary continuations.
#[test]
fn parse_damage_to_its_owner_word_boundary() {
    assert!(parse_damage_to_its_owner("to its owner").is_ok());
    assert!(parse_damage_to_its_owner("to its owner, untap it").is_ok());
    assert!(parse_damage_to_its_owner("to its owners").is_err());
    assert!(parse_damage_to_its_owner("to its owner's hand").is_err());
    assert!(parse_damage_to_its_owner("to a creature").is_err());
}

/// CR 120.3: Strax, Sontaran Nurse — "Whenever Strax deals damage to a
/// creature" must scope `valid_target` to a Creature object filter, with no
/// condition. (Subject-led grammar.)
#[test]
fn strax_glory_of_battle_scopes_creature_recipient() {
    let def = parse_trigger_line(
        "Whenever Strax deals damage to a creature, put a +1/+1 counter on Strax.",
        "Strax, Sontaran Nurse",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.valid_source, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(TypedFilter::creature()))
    );
    assert_eq!(def.condition, None);
}

/// CR 120.1 + CR 108.3: The Beast, Deathless Prince — "Whenever a creature
/// deals combat damage to its owner" must gate on the relational condition
/// (damaged player == damaging object's owner), leaving `valid_target` unset.
#[test]
fn the_beast_owner_relation_condition() {
    let def = parse_trigger_line(
        "Whenever a creature deals combat damage to its owner, untap The Beast and draw a card.",
        "The Beast, Deathless Prince",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::Typed(TypedFilter::creature()))
    );
    assert_eq!(def.valid_target, None);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DamagedPlayerIsEventSourceOwner)
    );
}

/// CR 120.3: SelfRef class sample — Lowland Basilisk and Mirri the Cursed
/// also scope `valid_target` to the Creature recipient.
#[test]
fn self_ref_class_sample_scopes_creature_recipient() {
    let basilisk = parse_trigger_line(
            "Whenever Lowland Basilisk deals damage to a creature, destroy that creature at end of combat.",
            "Lowland Basilisk",
        );
    assert_eq!(basilisk.mode, TriggerMode::DamageDone);
    assert_eq!(
        basilisk.valid_target,
        Some(TargetFilter::Typed(TypedFilter::creature()))
    );
    assert_eq!(basilisk.condition, None);

    let mirri = parse_trigger_line(
            "Whenever Mirri the Cursed deals combat damage to a creature, put a +1/+1 counter on Mirri the Cursed.",
            "Mirri the Cursed",
        );
    assert_eq!(mirri.mode, TriggerMode::DamageDone);
    assert_eq!(mirri.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(
        mirri.valid_target,
        Some(TargetFilter::Typed(TypedFilter::creature()))
    );
}

/// CR 120.3: non-SelfRef class sample — Greven il-Vec ("a creature you
/// control deals damage to a creature") must populate `valid_target` too,
/// which is exactly the precondition that makes the aggregate-path guard
/// (Step 0b) necessary. Source-led grammar.
#[test]
fn non_self_ref_class_sample_scopes_creature_recipient() {
    let def = parse_trigger_line(
            "Whenever a creature you control deals damage to a creature, destroy the other creature. It can't be regenerated.",
            "Greven il-Vec",
        );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You)
        ))
    );
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(TypedFilter::creature()))
    );
    assert_eq!(def.condition, None);
}

/// CR 120.3 + CR 102.2: "creature or player" must NOT be mis-scoped to
/// `Typed([Creature])` — the recipient covers a player too, so the trigger
/// stays unscoped (any recipient fires). Confirms the terminator guard at
/// the full-card level (Crovax / Flesh Reaver class).
#[test]
fn creature_or_player_recipient_not_mis_scoped() {
    let crovax = parse_trigger_line(
        "Whenever a creature you control deals damage to a creature or player, you gain 1 life.",
        "Crovax the Cursed",
    );
    assert_eq!(crovax.mode, TriggerMode::DamageDone);
    assert_ne!(
        crovax.valid_target,
        Some(TargetFilter::Typed(TypedFilter::creature())),
        "the player leg must not be dropped"
    );

    let flesh_reaver = parse_trigger_line(
            "Whenever Flesh Reaver deals damage to a creature or opponent, Flesh Reaver deals that much damage to you.",
            "Flesh Reaver",
        );
    assert_eq!(flesh_reaver.mode, TriggerMode::DamageDone);
    assert_ne!(
        flesh_reaver.valid_target,
        Some(TargetFilter::Typed(TypedFilter::creature())),
        "the opponent leg must not be dropped"
    );
}

/// CR 120.3 + CR 120.1: source-led smoke for the second parse site (Step 4b)
/// — a synthetic "a creature you control deals damage to a creature" / "to
/// its owner" exercise the recipient handling at
/// `try_parse_source_deals_damage_trigger`.
#[test]
fn source_led_recipient_smoke() {
    let object_recipient = parse_trigger_line(
        "Whenever a creature you control deals damage to a creature, draw a card.",
        "Test Source Card",
    );
    assert_eq!(object_recipient.mode, TriggerMode::DamageDone);
    assert_eq!(
        object_recipient.valid_target,
        Some(TargetFilter::Typed(TypedFilter::creature()))
    );

    let owner_recipient = parse_trigger_line(
        "Whenever a creature deals combat damage to its owner, draw a card.",
        "Test Owner Card",
    );
    assert_eq!(owner_recipient.mode, TriggerMode::DamageDone);
    assert_eq!(
        owner_recipient.condition,
        Some(TriggerCondition::DamagedPlayerIsEventSourceOwner)
    );
}
