use super::*;
use insta::assert_json_snapshot;

// -----------------------------------------------------------------------
// Group 1: Continuation patching
// -----------------------------------------------------------------------

#[test]
fn continuation_search_put_onto_battlefield_then_shuffle() {
    let def = parse_effect_chain(
        "search your library for a creature card, put it onto the battlefield, then shuffle",
        AbilityKind::Spell,
    );
    assert_json_snapshot!("continuation_search_put_battlefield_shuffle", def);
}

#[test]
fn continuation_search_reveal_put_into_hand_then_shuffle() {
    let def = parse_effect_chain(
        "search your library for a card, reveal it, put it into your hand, then shuffle",
        AbilityKind::Spell,
    );
    assert_json_snapshot!("continuation_search_reveal_hand_shuffle", def);
}

#[test]
fn continuation_search_conditional_destination_does_not_insert_default_put() {
    let def = parse_effect_chain(
            "Search your library for a creature or land card and reveal it. Put it onto the battlefield tapped if it's a land card. Otherwise, put it into your hand. Then shuffle.",
            AbilityKind::Spell,
        );

    match &*def.effect {
        Effect::SearchLibrary {
            reveal: true,
            split: None,
            ..
        } => {}
        other => panic!("expected revealed SearchLibrary without split, got {other:?}"),
    }

    let put_land = def
        .sub_ability
        .as_deref()
        .expect("search should chain directly to conditional destination");
    assert_eq!(
        put_land.condition,
        Some(AbilityCondition::RevealedHasCardType {
            card_types: vec![CoreType::Land],
            additional_filter: None,
            subtype_filter: None,
        })
    );
    match &*put_land.effect {
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Battlefield,
            target: TargetFilter::ParentTarget,
            enter_tapped: crate::types::zones::EtbTapState::Tapped,
            ..
        } => {}
        other => panic!("expected conditional battlefield put, got {other:?}"),
    }

    let put_nonland = put_land
        .else_ability
        .as_deref()
        .expect("conditional destination should carry hand fallback");
    match &*put_nonland.effect {
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Hand,
            target: TargetFilter::ParentTarget,
            ..
        } => {}
        other => panic!("expected hand fallback, got {other:?}"),
    }

    let shuffle = put_land
        .sub_ability
        .as_deref()
        .expect("conditional destination should chain into shuffle");
    assert!(matches!(&*shuffle.effect, Effect::Shuffle { .. }));
}

#[test]
fn continuation_search_exile_then_shuffle() {
    let def = parse_effect_chain(
        "search your library for a card, exile it face down, then shuffle",
        AbilityKind::Spell,
    );

    let Some(change_zone) = def.sub_ability.as_ref() else {
        panic!("search should chain into the exile destination");
    };
    match &*change_zone.effect {
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Exile,
            target: TargetFilter::Any,
            ..
        } => {}
        other => panic!("expected library-to-exile search destination, got {other:?}"),
    }
    let Some(shuffle) = change_zone.sub_ability.as_ref() else {
        panic!("exile destination should chain into shuffle");
    };
    assert!(matches!(&*shuffle.effect, Effect::Shuffle { .. }));
}

#[test]
fn beseech_the_mirror_search_exiles_and_has_hand_fallback() {
    let def = parse_effect_chain(
            "search your library for a card, exile it face down, then shuffle. if this spell was bargained, you may cast the exiled card without paying its mana cost if that spell's mana value is 4 or less. put the exiled card into your hand if it wasn't cast this way",
            AbilityKind::Spell,
        );

    let Some(exile) = def.sub_ability.as_ref() else {
        panic!("search should chain into exile");
    };
    assert!(matches!(
        &*exile.effect,
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Exile,
            ..
        }
    ));

    let cast = exile
        .sub_ability
        .as_deref()
        .and_then(|shuffle| shuffle.sub_ability.as_deref())
        .expect("shuffle should chain into bargained cast");
    match &*cast.effect {
        Effect::CastFromZone {
            constraint:
                Some(CastPermissionConstraint::ManaValue {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: 4 },
                }),
            ..
        } => {}
        other => panic!("expected CastFromZone with mana-value constraint, got {other:?}"),
    }
    assert!(cast.optional);
    assert!(matches!(
        cast.condition,
        Some(AbilityCondition::AdditionalCostPaid { .. })
    ));
    assert!(
        cast.sub_ability.is_none(),
        "accepting the optional cast must not also run the hand fallback"
    );

    let hand_fallback = cast
        .else_ability
        .as_ref()
        .expect("condition-false path should put the exiled card into hand");
    assert!(matches!(
        &*hand_fallback.effect,
        Effect::ChangeZoneAll {
            origin: Some(Zone::Exile),
            destination: Zone::Hand,
            target: TargetFilter::TrackedSet { .. },
            ..
        }
    ));
}

#[test]
fn continuation_draw_then_discard() {
    let def = parse_effect_chain("draw two cards, then discard a card", AbilityKind::Spell);
    assert_json_snapshot!("continuation_draw_then_discard", def);
}

/// Issue #3296: the "If you do, discard that many cards" rider must read the
/// draw count via `PreviousEffectAmount`, not the combat-damage trigger's
/// `EventContextAmount` (which can equal the whole hand size).
#[test]
fn hordewing_skaab_discard_that_many_uses_previous_effect_amount() {
    let def = parse_effect_chain(
            "you may draw cards equal to the number of opponents dealt damage this way. If you do, discard that many cards.",
            AbilityKind::Spell,
        );
    let sub = def.sub_ability.as_ref().expect("discard rider sub_ability");
    assert!(matches!(
        &*sub.effect,
        Effect::Discard {
            count: QuantityExpr::Ref {
                qty: QuantityRef::PreviousEffectAmount { .. },
            },
            ..
        }
    ));
}

#[test]
fn continuation_search_put_onto_battlefield_tapped_then_shuffle() {
    let def = parse_effect_chain(
            "search your library for a basic land card, put it onto the battlefield tapped, then shuffle",
            AbilityKind::Spell,
        );
    assert_json_snapshot!("continuation_search_battlefield_tapped_shuffle", def);
}

// -----------------------------------------------------------------------
// Group 2: Condition lifting
// -----------------------------------------------------------------------

#[test]
fn condition_if_then_draw() {
    let def = parse_effect_chain("if you control a creature, draw a card", AbilityKind::Spell);
    assert_json_snapshot!("condition_if_control_creature_draw", def);
}

#[test]
fn condition_unless_pay() {
    let def = parse_effect_chain(
        "counter target spell unless its controller pays {2}",
        AbilityKind::Spell,
    );
    assert_json_snapshot!("condition_counter_unless_pays", def);
}

#[test]
fn condition_optional_you_may_draw() {
    let def = parse_effect_chain("you may draw a card", AbilityKind::Spell);
    assert_json_snapshot!("condition_you_may_draw", def);
}

#[test]
fn condition_you_may_pay_then_effect() {
    let def = parse_effect_chain(
        "you may pay {2}. if you do, draw a card",
        AbilityKind::Spell,
    );
    assert_json_snapshot!("condition_you_may_pay_if_do_draw", def);
}

// -----------------------------------------------------------------------
// Group 3: Delayed-trigger wrapping
// -----------------------------------------------------------------------

#[test]
fn delayed_trigger_at_beginning_of_next_end_step() {
    let def = parse_effect_chain(
        "at the beginning of the next end step, sacrifice it",
        AbilityKind::Spell,
    );
    assert_json_snapshot!("delayed_trigger_next_end_step_sacrifice", def);
}

#[test]
fn delayed_trigger_beginning_of_next_upkeep() {
    let def = parse_effect_chain(
        "at the beginning of your next upkeep, draw a card",
        AbilityKind::Spell,
    );
    assert_json_snapshot!("delayed_trigger_next_upkeep_draw", def);
}

#[test]
fn delayed_trigger_until_end_of_turn() {
    let def = parse_effect_chain(
        "target creature gets +3/+3 until end of turn",
        AbilityKind::Spell,
    );
    assert_json_snapshot!("delayed_until_end_of_turn_pump", def);
}

#[test]
fn delayed_trigger_exile_return_end_step() {
    let def = parse_effect_chain(
            "exile target creature. return it to the battlefield under its owner's control at the beginning of the next end step",
            AbilityKind::Spell,
        );
    assert_json_snapshot!("delayed_exile_return_next_end_step", def);
}

/// Walks a parsed chain to the `ChangeZone` nested inside the first
/// `CreateDelayedTrigger` reachable via `sub_ability`. Returns `(origin,
/// is_parent_target)`.
fn delayed_return_change_zone(def: &AbilityDefinition) -> (Option<Zone>, bool) {
    let mut cursor = Some(def);
    while let Some(node) = cursor {
        if let Effect::CreateDelayedTrigger { effect, .. } = &*node.effect {
            if let Effect::ChangeZone { origin, target, .. } = &*effect.effect {
                return (*origin, matches!(target, TargetFilter::ParentTarget));
            }
        }
        cursor = node.sub_ability.as_deref();
    }
    panic!("no delayed-trigger ChangeZone found in chain");
}

/// CR 603.7c: real Flickerwisp phrasing ("return that card") stamps the
/// prior exile clause's destination as the delayed return's expected origin.
/// Regression guard for the anaphor-detector-gating defect.
#[test]
fn delayed_return_stamps_exile_origin_for_that_card_phrasing() {
    let def = parse_effect_chain(
            "exile target creature. return that card to the battlefield at the beginning of the next end step",
            AbilityKind::Spell,
        );
    let (origin, is_parent) = delayed_return_change_zone(&def);
    assert_eq!(origin, Some(Zone::Exile));
    assert!(is_parent);
}

/// The stamp is independent of which anaphor the text uses ("return it").
#[test]
fn delayed_return_stamps_exile_origin_for_it_phrasing() {
    let def = parse_effect_chain(
            "exile target creature. return it to the battlefield under its owner's control at the beginning of the next end step",
            AbilityKind::Spell,
        );
    let (origin, _) = delayed_return_change_zone(&def);
    assert_eq!(origin, Some(Zone::Exile));
}

/// SHOULD-FIX 1: a top-level `ParentTarget` `ChangeZone` NOT wrapped in a
/// `CreateDelayedTrigger` must keep `origin == None` — only delayed snapshot
/// returns are stamped.
#[test]
fn non_delayed_parent_target_change_zone_not_stamped() {
    let mut prev = Effect::ChangeZone {
        enters_modified_if: None,
        origin: None,
        destination: Zone::Exile,
        target: TargetFilter::ParentTarget,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
    };
    // Non-delayed top-level ParentTarget return.
    stamp_delayed_returns(&mut prev, Zone::Exile);
    match prev {
        Effect::ChangeZone { origin, .. } => assert_eq!(origin, None),
        _ => unreachable!(),
    }
}

/// No spurious stamp on a non-snapshot delayed clause (draw a card).
#[test]
fn delayed_non_snapshot_clause_not_stamped() {
    let def = parse_effect_chain(
        "exile target creature. at the beginning of the next end step, draw a card",
        AbilityKind::Spell,
    );
    // The delayed clause has no ChangeZone — walk it and confirm no panic-free
    // ChangeZone exists; if a CreateDelayedTrigger is present its inner effect
    // is Draw, not ChangeZone.
    let mut cursor = Some(&def);
    let mut saw_delayed = false;
    while let Some(node) = cursor {
        if let Effect::CreateDelayedTrigger { effect, .. } = &*node.effect {
            saw_delayed = true;
            assert!(
                !matches!(&*effect.effect, Effect::ChangeZone { .. }),
                "delayed draw clause must not become a ChangeZone"
            );
        }
        cursor = node.sub_ability.as_deref();
    }
    assert!(saw_delayed, "expected a delayed-trigger clause");
}

/// NIT 2 (mandatory): `stamp_inside_delayed` reads the prior clause's
/// `destination` rather than hard-coding `Exile`. Synthetic two-clause IR with
/// a non-Exile prior destination (Hand) proves the helper tracks `destination`.
#[test]
fn delayed_return_stamps_non_exile_prior_destination() {
    let inner_return = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            enters_modified_if: None,
            origin: None,
            destination: Zone::Battlefield,
            target: TargetFilter::ParentTarget,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
        },
    );
    let mut delayed = Effect::CreateDelayedTrigger {
        condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
        effect: Box::new(inner_return),
        uses_tracked_set: false,
    };
    // Prior clause placed the referent in Hand, not Exile.
    stamp_delayed_returns(&mut delayed, Zone::Hand);
    match delayed {
        Effect::CreateDelayedTrigger { effect, .. } => match &*effect.effect {
            Effect::ChangeZone { origin, .. } => assert_eq!(*origin, Some(Zone::Hand)),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

// -----------------------------------------------------------------------
// Group 4: Sub_ability assembly
// -----------------------------------------------------------------------

#[test]
fn assembly_two_clause_chain() {
    let def = parse_effect_chain(
        "target creature gets +2/+2 until end of turn. draw a card",
        AbilityKind::Spell,
    );
    assert_json_snapshot!("assembly_two_clause_pump_draw", def);
}

#[test]
fn assembly_three_clause_chain() {
    let def = parse_effect_chain(
        "destroy target creature. its controller loses 2 life. you gain 2 life",
        AbilityKind::Spell,
    );
    assert_json_snapshot!("assembly_three_clause_destroy_lose_gain", def);
}

#[test]
fn assembly_each_opponent_discard() {
    let def = parse_effect_chain("each opponent discards a card", AbilityKind::Spell);
    assert_json_snapshot!("assembly_each_opponent_discard", def);
}

#[test]
fn assembly_gain_life_and_draw() {
    let def = parse_effect_chain("you gain 3 life. draw a card", AbilityKind::Spell);
    assert_json_snapshot!("assembly_gain_life_draw", def);
}

#[test]
fn assembly_create_token_and_pump() {
    let def = parse_effect_chain(
        "create a 1/1 white Soldier creature token. put a +1/+1 counter on it",
        AbilityKind::Spell,
    );
    assert_json_snapshot!("assembly_create_token_put_counter", def);
}

#[test]
fn return_target_and_same_name_from_your_graveyard_carries_zone_and_mass_tail() {
    let def = parse_effect_chain(
            "Return target creature card and all other cards with the same name as that card from your graveyard to the battlefield tapped.",
            AbilityKind::Activated,
        );

    let Effect::ChangeZone {
        origin,
        destination,
        target,
        enter_tapped,
        ..
    } = &*def.effect
    else {
        panic!("expected primary ChangeZone, got {:?}", def.effect);
    };
    assert_eq!(*origin, Some(Zone::Graveyard));
    assert_eq!(*destination, Zone::Battlefield);
    assert!(enter_tapped.is_tapped());
    let TargetFilter::Typed(primary) = target else {
        panic!("expected typed primary target, got {0:?}", target);
    };
    assert!(primary.properties.contains(&FilterProp::InZone {
        zone: Zone::Graveyard
    }));
    assert!(
        primary.properties.contains(&FilterProp::Owned {
            controller: ControllerRef::You
        }),
        "from your graveyard should be owner-scoped, got {:?}",
        primary.properties
    );

    let same_name = def.sub_ability.as_ref().expect("expected same-name tail");
    let Effect::ChangeZoneAll {
        origin,
        destination,
        target,
        enters_under,
        enter_tapped,
        enter_with_counters: _,
        face_down_profile: None,
        library_position: None,
        random_order: false,
    } = &*same_name.effect
    else {
        panic!("expected ChangeZoneAll tail, got {:?}", same_name.effect);
    };
    assert_eq!(*origin, Some(Zone::Graveyard));
    assert_eq!(*destination, Zone::Battlefield);
    assert_eq!(*enters_under, None);
    assert!(enter_tapped.is_tapped());
    let TargetFilter::Typed(tail) = target else {
        panic!("expected typed same-name tail, got {0:?}", target);
    };
    assert!(tail.properties.contains(&FilterProp::InZone {
        zone: Zone::Graveyard
    }));
    assert!(tail.properties.contains(&FilterProp::Owned {
        controller: ControllerRef::You
    }));
    assert!(tail
        .properties
        .contains(&FilterProp::SameNameAsParentTarget));
}

#[test]
fn cost_paid_object_instead_clause_uses_cost_paid_toughness() {
    let def = parse_effect_chain(
            "Create a Blood token. If you sacrificed an Angel this way, create a number of Blood tokens equal to its toughness instead.",
            AbilityKind::Activated,
        );

    assert!(matches!(
        *def.effect,
        Effect::Token {
            count: QuantityExpr::Fixed { value: 1 },
            ..
        }
    ));

    let instead = def.sub_ability.as_ref().expect("expected instead branch");
    assert!(matches!(
        instead.condition,
        Some(AbilityCondition::ConditionInstead { ref inner })
            if matches!(
                **inner,
                AbilityCondition::CostPaidObjectMatchesFilter {
                    filter: TargetFilter::Typed(TypedFilter { ref type_filters, .. })
                } if type_filters.iter().any(|filter| {
                    matches!(filter, TypeFilter::Subtype(subtype) if subtype == "Angel")
                })
            )
    ));
    assert!(matches!(
        *instead.effect,
        Effect::Token {
            count: QuantityExpr::Ref {
                qty: QuantityRef::Toughness {
                    scope: ObjectScope::CostPaidObject
                }
            },
            ..
        }
    ));
}

#[test]
fn returned_creatures_can_receive_counters_and_additive_type_followup() {
    let def = parse_effect_chain(
            "Return each creature card from your graveyard to the battlefield with a finality counter on it. Those creatures are Vampires in addition to their other types.",
            AbilityKind::Activated,
        );

    // CR 400.7: "return each ... to the battlefield" is a mass move, so it
    // lowers to `ChangeZoneAll` (not single-target `ChangeZone`) even though
    // a `finality` counter rides along — the counters are threaded through.
    let Effect::ChangeZoneAll {
        enter_with_counters,
        ..
    } = &*def.effect
    else {
        panic!("expected ChangeZoneAll, got {:?}", def.effect);
    };
    assert_eq!(
        enter_with_counters,
        &vec![(CounterType::Finality, QuantityExpr::Fixed { value: 1 },)]
    );

    let subtype_followup = def.sub_ability.as_ref().expect("expected subtype followup");
    assert_eq!(subtype_followup.duration, Some(Duration::Permanent));
    let Effect::GenericEffect {
        static_abilities,
        duration,
        target,
    } = &*subtype_followup.effect
    else {
        panic!("expected GenericEffect, got {:?}", subtype_followup.effect);
    };
    assert_eq!(*duration, Some(Duration::Permanent));
    assert_eq!(*target, None);
    assert!(static_abilities.iter().any(|static_def| {
        matches!(
            static_def.affected,
            Some(TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            })
        ) && static_def.modifications.iter().any(|modification| {
            matches!(
                modification,
                ContinuousModification::AddSubtype { subtype } if subtype == "Vampire"
            )
        }) && !static_def.modifications.iter().any(|modification| {
            matches!(
                modification,
                ContinuousModification::RemoveAllSubtypes { .. }
            )
        })
    }));
}

#[test]
fn countered_creatures_can_receive_tracked_set_keyword_followup() {
    let def = parse_effect_chain(
            "Put a +1/+1 counter on each creature you control. Those creatures gain flying until your next turn.",
            AbilityKind::Activated,
        );

    assert!(
        matches!(&*def.effect, Effect::PutCounterAll { .. }),
        "expected PutCounterAll, got {:?}",
        def.effect
    );

    let keyword_followup = def.sub_ability.as_ref().expect("expected keyword followup");
    let Effect::GenericEffect {
        static_abilities,
        target,
        ..
    } = &*keyword_followup.effect
    else {
        panic!("expected GenericEffect, got {:?}", keyword_followup.effect);
    };
    assert!(matches!(
        target,
        None | Some(TargetFilter::TrackedSet {
            id: TrackedSetId(0)
        })
    ));
    assert!(static_abilities.iter().any(|static_def| {
        matches!(
            static_def.affected,
            Some(TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            })
        ) && static_def.modifications.iter().any(|modification| {
            matches!(
                modification,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Flying
                }
            )
        })
    }));
}

#[test]
fn returned_target_can_receive_contracted_additive_type_followup() {
    let def = parse_effect_chain(
            "Return target creature card from a graveyard to the battlefield under your control. It's a Phyrexian in addition to its other types.",
            AbilityKind::Activated,
        );

    assert!(
        matches!(&*def.effect, Effect::ChangeZone { .. }),
        "expected ChangeZone, got {:?}",
        def.effect
    );

    let subtype_followup = def.sub_ability.as_ref().expect("expected subtype followup");
    assert_eq!(subtype_followup.duration, Some(Duration::Permanent));
    let Effect::GenericEffect {
        static_abilities,
        duration,
        target,
    } = &*subtype_followup.effect
    else {
        panic!("expected GenericEffect, got {:?}", subtype_followup.effect);
    };
    assert_eq!(*duration, Some(Duration::Permanent));
    assert_eq!(*target, Some(TargetFilter::ParentTarget));
    assert!(static_abilities.iter().any(|static_def| {
        static_def.modifications.iter().any(|modification| {
            matches!(
                modification,
                ContinuousModification::AddSubtype { subtype } if subtype == "Phyrexian"
            )
        })
    }));
}

/// std BATCH 12 (Brilliance Unleashed class): a returned permanent followed by
/// a non-additive copula animation — "Return target X ... It's a 3/3 Robot
/// artifact creature with flying" — must lower the animation to a `GenericEffect`
/// bound to `ParentTarget` (the returned object), NOT `Effect::Unimplemented`
/// and NOT `SelfRef`. CR 205.1a + CR 613.1d (Layer 4 type set + Layer 7b base
/// P/T). Revert-discriminating on the `try_parse_contracted_subject_additive_type_clause`
/// animation fallback: without it the followup is `Effect::Unimplemented`.
#[test]
fn returned_target_receives_non_additive_animation_bound_to_parent() {
    let def = parse_effect_chain(
            "Return target artifact card from your graveyard to the battlefield. It's a 3/3 Robot artifact creature with flying.",
            AbilityKind::Activated,
        );
    assert!(
        matches!(&*def.effect, Effect::ChangeZone { .. }),
        "expected ChangeZone head, got {:?}",
        def.effect
    );
    let followup = def
        .sub_ability
        .as_ref()
        .expect("expected animation followup");
    let Effect::GenericEffect {
        static_abilities,
        target,
        ..
    } = &*followup.effect
    else {
        panic!("expected GenericEffect followup, got {:?}", followup.effect);
    };
    assert_eq!(*target, Some(TargetFilter::ParentTarget));
    assert!(static_abilities
        .iter()
        .all(|sd| matches!(sd.affected, Some(TargetFilter::ParentTarget))));
    let mods = &static_abilities[0].modifications;
    assert!(mods
        .iter()
        .any(|m| matches!(m, ContinuousModification::SetPower { value: 3 })));
    assert!(mods.iter().any(|m| matches!(
        m,
        ContinuousModification::AddKeyword {
            keyword: crate::types::keywords::Keyword::Flying
        }
    )));
    assert!(mods.iter().any(|m| matches!(
        m,
        ContinuousModification::AddSubtype { subtype } if subtype == "Robot"
    )));
}

/// std BATCH 12 honest-defer gate: the same non-additive copula animation
/// joined by a bare "and" to an *anaphoric* "Return it" (no fresh typed
/// referent in scope — Brilliance Unleashed's modal-else branch) must NOT
/// silently animate the source permanent. The animation fallback's
/// ParentTarget-bind gate declines, so the conjunct honest-defers to
/// `Effect::unimplemented` rather than producing a wrong `SelfRef` binding.
#[test]
fn anaphoric_return_then_animation_honest_defers_when_no_parent_referent() {
    let def = parse_effect_chain(
            "Otherwise, return it to the battlefield and it's a 3/3 Robot artifact creature with flying.",
            AbilityKind::Activated,
        );
    let mut found_unimplemented = false;
    let mut cursor: Option<&AbilityDefinition> = Some(&def);
    while let Some(node) = cursor {
        if matches!(&*node.effect, Effect::Unimplemented { .. }) {
            found_unimplemented = true;
        }
        // Walk both the sequential sub_ability chain and any else_ability.
        if let Some(else_ab) = &node.else_ability {
            let mut else_cursor: Option<&AbilityDefinition> = Some(else_ab);
            while let Some(en) = else_cursor {
                if matches!(&*en.effect, Effect::Unimplemented { .. }) {
                    found_unimplemented = true;
                }
                else_cursor = en.sub_ability.as_deref();
            }
        }
        cursor = node.sub_ability.as_deref();
    }
    assert!(
        found_unimplemented,
        "anaphoric return + animation with no parent referent must honest-defer \
             to Effect::Unimplemented (not a wrong SelfRef animation), got {def:#?}"
    );
}

#[test]
fn plural_still_lands_retains_land_core_type_not_lands_subtype() {
    let def = parse_effect_chain("They're still lands.", AbilityKind::Activated);
    let Effect::GenericEffect {
        static_abilities, ..
    } = &*def.effect
    else {
        panic!("expected GenericEffect, got {:?}", def.effect);
    };
    let mods = &static_abilities[0].modifications;
    assert!(mods.iter().any(|m| matches!(
        m,
        ContinuousModification::AddType {
            core_type: CoreType::Land
        }
    )));
    assert!(!mods.iter().any(|m| matches!(
        m,
        ContinuousModification::AddSubtype { subtype } if subtype == "Lands"
    )));
}

#[test]
fn leading_cast_from_graveyard_condition_scopes_over_then_put_transformed_chain() {
    let def = parse_effect_chain(
            "If this spell was cast from a graveyard, exile it, then put it onto the battlefield transformed under its owner's control with a finality counter on it.",
            AbilityKind::Spell,
        );

    assert_eq!(
        def.condition,
        Some(AbilityCondition::WasCast {
            zone: Some(Zone::Graveyard)
        })
    );
    assert!(matches!(
        *def.effect,
        Effect::ChangeZone {
            destination: Zone::Exile,
            target: TargetFilter::ParentTarget,
            ..
        }
    ));

    let put = def.sub_ability.as_ref().expect("expected then-put clause");
    assert_eq!(
        put.condition,
        Some(AbilityCondition::WasCast {
            zone: Some(Zone::Graveyard)
        })
    );
    let Effect::ChangeZone {
        destination,
        target,
        enter_transformed,
        enters_under,
        enter_with_counters,
        ..
    } = &*put.effect
    else {
        panic!("expected transformed ChangeZone, got {:?}", put.effect);
    };
    assert_eq!(*destination, Zone::Battlefield);
    assert_eq!(*target, TargetFilter::ParentTarget);
    assert!(
        *enter_transformed,
        "expected transformed battlefield entry, got {:?}",
        put.effect
    );
    assert_eq!(*enters_under, None);
    assert_eq!(
        enter_with_counters,
        &vec![(CounterType::Finality, QuantityExpr::Fixed { value: 1 },)]
    );
}

#[test]
fn sylvan_library_followup_parses_drawn_this_turn_choice() {
    let def = parse_effect_chain(
            "You may draw two additional cards. If you do, choose two cards in your hand drawn this turn. For each of those cards, pay 4 life or put the card on top of your library.",
            AbilityKind::Spell,
        );

    assert!(def.optional);
    assert!(matches!(
        &*def.effect,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            ..
        }
    ));
    let followup = def.sub_ability.as_ref().expect("expected followup");
    assert_eq!(
        followup.condition,
        Some(AbilityCondition::effect_performed())
    );
    assert!(followup.sub_ability.is_none());
    assert!(matches!(
        &*followup.effect,
        Effect::ChooseDrawnThisTurnPayOrTopdeck {
            count: QuantityExpr::Fixed { value: 2 },
            life_payment: QuantityExpr::Fixed { value: 4 },
            player: TargetFilter::Controller,
        }
    ));
}

/// CR 611.2b + CR 703.4q: "Until end of turn, you don't lose unspent red
/// mana as steps and phases end." (The Last Agni Kai) parses as a spell
/// effect that installs a turn-scoped `StepEndUnspentMana { Retain }`
/// static via `Effect::GenericEffect`. The static carries both a `mode`
/// and an `AddStaticMode` modification so `register_transient_effect`
/// can propagate the rule to the controller via `SpecificPlayer`.
#[test]
fn until_end_of_turn_retain_unspent_color_mana_installs_generic_effect() {
    use crate::types::ability::Duration;
    use crate::types::mana::{ManaColor, StepEndManaAction};
    use crate::types::statics::StaticMode;
    let def = parse_effect_chain(
        "Until end of turn, you don't lose unspent red mana as steps and phases end.",
        AbilityKind::Spell,
    );
    let Effect::GenericEffect {
        ref static_abilities,
        duration,
        ..
    } = *def.effect
    else {
        panic!("expected GenericEffect, got {:?}", def.effect);
    };
    assert_eq!(duration, Some(Duration::UntilEndOfTurn));
    assert_eq!(static_abilities.len(), 1);
    assert_eq!(
        static_abilities[0].mode,
        StaticMode::StepEndUnspentMana {
            filter: Some(ManaColor::Red),
            action: StepEndManaAction::Retain,
        }
    );
    assert_eq!(static_abilities[0].affected, Some(TargetFilter::Controller));
}

// Crafty Cutpurse parser regression. Pre-fix the trigger effect parsed as
// `Effect::Unimplemented { name: "each", ... }` because no specialized
// parser recognized the controller-redirect phrasing — so even though the
// engine's replacement pipeline can express the redirect, the trigger
// never installed it. This test pins the parser shape end-to-end.
#[test]
fn crafty_cutpurse_oracle_text_parses_to_token_controller_redirect() {
    let e = parse_effect(
            "each token that would be created under an opponent's control this turn is created under your control instead",
        );
    let Effect::AddTargetReplacement {
        replacement,
        target,
    } = e
    else {
        panic!("expected AddTargetReplacement, got {e:?}");
    };
    assert_eq!(target, TargetFilter::SelfRef);
    assert_eq!(replacement.event, ReplacementEvent::CreateToken);
    assert_eq!(replacement.token_owner_scope, Some(ControllerRef::Opponent));
    assert_eq!(replacement.token_owner_redirect, Some(ControllerRef::You));
    assert_eq!(
        replacement.expiry,
        Some(RestrictionExpiry::EndOfTurn),
        "Crafty Cutpurse's redirect is bounded to 'this turn' — must expire at EOT"
    );
}

/// CR 608.2c + CR 109.4 (issue #409): Gluntch, the Bestower's end-step
/// "choose a player … choose a second player to … choose a third player
/// to …" chain decomposes into three `Choose(Player)` nodes. The dependent
/// effects bind to the chosen player via `ControllerRef::ChosenPlayer`, and
/// the 2nd/3rd choose clauses no longer fall back to `Unimplemented`.
#[test]
fn strax_choose_a_player_at_random_records_random_selection() {
    // CR 608.2d (override): Strax, Sontaran Nurse — "Choose a player at
    // random. When you do, ~ fights another target creature that player
    // controls." The Choose(Player) must record TargetSelectionMode::Random
    // and keep the dependent reflexive Fight as a WhenYouDo sub.
    let def = parse_effect_chain(
        "Choose a player at random. When you do, Strax fights another target \
             creature that player controls.",
        AbilityKind::Spell,
    );
    match def.effect.as_ref() {
        Effect::Choose {
            choice_type: ChoiceType::Player,
            selection,
            ..
        } => assert_eq!(*selection, TargetSelectionMode::Random),
        other => panic!("expected random Choose(Player), got {other:?}"),
    }
}

#[test]
fn gluntch_choose_player_chain_parses_with_chosen_player_scopes() {
    let def = parse_effect_chain(
        "choose a player. They put two +1/+1 counters on a creature they \
             control. Choose a second player to draw a card. Then choose a \
             third player to create two Treasure tokens.",
        AbilityKind::Spell,
    );

    // Node 0: the first `Choose(Player)`.
    assert!(
        matches!(
            def.effect.as_ref(),
            Effect::Choose {
                choice_type: ChoiceType::Player,
                ..
            }
        ),
        "first node must be Choose(Player), got {:?}",
        def.effect
    );

    // Node 1: PutCounter on a creature controlled by the 1st chosen player.
    let node1 = def.sub_ability.as_ref().expect("PutCounter node");
    let Effect::PutCounter { target, .. } = node1.effect.as_ref() else {
        panic!("node 1 must be PutCounter, got {:?}", node1.effect);
    };
    let TargetFilter::Typed(tf) = target else {
        panic!("PutCounter target must be Typed, got {0:?}", target);
    };
    assert_eq!(
        tf.controller,
        Some(ControllerRef::ChosenPlayer { index: 0 }),
        "the +1/+1 counters go on a creature the 1st chosen player controls"
    );

    // Node 2: the second `Choose(Player)`.
    let node2 = node1.sub_ability.as_ref().expect("2nd Choose node");
    assert!(
        matches!(
            node2.effect.as_ref(),
            Effect::Choose {
                choice_type: ChoiceType::Player,
                ..
            }
        ),
        "node 2 must be Choose(Player) — not Unimplemented — got {:?}",
        node2.effect
    );

    // Node 3: Draw by the 2nd chosen player.
    let node3 = node2.sub_ability.as_ref().expect("Draw node");
    let Effect::Draw { target, .. } = node3.effect.as_ref() else {
        panic!("node 3 must be Draw, got {:?}", node3.effect);
    };
    assert_eq!(
        target.chosen_player_index(),
        Some(1),
        "the 2nd chosen player draws the card"
    );

    // Node 4: the third `Choose(Player)`.
    let node4 = node3.sub_ability.as_ref().expect("3rd Choose node");
    assert!(
        matches!(
            node4.effect.as_ref(),
            Effect::Choose {
                choice_type: ChoiceType::Player,
                ..
            }
        ),
        "node 4 must be Choose(Player) — not Unimplemented — got {:?}",
        node4.effect
    );

    // Node 5: Treasure tokens owned by the 3rd chosen player.
    let node5 = node4.sub_ability.as_ref().expect("Token node");
    let Effect::Token { owner, .. } = node5.effect.as_ref() else {
        panic!("node 5 must be Token, got {:?}", node5.effect);
    };
    assert_eq!(
        owner.chosen_player_index(),
        Some(2),
        "the 3rd chosen player creates (owns) the Treasure tokens"
    );
}

/// Issue #534 — Skullwinder's ETB trigger must decompose into the ordered
/// chain `ChangeZone` (caster's graveyard) → `Choose { Opponent }` → `ChangeZone`
/// whose target filter carries `FilterProp::Owned { ChosenPlayer { 0 } }`.
/// The pre-fix parser dropped "then choose an opponent" entirely and left
/// the dependent return scoped to `ScopedPlayer`, which falls back to the
/// caster — the agency bug. CR 608.2c (rules of English: "That player" is
/// the just-chosen opponent) + CR 109.4 (the returned card is *owned*).
#[test]
fn skullwinder_etb_parses_choose_opponent() {
    let parsed = crate::parser::oracle::parse_oracle_text(
        "Deathtouch\nWhen this creature enters, return target card from your \
             graveyard to your hand, then choose an opponent. That player returns \
             a card from their graveyard to their hand.",
        "Skullwinder",
        &[],
        &["Creature".to_string()],
        &["Snake".to_string()],
    );

    let trigger = parsed
        .triggers
        .first()
        .expect("Skullwinder has an ETB trigger");
    let chain = trigger
        .execute
        .as_ref()
        .expect("ETB trigger has an execute chain");

    // Node 1: return the caster's own graveyard card.
    assert!(
        matches!(
            chain.effect.as_ref(),
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Hand,
                ..
            }
        ),
        "node 1 must return caster's graveyard card to hand, got {:?}",
        chain.effect
    );

    // Node 2: choose an opponent.
    let choose = chain
        .sub_ability
        .as_ref()
        .expect("node 2 — Choose(Opponent)");
    assert!(
        matches!(
            choose.effect.as_ref(),
            Effect::Choose {
                choice_type: ChoiceType::Opponent { .. },
                ..
            }
        ),
        "node 2 must be Choose {{ Opponent }} — not dropped/Unimplemented — got {:?}",
        choose.effect
    );

    // Node 3: the chosen opponent returns a card from THEIR graveyard.
    let return_to_hand = choose
        .sub_ability
        .as_ref()
        .expect("node 3 — chosen player's return");
    let Effect::ChangeZone {
        origin: Some(Zone::Graveyard),
        destination: Zone::Hand,
        target,
        ..
    } = return_to_hand.effect.as_ref()
    else {
        panic!(
            "node 3 must return from graveyard to hand, got {:?}",
            return_to_hand.effect
        );
    };
    let TargetFilter::Typed(tf) = target else {
        panic!(
            "node 3 return target must be a Typed filter, got {0:?}",
            target
        );
    };
    assert!(
        tf.properties.iter().any(|prop| matches!(
            prop,
            FilterProp::Owned {
                controller: ControllerRef::ChosenPlayer { index: 0 }
            }
        )),
        "node 3 return filter must scope ownership to ChosenPlayer {{ index: 0 }}, \
             got properties {:?}",
        tf.properties
    );
    // No ScopedPlayer ref may survive anywhere — that is the wrong-player bug.
    assert!(
        tf.controller != Some(ControllerRef::ScopedPlayer)
            && !tf.properties.iter().any(|prop| matches!(
                prop,
                FilterProp::Owned {
                    controller: ControllerRef::ScopedPlayer
                }
            )),
        "no ScopedPlayer ref may survive in the chain, got {tf:?}"
    );
}

// -----------------------------------------------------------------------
// Balance equalization parser arms (try_parse_balance_equalization)
// -----------------------------------------------------------------------

/// Assert a `Difference { ObjectCount(Land, You), ControlledByEachPlayer }`
/// sacrifice link with `player_scope: All`.
fn assert_land_sacrifice_clause(def: &AbilityDefinition) {
    assert_eq!(def.player_scope, Some(PlayerFilter::All));
    let Effect::Sacrifice { target, count, .. } = &*def.effect else {
        panic!("expected Effect::Sacrifice, got {:?}", def.effect);
    };
    assert!(
        matches!(
            target,
            TargetFilter::Typed(tf)
                if tf.controller == Some(ControllerRef::You)
                && tf.type_filters == vec![TypeFilter::Land]
        ),
        "sacrifice target must be lands you control, got {0:?}",
        target
    );
    let QuantityExpr::Difference { left, right } = count else {
        panic!("sacrifice count must be a Difference, got {count:?}");
    };
    // CR 109.5: the LEFT per-player count is re-scoped to `ScopedPlayer` so
    // it reads the iterating player at the `resolve_ref` seam, not the
    // caster. A `You` LEFT operand is the regression: every player would cut
    // to the caster's count − min instead of their own.
    assert!(
        matches!(
            &**left,
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(tf)
                }
            } if tf.controller == Some(ControllerRef::ScopedPlayer)
        ),
        "LEFT count operand must be ObjectCount scoped to ScopedPlayer, got {left:?}"
    );
    // CR 107.1 + CR 608.2e: the RIGHT minimum keeps `You` — it builds its own
    // per-player context (`from_ability_with_controller(a, p.id)` + the
    // `obj.controller == p.id` gate). `ScopedPlayer` here would collapse to
    // the iterating player and zero the cross-player minimum.
    assert!(
        matches!(
            &**right,
            QuantityExpr::Ref {
                qty: QuantityRef::ControlledByEachPlayer {
                    aggregate: AggregateFunction::Min,
                    filter: TargetFilter::Typed(tf),
                }
            } if tf.controller == Some(ControllerRef::You)
        ),
        "RIGHT minimum operand must be ControlledByEachPlayer(Min) scoped to You, got {right:?}"
    );
}

/// Assert a `Difference { HandSize(ScopedPlayer), HandSize(AllPlayers Min) }`
/// discard link with `player_scope: All`.
fn assert_hand_discard_clause(def: &AbilityDefinition) {
    assert_eq!(def.player_scope, Some(PlayerFilter::All));
    let Effect::Discard { target, count, .. } = &*def.effect else {
        panic!("expected Effect::Discard, got {:?}", def.effect);
    };
    assert_eq!(*target, TargetFilter::Controller);
    let QuantityExpr::Difference { left, right } = count else {
        panic!("discard count must be a Difference, got {count:?}");
    };
    assert!(matches!(
        &**left,
        QuantityExpr::Ref {
            qty: QuantityRef::HandSize {
                player: PlayerScope::ScopedPlayer
            }
        }
    ));
    assert!(matches!(
        &**right,
        QuantityExpr::Ref {
            qty: QuantityRef::HandSize {
                player: PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Min,
                    ..
                }
            }
        }
    ));
}

#[test]
fn balance_parses_to_three_link_equalization_chain() {
    // Arm B (sacrifice lands) + Arm C (discard cards, sacrifice creatures).
    let def = parse_effect_chain(
            "Each player chooses a number of lands they control equal to the number of lands controlled by the player who controls the fewest, then sacrifices the rest. Players discard cards and sacrifice creatures the same way.",
            AbilityKind::Spell,
        );
    // Link 1: sacrifice lands.
    assert_land_sacrifice_clause(&def);
    // Link 2: discard cards.
    let link2 = def.sub_ability.as_ref().expect("expected discard clause");
    assert_hand_discard_clause(link2);
    // Link 3: sacrifice creatures.
    let link3 = link2
        .sub_ability
        .as_ref()
        .expect("expected creature sacrifice clause");
    assert_eq!(link3.player_scope, Some(PlayerFilter::All));
    let Effect::Sacrifice { target, .. } = &*link3.effect else {
        panic!("link 3 must be Effect::Sacrifice, got {:?}", link3.effect);
    };
    assert!(matches!(
        target,
        TargetFilter::Typed(tf)
            if tf.controller == Some(ControllerRef::You)
            && tf.type_filters == vec![TypeFilter::Creature]
    ));
    // The chain ends after three links.
    assert!(link3.sub_ability.is_none(), "chain must be exactly 3 links");
}

#[test]
fn restore_balance_reversed_clause_order_parses() {
    // Restore Balance reverses the "the same way" clause order
    // (sacrifice creatures before discard cards) — the `alt()` over the
    // verb handles it for free.
    let def = parse_effect_chain(
            "Each player chooses a number of lands they control equal to the number of lands controlled by the player who controls the fewest, then sacrifices the rest. Players sacrifice creatures and discard cards the same way.",
            AbilityKind::Spell,
        );
    assert_land_sacrifice_clause(&def);
    let link2 = def.sub_ability.as_ref().expect("expected clause 2");
    // Reversed: clause 2 is the creature sacrifice.
    assert!(matches!(&*link2.effect, Effect::Sacrifice { .. }));
    let link3 = link2.sub_ability.as_ref().expect("expected clause 3");
    assert_hand_discard_clause(link3);
    assert!(link3.sub_ability.is_none());
}

#[test]
fn balancing_act_single_continuation_clause_parses() {
    // Balancing Act: "Each player" subject + a single "the same way" clause.
    let def = parse_effect_chain(
            "Each player chooses a number of permanents they control equal to the number of permanents controlled by the player who controls the fewest, then sacrifices the rest. Each player discards cards the same way.",
            AbilityKind::Spell,
        );
    // Link 1: sacrifice permanents.
    assert_eq!(def.player_scope, Some(PlayerFilter::All));
    assert!(matches!(&*def.effect, Effect::Sacrifice { .. }));
    // Link 2: the single discard continuation.
    let link2 = def.sub_ability.as_ref().expect("expected discard clause");
    assert_hand_discard_clause(link2);
    assert!(link2.sub_ability.is_none(), "chain must be exactly 2 links");
}

#[test]
fn non_balance_text_is_not_intercepted() {
    // The interceptor must not fire on unrelated "each player" text.
    assert!(
        try_parse_balance_equalization("Each player draws a card.", AbilityKind::Spell).is_none(),
        "interceptor must only match the Balance equalization shape"
    );
}

#[test]
fn balance_arm_b_rejects_non_equalization_quantity() {
    // CR 107.1b: Arm B must REQUIRE the equalization-shape quantity
    // (`ControlledByEachPlayer { aggregate: Min, .. }`) — a superficially
    // similar phrase whose inner quantity is unrelated (here: a hand-size
    // ref) must NOT be intercepted. Confirms Arm B verifies the structure
    // rather than discarding the parsed ref.
    assert!(
            try_parse_balance_equalization(
                "Each player chooses a number of lands they control equal to the number of cards in their hand, then sacrifices the rest. Players discard cards and sacrifice creatures the same way.",
                AbilityKind::Spell,
            )
            .is_none(),
            "interceptor must reject inputs whose inner quantity is not the equalization shape"
        );
}

/// GitHub issue #1504 — Baleful Mastery: "an opponent draws a card" must not
/// require targeting an opponent at cast; opponent is chosen on resolution.
#[test]
fn baleful_mastery_opponent_draw_uses_choose_not_cast_target() {
    let text = "If the {1}{B} cost was paid, an opponent draws a card. Exile target creature or planeswalker.";
    let def = parse_effect_chain(text, AbilityKind::Spell);

    // Chain order: conditional opponent draw is the head; exile is sub_ability.
    assert!(
        matches!(
            def.effect.as_ref(),
            Effect::Choose {
                choice_type: ChoiceType::Opponent { .. },
                ..
            }
        ),
        "opponent draw must be Choose(Opponent), got {:?}",
        def.effect
    );
    assert!(
        matches!(
            def.condition,
            Some(AbilityCondition::AlternativeManaCostPaid)
        ),
        "draw must be gated on alternative mana cost payment, got {:?}",
        def.condition
    );
    let draw_effect = def
        .sub_ability
        .as_ref()
        .expect("Choose should chain to Draw");
    let Effect::Draw { target, .. } = draw_effect.effect.as_ref() else {
        panic!("expected Draw sub-ability, got {:?}", draw_effect.effect);
    };
    assert!(
        target.chosen_player_index() == Some(0),
        "Draw must target ChosenPlayer {{0}}, got {0:?}",
        target
    );
    let exile = draw_effect
        .sub_ability
        .as_ref()
        .expect("Draw should chain to exile");
    assert!(
        matches!(exile.effect.as_ref(), Effect::ChangeZone { .. }),
        "exile must be chained after draw, got {:?}",
        exile.effect
    );
}

#[test]
fn named_choice_recognizes_enumerated_card_types() {
    // Issue #930 — Cloud Key's older templating enumerates the card-type
    // options ("choose artifact, creature, enchantment, instant, or
    // sorcery") instead of the modern "choose a card type". Both must map
    // to ChoiceType::CardType so the chosen type persists for downstream
    // IsChosenCardType reads (CR 205.2).
    assert!(matches!(
        try_parse_named_choice("choose artifact, creature, enchantment, instant, or sorcery"),
        Some(ChoiceType::CardType { .. })
    ));
    assert!(matches!(
        try_parse_named_choice("choose a card type"),
        Some(ChoiceType::CardType { .. })
    ));
    // Trailing period, as it appears in oracle text.
    assert!(matches!(
        try_parse_named_choice("choose artifact, creature, enchantment, instant, or sorcery."),
        Some(ChoiceType::CardType { .. })
    ));
}

#[test]
fn named_choice_enumeration_does_not_misfire() {
    // Non-card-type lists and articled / creature-type choices must not be
    // treated as a card-type enumeration.
    assert!(!is_card_type_enumeration("one or more creatures"));
    assert!(!is_card_type_enumeration("a creature"));
    assert!(!matches!(
        try_parse_named_choice("choose a creature type"),
        Some(ChoiceType::CardType { .. })
    ));
}
