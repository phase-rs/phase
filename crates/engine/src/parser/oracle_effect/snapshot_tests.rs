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
    assert!(
        change_zone.face_down_in_exile.is_face_down(),
        "face-down SearchLibrary exile must use the typed intent carrier"
    );
    match &*change_zone.effect {
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Exile,
            target: TargetFilter::Any,
            face_down_profile: None,
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

/// CR 400.1 + CR 201.2: the same-name graveyard-return tail must follow the
/// DESTINATION the card names, not one baked into the recognizer.
///
/// Echoing Return — "Return target creature card and all other cards with the
/// same name as that card from your graveyard to your hand." — is the card this
/// fixes. The recognizer matched a literal marker ending "... to the
/// battlefield", so a hand destination failed the match outright and the entire
/// same-name clause was dropped: the spell returned its single target and left
/// every other copy in the graveyard, with no parse warning to show for it.
///
/// Revert-failing: restore "to the battlefield" into the marker string and the
/// `to your hand` case below loses its tail (`sub_ability` is `None`).
///
/// The `to your hand tapped` case is the paired negative. CR 110.5b + CR 110.5d:
/// 110.5b sets the default entry state, and 110.5d is the rule that actually makes
/// status battlefield-only ("cards not on the battlefield are neither tapped nor
/// untapped"), so "tapped" is admitted only on the battlefield arm. The repo pairs
/// these two for exactly this proposition at
/// `game/conditions.rs::eval_source_is_tapped_on_battlefield` (the zone-guarded tap
/// predicate) and its scope-parameterized sibling `StaticCondition::IsTapped` in
/// `types/ability.rs`.
#[test]
fn same_name_graveyard_return_follows_the_named_destination() {
    let to_hand = parse_effect_chain(
        "Return target creature card and all other cards with the same name as that card from your graveyard to your hand.",
        AbilityKind::Spell,
    );
    let Effect::ChangeZone { destination, .. } = &*to_hand.effect else {
        panic!("expected a primary ChangeZone, got {:?}", to_hand.effect);
    };
    assert_eq!(
        *destination,
        Zone::Hand,
        "primary must follow the card's text"
    );

    let tail = to_hand
        .sub_ability
        .as_ref()
        .expect("the same-name tail must survive a hand destination");
    let Effect::ChangeZoneAll {
        origin,
        destination,
        target,
        enter_tapped,
        ..
    } = &*tail.effect
    else {
        panic!("expected a ChangeZoneAll tail, got {:?}", tail.effect);
    };
    assert_eq!(*origin, Some(Zone::Graveyard));
    assert_eq!(
        *destination,
        Zone::Hand,
        "the tail must land where the target does"
    );
    // CR 110.5d: a card in hand has no tapped status, so the hand arm must carry
    // no entry state at all — neither Tapped nor an explicit Untapped.
    assert!(
        enter_tapped.is_unspecified(),
        "a hand destination has no entry tap state, got {enter_tapped:?}"
    );
    let TargetFilter::Typed(tf) = target else {
        panic!("expected a typed tail filter, got {target:?}");
    };
    assert!(
        tf.properties.contains(&FilterProp::SameNameAsParentTarget),
        "the tail is the same-NAME group, got {:?}",
        tf.properties
    );

    // Reach-guard: the battlefield destination this recognizer already served
    // (Rat King, Verminister) must be untouched, so the assertions above are
    // about the DESTINATION and not about the tail being broken outright.
    let to_battlefield = parse_effect_chain(
        "Return target creature card and all other cards with the same name as that card from your graveyard to the battlefield tapped.",
        AbilityKind::Spell,
    );
    let Effect::ChangeZoneAll {
        destination,
        enter_tapped,
        ..
    } = &*to_battlefield
        .sub_ability
        .as_ref()
        .expect("battlefield tail must still be built")
        .effect
    else {
        panic!("expected a ChangeZoneAll tail for the battlefield case");
    };
    assert_eq!(*destination, Zone::Battlefield);
    assert!(enter_tapped.is_tapped(), "tapped must still reach the tail");

    // Paired negative, CR 110.5b + CR 110.5d: status is battlefield-only, so this
    // recognizer must decline rather than invent a tapped hand entry.
    let nonsense = parse_effect_chain(
        "Return target creature card and all other cards with the same name as that card from your graveyard to your hand tapped.",
        AbilityKind::Spell,
    );
    assert!(
        nonsense.sub_ability.is_none(),
        "a tapped hand destination must not parse through this recognizer, got {:?}",
        nonsense.sub_ability
    );

    // ...and the refusal must come from the DESTINATION ARM itself, not from the
    // caller's `all_consuming` rejecting leftover residue. Those are different
    // failures: the second would still accept "to your hand tapped" anywhere the
    // caller happened to be laxer about the tail.
    assert!(
        super::parse_same_name_return_destination("your hand tapped.").is_err(),
        "the pairing match must refuse a tapped hand destination on its own"
    );
    assert!(
        super::parse_same_name_return_destination("the battlefield tapped.").is_ok(),
        "reach-guard: the battlefield arm must still admit tapped"
    );
    // CR 110.5b: without "tapped" the battlefield arm sets no entry override.
    assert_eq!(
        super::parse_same_name_return_destination("the battlefield.")
            .ok()
            .map(|(_, parsed)| parsed),
        Some((
            Zone::Battlefield,
            crate::types::zones::EtbTapState::Unspecified
        )),
        "an unqualified battlefield destination enters with no tap override"
    );
    // Bounded on purpose: zones this recognizer does not model must decline here
    // rather than receive a confidently wrong ChangeZoneAll.
    for unmodelled in [
        "your library.",
        "exile.",
        "your graveyard.",
        "the command zone.",
    ] {
        assert!(
            super::parse_same_name_return_destination(unmodelled).is_err(),
            "{unmodelled:?} is outside the modelled destinations and must decline"
        );
    }
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
        enters_attacking: false,
        enter_with_counters: _,
        face_down_profile: None,
        library_position: None,
        library_shuffle: _,
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
        end_cost: _,
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
        end_cost: _,
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
            choice_type:
                ChoiceType::Player {
                    distinctness: PlayerChoiceDistinctness::Independent,
                },
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

    // Node 0: the first `Choose(Player)` — no ordinal, so the default
    // `Independent` distinctness applies (CR 608.2c; issue #6381 confirms the
    // bare "choose a player" must NOT exclude prior choices).
    assert!(
        matches!(
            def.effect.as_ref(),
            Effect::Choose {
                choice_type: ChoiceType::Player {
                    distinctness: PlayerChoiceDistinctness::Independent
                },
                ..
            }
        ),
        "first node must be Choose(Player) with Independent distinctness, got {:?}",
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

    // Node 2: the second `Choose(Player)` — "a second player" carries the
    // ordinal, so it must be `DistinctFromPriorChoices` (Gluntch's "three
    // distinct players" ruling).
    let node2 = node1.sub_ability.as_ref().expect("2nd Choose node");
    assert!(
        matches!(
            node2.effect.as_ref(),
            Effect::Choose {
                choice_type: ChoiceType::Player {
                    distinctness: PlayerChoiceDistinctness::DistinctFromPriorChoices
                },
                ..
            }
        ),
        "node 2 must be Choose(Player) with DistinctFromPriorChoices — not Unimplemented — got {:?}",
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

    // Node 4: the third `Choose(Player)` — "a third player" carries the
    // ordinal, so it must be `DistinctFromPriorChoices` too.
    let node4 = node3.sub_ability.as_ref().expect("3rd Choose node");
    assert!(
        matches!(
            node4.effect.as_ref(),
            Effect::Choose {
                choice_type: ChoiceType::Player {
                    distinctness: PlayerChoiceDistinctness::DistinctFromPriorChoices
                },
                ..
            }
        ),
        "node 4 must be Choose(Player) with DistinctFromPriorChoices — not Unimplemented — got {:?}",
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
// Balance equalization parser arms (parse_balance_equalization_ir)
// -----------------------------------------------------------------------

/// Assert a `Difference { ObjectCount(Land, You), ControlledByEachPlayer }`
/// sacrifice link with `player_scope: All`.
fn assert_land_sacrifice_clause(def: &AbilityDefinition) {
    assert_eq!(def.player_scope, Some(PlayerFilter::All));
    assert_eq!(def.sub_link, SubAbilityLink::SequentialSibling);
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
                    relation: PlayerRelation::All,
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
    assert_eq!(def.sub_link, SubAbilityLink::SequentialSibling);
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
    assert_eq!(link3.sub_link, SubAbilityLink::SequentialSibling);
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
        parse_balance_equalization_ir(
            "Each player draws a card.",
            AbilityKind::Spell,
            &ParseContext::default(),
        )
        .is_none(),
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
            parse_balance_equalization_ir(
                "Each player chooses a number of lands they control equal to the number of cards in their hand, then sacrifices the rest. Players discard cards and sacrifice creatures the same way.",
                AbilityKind::Spell,
                &ParseContext::default(),
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
fn named_choice_keeps_enumerated_card_types_as_source_ordered_labels() {
    let canonical =
        try_parse_named_choice("choose artifact, creature, enchantment, instant, or sorcery");
    assert_eq!(
        canonical,
        Some(ChoiceType::Labeled {
            options: vec![
                "Artifact".to_string(),
                "Creature".to_string(),
                "Enchantment".to_string(),
                "Instant".to_string(),
                "Sorcery".to_string(),
            ],
        })
    );

    let reordered =
        try_parse_named_choice("choose sorcery, artifact, instant, creature, or enchantment");
    assert_eq!(
        reordered,
        Some(ChoiceType::Labeled {
            options: vec![
                "Sorcery".to_string(),
                "Artifact".to_string(),
                "Instant".to_string(),
                "Creature".to_string(),
                "Enchantment".to_string(),
            ],
        })
    );

    assert_eq!(
        try_parse_named_choice("choose a card type"),
        Some(ChoiceType::card_type())
    );
}

#[test]
fn named_choice_enumeration_does_not_misfire() {
    for choice in [
        "choose artifact, creature, or sorcery",
        "choose artifact, creature, enchantment, instant, or land",
        "choose artifact, creature, enchantment, instant, or planeswalker",
        "choose artifact, creature, enchantment, instant, or artifact",
        "choose artifact, creature, enchantment, instant, or mystery",
    ] {
        assert!(
            matches!(
                try_parse_named_choice(choice),
                Some(ChoiceType::Labeled { .. })
            ),
            "{choice:?} must retain the labeled-choice fallback"
        );
    }
    assert!(matches!(
        try_parse_named_choice("choose a creature type"),
        Some(ChoiceType::CreatureType { .. })
    ));
}

fn chain_effects(def: &AbilityDefinition) -> Vec<&Effect> {
    std::iter::successors(Some(def), |def| def.sub_ability.as_deref())
        .map(|def| def.effect.as_ref())
        .collect()
}

fn chain_shuffle_target(def: &AbilityDefinition) -> &TargetFilter {
    chain_effects(def)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Shuffle { target } => Some(target),
            _ => None,
        })
        .expect("chain carries a shuffle")
}

fn parse_card(
    oracle: &str,
    name: &str,
    keywords: &[&str],
    types: &[&str],
) -> crate::parser::oracle::ParsedAbilities {
    let owned = |items: &[&str]| {
        items
            .iter()
            .map(|item| item.to_string())
            .collect::<Vec<_>>()
    };
    crate::parser::oracle::parse_oracle_text(oracle, name, &owned(keywords), &owned(types), &[])
}

/// CR 608.2c + CR 701.24a: "that player shuffles" names the searched opponent.
#[test]
fn search_target_opponent_then_that_player_shuffles_names_the_declared_player() {
    let parsed = parse_card(
        "Search target opponent's library for a card and exile it face down. Then that player shuffles. You may play that card for as long as it remains exiled.",
        "Praetor's Grasp",
        &[],
        &["Sorcery"],
    );
    assert_eq!(chain_shuffle_target(&parsed.abilities[0]), &declared());
}

/// CR 608.2c + CR 701.24a: a trigger's "that player shuffles" names the searched opponent,
/// not the triggering player.
#[test]
fn trigger_search_then_that_player_shuffles_names_the_declared_player() {
    let parsed = parse_card(
        "Prowl {2}{B} (You may cast this for its prowl cost if you dealt combat damage to a player this turn with a Goblin or Rogue.)\nWhen this creature enters, if its prowl cost was paid, search target opponent's library for three cards and exile them. Then that player shuffles.",
        "Earwig Squad",
        &["Prowl"],
        &["Creature"],
    );
    let execute = parsed.triggers[0].execute.as_deref().expect("trigger body");
    assert_eq!(chain_shuffle_target(execute), &declared());
}

/// CR 608.2c: a search of a scoped player, not a declared target, keeps its shuffle on that player.
#[test]
fn scoped_player_search_then_shuffles_keeps_the_scoped_player() {
    let parsed = parse_card(
        "Players can't draw cards or gain life.\nAt the beginning of each player's draw step, that player loses 3 life, searches their library for a card, puts it into their hand, then shuffles.",
        "Mornsong Aria",
        &[],
        &["Enchantment"],
    );
    let execute = parsed.triggers[0].execute.as_deref().expect("trigger body");
    assert!(chain_effects(execute).into_iter().any(|effect| matches!(
        effect,
        Effect::SearchLibrary {
            target_player: Some(TargetFilter::ScopedPlayer),
            ..
        }
    )));
    assert_eq!(chain_shuffle_target(execute), &TargetFilter::ScopedPlayer);
}

fn declared() -> TargetFilter {
    TargetFilter::DeclaredPlayer {
        group: ChosenGroupId::declared_player(0),
    }
}

fn card_effects(parsed: &crate::parser::oracle::ParsedAbilities) -> Vec<&Effect> {
    parsed
        .abilities
        .iter()
        .chain(parsed.triggers.iter().filter_map(|t| t.execute.as_deref()))
        .flat_map(chain_effects)
        .collect()
}

/// The player a `Draw`/`Discard`/`Shuffle`/`GainLife`/`LoseLife`/search/`Token` names.
fn player_field(effect: &Effect) -> Option<&TargetFilter> {
    match effect {
        Effect::Draw { target, .. }
        | Effect::Discard { target, .. }
        | Effect::Shuffle { target } => Some(target),
        Effect::GainLife { player, .. } => Some(player),
        Effect::LoseLife { target, .. } => target.as_ref(),
        Effect::SearchLibrary { target_player, .. } => target_player.as_ref(),
        Effect::Token { owner, .. } => Some(owner),
        _ => None,
    }
}

/// Player fields that surface a target slot of their own.
fn declared_player_fields(effects: &[&Effect]) -> usize {
    effects
        .iter()
        .filter_map(|e| player_field(e))
        .filter(|f| matches!(f, TargetFilter::Player | TargetFilter::Typed(_)))
        .count()
}

fn last_draw<'a>(effects: &[&'a Effect]) -> &'a TargetFilter {
    effects
        .iter()
        .rev()
        .find_map(|e| match e {
            Effect::Draw { target, .. } => Some(target),
            _ => None,
        })
        .expect("chain draws")
}

fn shuffle_target<'a>(effects: &[&'a Effect]) -> &'a TargetFilter {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::Shuffle { target } => Some(target),
            _ => None,
        })
        .expect("chain shuffles")
}

fn no_unimplemented(effects: &[&Effect]) -> bool {
    !effects
        .iter()
        .any(|e| matches!(e, Effect::Unimplemented { .. }))
}

/// CR 115.1 + CR 608.2c: the search declares the player once; "that player shuffles, then
/// draws" name that slot instead of cloning the filter into a second target.
#[test]
fn search_declared_player_carries_its_slot_to_shuffle_and_draw() {
    for (name, oracle, types) in [
        (
            "Unmoored Ego",
            "Choose a card name. Search target opponent's graveyard, hand, and library for up to four cards with that name and exile them. That player shuffles, then draws a card for each card exiled from their hand this way.",
            &["Sorcery"],
        ),
        (
            "Lost Legacy",
            "Choose a nonartifact, nonland card name. Search target player's graveyard, hand, and library for any number of cards with that name and exile them. That player shuffles, then draws a card for each card exiled from their hand this way.",
            &["Sorcery"],
        ),
        (
            "The Stone Brain",
            "{2}, {T}, Exile The Stone Brain: Choose a card name. Search target opponent's graveyard, hand, and library for up to four cards with that name and exile them. That player shuffles, then draws a card for each card exiled from their hand this way. Activate only as a sorcery.",
            &["Artifact"],
        ),
    ] {
        let parsed = parse_card(oracle, name, &[], types);
        let effects = card_effects(&parsed);
        assert!(
            effects.iter().any(|e| matches!(
                e,
                Effect::SearchLibrary {
                    target_player: Some(TargetFilter::Player | TargetFilter::Typed(_)),
                    ..
                }
            )),
            "{name}: declaring search present, got {effects:?}"
        );
        assert_eq!(shuffle_target(&effects), &declared(), "{name}");
        assert_eq!(last_draw(&effects), &declared(), "{name}");
        assert_eq!(declared_player_fields(&effects), 1, "{name}: {effects:?}");
    }
}

/// CR 115.1 + CR 701.24a: a declared "target player searches ... then shuffles" shuffles that slot.
#[test]
fn declared_player_search_shuffle_names_the_declared_player() {
    for (name, oracle, keywords, types) in [
        (
            "Fertilid's Favor",
            "Target player searches their library for a basic land card, puts it onto the battlefield tapped, then shuffles. Put two +1/+1 counters on up to one target artifact or creature.",
            &[][..],
            &["Instant"][..],
        ),
        (
            "Fertilid",
            "This creature enters with two +1/+1 counters on it.\n{1}{G}, Remove a +1/+1 counter from this creature: Target player searches their library for a basic land card, puts it onto the battlefield tapped, then shuffles.",
            &[][..],
            &["Creature"][..],
        ),
        (
            "Varragoth, Bloodsky Sire",
            "Deathtouch\nBoast — {1}{B}: Target player searches their library for a card, then shuffles and puts that card on top. (Activate only if this creature attacked this turn and only once each turn.)",
            &["Deathtouch"][..],
            &["Creature"][..],
        ),
    ] {
        let parsed = parse_card(oracle, name, keywords, types);
        let effects = card_effects(&parsed);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::SearchLibrary { .. })),
            "{name}: search present, got {effects:?}"
        );
        assert_eq!(shuffle_target(&effects), &declared(), "{name}");
    }
}

/// CR 111.2 + CR 115.1: the tokens go to the searched player's slot, not the caster.
#[test]
fn necromentia_tokens_belong_to_the_searched_player() {
    let parsed = parse_card(
        "Choose a card name other than a basic land card name. Search target opponent's graveyard, hand, and library for any number of cards with that name and exile them. That player shuffles, then creates a 2/2 black Zombie creature token for each card exiled from their hand this way.",
        "Necromentia",
        &[],
        &["Sorcery"],
    );
    let effects = card_effects(&parsed);
    assert!(effects
        .iter()
        .any(|e| matches!(e, Effect::SearchLibrary { .. })));
    let owner = effects
        .iter()
        .find_map(|e| match e {
            Effect::Token { owner, .. } => Some(owner),
            _ => None,
        })
        .expect("chain creates tokens");
    assert_eq!(owner, &declared());
}

/// CR 115.1 + CR 701.23a: a carried declared player that searches its own library is the
/// declared target's controller reference, the form `searcher_is_library_owner` accepts.
#[test]
fn restorative_technique_declared_player_searches_and_shuffles_its_own_library() {
    let parsed = parse_card(
        "Target player gains 2 life, then searches their library for a basic land card, puts it onto the battlefield tapped, then shuffles. Put a +1/+1 counter on up to one target creature.",
        "Restorative Technique",
        &[],
        &["Sorcery"],
    );
    let effects = card_effects(&parsed);
    assert!(matches!(
        effects[0],
        Effect::GainLife {
            player: TargetFilter::Player,
            ..
        }
    ));
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::SearchLibrary {
            target_player: Some(TargetFilter::DeclaredPlayer { .. }),
            ..
        }
    )));
    assert_eq!(shuffle_target(&effects), &declared());
    assert_eq!(declared_player_fields(&effects), 1, "{effects:?}");
}

/// CR 608.2c: "gains life" after a declared player gains life names that player; a declaring
/// effect with no player filter (Devour Flesh's sacrifice) keeps the `ParentTarget` read.
#[test]
fn declared_player_gains_life_continuation_names_the_declared_player() {
    for (name, oracle, expected) in [
        (
            "Life Burst",
            "Target player gains 4 life, then gains 4 life for each card named Life Burst in each graveyard.",
            declared(),
        ),
        (
            "Devour Flesh",
            "Target player sacrifices a creature of their choice, then gains life equal to that creature's toughness.",
            TargetFilter::ParentTarget,
        ),
    ] {
        let parsed = parse_card(oracle, name, &[], &["Instant"]);
        let effects = card_effects(&parsed);
        let gains: Vec<&TargetFilter> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::GainLife { player, .. } => Some(player),
                _ => None,
            })
            .collect();
        assert!(!gains.is_empty(), "{name}: {effects:?}");
        assert_eq!(
            *gains.last().unwrap(),
            &expected,
            "{name}: {effects:?}"
        );
    }
}

/// CR 608.2c + CR 115.1: after a reflexive "choose a card from it" the node's own targets are the
/// chosen card, so "that player ... then draws/discards" names the declared player's slot.
#[test]
fn reflexive_choice_continuation_names_the_declared_player() {
    for (name, oracle, keywords, types) in [
        (
            "Oildeep Gearhulk",
            "Lifelink, ward {1}\nWhen this creature enters, look at target player's hand. You may choose a card from it. If you do, that player discards that card, then draws a card.",
            &["Lifelink"][..],
            &["Artifact", "Creature"][..],
        ),
        (
            "Revealing Eye",
            "Menace\nWhen this creature transforms into Revealing Eye, target opponent reveals their hand. You may choose a nonland card from it. If you do, that player discards that card, then draws a card.",
            &["Menace"][..],
            &["Creature"][..],
        ),
        (
            "Salt Vampire",
            "Lifelink\nWhen this creature enters, look at target opponent's hand. You may choose a nonland card from it. If you do, that player exiles that card, then draws a card.",
            &["Lifelink"][..],
            &["Creature"][..],
        ),
        (
            "Memory Worm",
            "Paradox — Whenever you cast a spell from anywhere other than your hand, this creature deals 2 damage to target player. That player discards a card, then draws a card. Put a +1/+1 counter on this creature.",
            &[][..],
            &["Creature"][..],
        ),
    ] {
        let parsed = parse_card(oracle, name, keywords, types);
        let effects = card_effects(&parsed);
        assert!(
            effects.iter().any(|e| matches!(
                e,
                Effect::RevealHand { .. } | Effect::DealDamage { .. }
            )),
            "{name}: declaring node present, got {effects:?}"
        );
        assert!(no_unimplemented(&effects), "{name}: {effects:?}");
        assert_eq!(last_draw(&effects), &declared(), "{name}: {effects:?}");
        if name != "Memory Worm" {
            let chosen_card = effects
                .iter()
                .find_map(|e| match e {
                    Effect::DiscardCard { target, .. } | Effect::ChangeZone { target, .. }
                        if *target == TargetFilter::ParentTarget =>
                    {
                        Some(target)
                    }
                    _ => None,
                })
                .expect("the chosen-card node keeps ParentTarget");
            assert_eq!(chosen_card, &TargetFilter::ParentTarget);
        }
    }
}

/// CR 608.2c: Tourach's Canticle's second discard is the revealed opponent's, not the caster's.
#[test]
fn tourachs_canticle_second_discard_names_the_declared_player() {
    let parsed = parse_card(
        "Target opponent reveals their hand. You choose a card from it. That player discards that card, then discards a card at random.",
        "Tourach's Canticle",
        &[],
        &["Sorcery"],
    );
    let effects = card_effects(&parsed);
    assert!(effects
        .iter()
        .any(|e| matches!(e, Effect::RevealHand { .. })));
    assert!(no_unimplemented(&effects), "{effects:?}");
    let discard = effects
        .iter()
        .rev()
        .find_map(|e| match e {
            Effect::Discard { target, .. } => Some(target),
            _ => None,
        })
        .expect("chain discards at random");
    assert_eq!(discard, &declared());
}

/// CR 608.2c: Vendilion Clique's chain is coverage-red by design; only its `Draw` is asserted.
#[test]
fn vendilion_clique_draw_names_the_declared_player() {
    let parsed = parse_card(
        "Flash\nFlying\nWhen Vendilion Clique enters, look at target player's hand. You may choose a nonland card from it. If you do, that player reveals the chosen card, puts it on the bottom of their library, then draws a card.",
        "Vendilion Clique",
        &["Flash", "Flying"],
        &["Creature"],
    );
    let effects = card_effects(&parsed);
    assert!(effects
        .iter()
        .any(|e| matches!(e, Effect::RevealHand { .. })));
    assert_eq!(last_draw(&effects), &declared());
}

/// CR 608.2c: an anaphor that names an event player, and no declaring clause, keeps that
/// player for the continuation.
#[test]
fn event_player_anaphor_continuation_keeps_its_own_reference() {
    for (name, oracle, keywords, types, expected) in [
        (
            "Barbed Shocker",
            "Trample, haste\nWhenever this creature deals damage to a player, that player discards all the cards in their hand, then draws that many cards.",
            &["Trample", "Haste"][..],
            &["Creature"][..],
            TargetFilter::TriggeringPlayer,
        ),
        (
            "Robber Fly",
            "Flying\nWhenever this creature becomes blocked, defending player discards all the cards in their hand, then draws that many cards.",
            &["Flying"][..],
            &["Creature"][..],
            TargetFilter::DefendingPlayer,
        ),
    ] {
        let parsed = parse_card(oracle, name, keywords, types);
        let effects = card_effects(&parsed);
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Discard { .. })),
            "{name}: {effects:?}"
        );
        assert_eq!(last_draw(&effects), &expected, "{name}: {effects:?}");
    }
}

/// CR 603.2 + CR 608.2c: the searching opponent, not the trigger's controller, loses the life.
#[test]
fn ob_nixilis_life_loss_names_the_searching_player() {
    let parsed = parse_card(
        "Flying, trample\nWhenever an opponent searches their library, that player sacrifices a creature of their choice and loses 10 life.\nWhenever another creature dies, put a +1/+1 counter on Ob Nixilis.",
        "Ob Nixilis, Unshackled",
        &["Flying", "Trample"],
        &["Creature"],
    );
    let effects = card_effects(&parsed);
    assert!(effects
        .iter()
        .any(|e| matches!(e, Effect::Sacrifice { .. })));
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::LoseLife {
            target: Some(TargetFilter::TriggeringPlayer),
            ..
        }
    )));
}

/// CR 608.2c: "that player" names the printed subject, not a player an earlier clause declared,
/// when the continuation's own subject is a different anaphor. Constructed text.
#[test]
fn a_different_anaphor_subject_does_not_take_the_declared_player() {
    let parsed = parse_card(
        "Target player draws a card. Defending player discards a card, then draws a card.",
        "Synthetic",
        &[],
        &["Sorcery"],
    );
    let effects = card_effects(&parsed);
    assert!(matches!(
        effects[0],
        Effect::Draw {
            target: TargetFilter::Player,
            ..
        }
    ));
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::Discard {
            target: TargetFilter::DefendingPlayer,
            ..
        }
    )));
    assert_eq!(last_draw(&effects), &TargetFilter::DefendingPlayer);
}

/// CR 608.2c: a continuation after an instead-body keeps the `ParentTarget` the Declared
/// carry supplies; Careful Consideration's declared player has no reflexive choice in between.
#[test]
fn careful_consideration_instead_body_keeps_parent_target() {
    let parsed = parse_card(
        "Target player draws four cards, then discards three cards. If you cast this spell during your main phase, instead that player draws four cards, then discards two cards.",
        "Careful Consideration",
        &[],
        &["Instant"],
    );
    let effects = card_effects(&parsed);
    assert!(matches!(
        effects[0],
        Effect::Draw {
            target: TargetFilter::Player,
            ..
        }
    ));
    let last_discard = effects
        .iter()
        .rev()
        .find_map(|e| match e {
            Effect::Discard { target, .. } => Some(target),
            _ => None,
        })
        .expect("chain discards");
    assert_eq!(last_discard, &TargetFilter::ParentTarget);
}

/// CR 608.2c: the carry changes nothing for chains that never declared a searched player.
#[test]
fn player_reference_controls_keep_their_lowering() {
    let stone = parse_card(
        "If a creature an opponent controls would die, exile it instead.\n{2}, {T}, Sacrifice Stone of Erech: Exile target player's graveyard. Draw a card.",
        "Stone of Erech",
        &[],
        &["Artifact"],
    );
    let stone_effects = card_effects(&stone);
    assert!(stone_effects
        .iter()
        .any(|e| matches!(e, Effect::ChangeZoneAll { .. })));
    assert_eq!(last_draw(&stone_effects), &TargetFilter::Controller);

    let looter = parse_card(
        "{T}: Target player draws a card, then discards a card.",
        "Cephalid Looter",
        &[],
        &["Creature"],
    );
    let looter_effects = card_effects(&looter);
    assert!(matches!(
        looter_effects[0],
        Effect::Draw {
            target: TargetFilter::Player,
            ..
        }
    ));
    assert!(looter_effects.iter().any(|e| matches!(
        e,
        Effect::Discard {
            target: TargetFilter::DeclaredPlayer { .. },
            ..
        }
    )));

    let knowledge = parse_card(
        "Prowl {3}{U} (You may cast this for its prowl cost if you dealt combat damage to a player this turn with a Rogue.)\nSearch target opponent's library for an instant or sorcery card. You may cast that card without paying its mana cost. Then that player shuffles.",
        "Knowledge Exploitation",
        &["Prowl"],
        &["Kindred", "Sorcery"],
    );
    assert_eq!(shuffle_target(&card_effects(&knowledge)), &declared());

    let betrayal = parse_card(
        "Exile all opponents' graveyards. You may cast spells from among those cards this turn, and mana of any type can be spent to cast them. At the beginning of the next end step, if any of those cards remain exiled, return them to their owners' graveyards.\nExile Mnemonic Betrayal.",
        "Mnemonic Betrayal",
        &[],
        &["Sorcery"],
    );
    let betrayal_effects = card_effects(&betrayal);
    assert!(betrayal_effects.iter().any(|e| matches!(
        e,
        Effect::ChangeZoneAll {
            target: TargetFilter::Typed(_),
            ..
        }
    )));
    assert!(!betrayal_effects.iter().any(|e| matches!(
        e,
        Effect::ChangeZoneAll {
            target: TargetFilter::ParentTargetSlot { .. },
            ..
        }
    )));
}

/// CR 608.2c + CR 115.1: "that player" names the declaring clause whatever target slot it
/// occupies, so a player declared after an object target is not read through slot 0 (the creature).
#[test]
fn player_declared_after_an_object_target_names_the_declared_player() {
    let parsed = parse_card(
        "Destroy target creature. This spell deals 2 damage to target player. That player discards a card, then draws a card.",
        "Object First Anaphor",
        &[],
        &["Sorcery"],
    );
    let effects = card_effects(&parsed);
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::DealDamage {
                target: TargetFilter::Player,
                ..
            }
        )),
        "player declaration reached: {effects:?}"
    );
    assert!(no_unimplemented(&effects), "{effects:?}");
    assert_eq!(last_draw(&effects), &declared(), "{effects:?}");
}

/// CR 608.2c: a search-declared player after an object target is likewise named, not slot-read.
#[test]
fn search_declared_player_after_an_object_target_names_the_declared_player() {
    let parsed = parse_card(
        "Destroy target creature. Search target opponent's library for a card and exile it. That player shuffles.",
        "Object First Search",
        &[],
        &["Sorcery"],
    );
    let effects = card_effects(&parsed);
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SearchLibrary {
                target_player: Some(_),
                ..
            }
        )),
        "declaring search reached: {effects:?}"
    );
    assert_eq!(shuffle_target(&effects), &declared(), "{effects:?}");
}

/// CR 115.1 + CR 601.2c: a non-targeting earlier clause (Sacrifice) declares nothing, so the
/// declared player is the only one named.
#[test]
fn player_declared_after_a_non_targeting_clause_names_the_declared_player() {
    let parsed = parse_card(
        "You sacrifice a creature. Target player gains 2 life. That player discards a card, then draws a card.",
        "Sacrifice First Anaphor",
        &[],
        &["Sorcery"],
    );
    let effects = card_effects(&parsed);
    assert!(no_unimplemented(&effects), "{effects:?}");
    assert_eq!(last_draw(&effects), &declared(), "{effects:?}");
    assert_eq!(declared_player_fields(&effects), 1, "{effects:?}");
}

/// CR 115.1 + CR 601.2c: the same holds when the declaring clause is a search.
#[test]
fn search_declared_player_after_a_non_targeting_clause_names_the_declared_player() {
    let parsed = parse_card(
        "Sacrifice a creature. Target opponent searches their library for a card and exiles it. That player shuffles.",
        "Sacrifice First Search",
        &[],
        &["Sorcery"],
    );
    let effects = card_effects(&parsed);
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SearchLibrary {
                target_player: Some(_),
                ..
            }
        )),
        "declaring search reached: {effects:?}"
    );
    assert_eq!(shuffle_target(&effects), &declared(), "{effects:?}");
    assert_eq!(declared_player_fields(&effects), 1, "{effects:?}");
}

/// Control: the same anaphor shape with the player declared first names the declared player.
#[test]
fn player_declared_first_names_the_declared_player() {
    let parsed = parse_card(
        "Target player gains 2 life. That player discards a card, then draws a card.",
        "Player First Anaphor",
        &[],
        &["Sorcery"],
    );
    let effects = card_effects(&parsed);
    assert!(no_unimplemented(&effects), "{effects:?}");
    assert_eq!(last_draw(&effects), &declared(), "{effects:?}");
    assert_eq!(declared_player_fields(&effects), 1, "{effects:?}");
}

/// Every `DeclaredPlayer` group and every declaring-node tag in `value`, counted per group id.
fn declared_player_groups(
    value: &serde_json::Value,
    reads: &mut std::collections::BTreeMap<u64, usize>,
    tags: &mut std::collections::BTreeMap<u64, usize>,
) {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("type").and_then(|t| t.as_str()) == Some("DeclaredPlayer") {
                *reads.entry(map["group"].as_u64().unwrap()).or_default() += 1;
            }
            if let Some(group) = map.get("declares_chosen_group").and_then(|g| g.as_u64()) {
                if ChosenGroupId(group as u32).is_declared_player() {
                    *tags.entry(group).or_default() += 1;
                }
            }
            map.values()
                .for_each(|v| declared_player_groups(v, reads, tags));
        }
        serde_json::Value::Array(items) => items
            .iter()
            .for_each(|v| declared_player_groups(v, reads, tags)),
        _ => {}
    }
}

/// CR 608.2c + CR 115.1: each ability's `DeclaredPlayer` reads name groups that exactly one
/// tagged declaring node carries, modal modes and reflexive, search and else-branch shapes included.
#[test]
fn declared_player_reads_name_exactly_one_tagged_declaration() {
    let mut reads_seen = 0;
    for (name, types, oracle) in [
        ("Unmoored Ego", &["Sorcery"][..], "Choose a card name. Search target opponent's graveyard, hand, and library for up to four cards with that name and exile them. That player shuffles, then draws a card for each card exiled from their hand this way."),
        ("Fertilid's Favor", &["Instant"][..], "Target player searches their library for a basic land card, puts it onto the battlefield tapped, then shuffles. Put two +1/+1 counters on up to one target artifact or creature."),
        ("Chain of Smog", &["Sorcery"], "Target player discards two cards. That player may copy this spell and may choose a new target for that copy."),
        ("The Ancient One", &["Creature"], "Descend 8 — The Ancient One can't attack or block unless there are eight or more permanent cards in your graveyard.\n{2}{U}{B}: Draw a card, then discard a card. When you discard a card this way, target player mills cards equal to its mana value."),
        ("Shadrix Silverquill", &["Creature"], "Flying, double strike\nAt the beginning of combat on your turn, you may choose two. Each mode must target a different player.\n• Target player creates a 2/1 white and black Inkling creature token with flying.\n• Target player draws a card and loses 1 life.\n• Target player puts a +1/+1 counter on each creature they control."),
        ("Browbeat", &["Sorcery"], "Any player may have Browbeat deal 5 damage to them. If no one does, target player draws three cards."),
        ("Praetor's Grasp", &["Sorcery"], "Search target opponent's library for a card and exile it face down. Then that player shuffles. You may play that card for as long as it remains exiled."),
        ("Oildeep Gearhulk", &["Artifact", "Creature"], "Lifelink, ward {1}\nWhen this creature enters, look at target player's hand. You may choose a card from it. If you do, that player discards that card, then draws a card."),
        ("Careful Consideration", &["Instant"], "Target player draws four cards, then discards three cards. If you cast this spell during your main phase, instead that player draws four cards, then discards two cards."),
        ("Restorative Technique", &["Sorcery"], "Target player gains 2 life, then searches their library for a basic land card, puts it onto the battlefield tapped, then shuffles. Put a +1/+1 counter on up to one target creature."),
        ("Eternal Dominion", &["Sorcery"], "Search target opponent's library for an artifact, creature, enchantment, or land card. Put that card onto the battlefield under your control. Then that player shuffles.\nEpic (For the rest of the game, you can't cast spells. At the beginning of each of your upkeeps, copy this spell except for its epic ability. You may choose a new target for the copy.)"),
        ("Necromentia", &["Sorcery"], "Choose a card name other than a basic land card name. Search target opponent's graveyard, hand, and library for any number of cards with that name and exile them. That player shuffles, then creates a 2/2 black Zombie creature token for each card exiled from their hand this way."),
        ("Vendilion Clique", &["Creature"], "Flash\nFlying\nWhen Vendilion Clique enters, look at target player's hand. You may choose a nonland card from it. If you do, that player reveals the chosen card, puts it on the bottom of their library, then draws a card."),
        ("Book Burning", &["Sorcery"], "Any player may have Book Burning deal 6 damage to them. If no one does, target player mills six cards."),
        ("Revealing Eye", &["Creature"], "Menace\nWhen this creature transforms into Revealing Eye, target opponent reveals their hand. You may choose a nonland card from it. If you do, that player discards that card, then draws a card."),
        ("Salt Vampire", &["Creature"], "Lifelink\nWhen this creature enters, look at target opponent's hand. You may choose a nonland card from it. If you do, that player exiles that card, then draws a card."),
        ("Kitesail Freebooter", &["Creature"], "Flying\nWhen this creature enters, target opponent reveals their hand. You choose a noncreature, nonland card from it. Exile that card until this creature leaves the battlefield."),
        ("Ghost-Lit Stalker", &["Creature"], "{4}{B}, {T}: Target player discards two cards. Activate only as a sorcery.\nChannel — {5}{B}{B}, Discard this card: Target player discards four cards. Activate only as a sorcery."),
        ("Undercity Plunder", &["Sorcery"], "Target opponent discards a card. Then they may discard an additional card. If they don't, conjure a duplicate of a random card from their library into your hand. It perpetually gains \"You may spend mana as though it were mana of any color to cast this spell.\""),
    ] {
        let parsed = serde_json::to_value(parse_card(oracle, name, &[], types)).unwrap();
        for key in ["abilities", "triggers"] {
            for (index, root) in parsed[key].as_array().unwrap().iter().enumerate() {
                let (mut reads, mut tags) = Default::default();
                declared_player_groups(root, &mut reads, &mut tags);
                reads_seen += reads.len();
                for group in reads.keys() {
                    assert_eq!(tags.get(group), Some(&1), "{name} {key}[{index}] group {group}");
                }
                assert!(tags.values().all(|&count| count == 1), "{name} {key}[{index}]: {tags:?}");
            }
        }
    }
    assert!(
        reads_seen >= 10,
        "the class texts reach DeclaredPlayer readers"
    );
}

/// CR 608.2c: a declaring effect with no player filter keeps its `ParentTarget` read instead of
/// taking a declaration.
#[test]
fn declared_player_reference_is_refused_where_the_declaration_cannot_be_tagged() {
    let parsed = serde_json::to_value(parse_card(
        "Target player sacrifices a creature of their choice, then gains life equal to that creature's toughness.",
        "Devour Flesh",
        &[],
        &["Instant"],
    ))
    .unwrap();
    let (mut reads, mut tags) = Default::default();
    declared_player_groups(&parsed, &mut reads, &mut tags);
    assert!(reads.is_empty(), "{reads:?}");
}

/// CR 608.2c + CR 608.2d: a bare "they" after a declared player reads that player's group, as
/// reader and as the "may" actor.
#[test]
fn a_they_after_a_declared_player_reads_the_declaring_clause() {
    let parsed = serde_json::to_value(parse_card(
        "Target opponent discards a card. Then they may discard an additional card.",
        "Row",
        &[],
        &["Sorcery"],
    ))
    .unwrap();
    let declaring = &parsed["abilities"][0];
    let reader = &declaring["sub_ability"];
    let group = &declaring["declares_chosen_group"];
    assert!(group.is_u64(), "the declaring clause is tagged");
    for slot in [&reader["effect"]["target"], &reader["optional_player"]] {
        assert_eq!(slot["type"], "DeclaredPlayer");
        assert_eq!(&slot["group"], group);
    }
}

/// CR 608.2d: a "may" reading a chosen-clause declaration reads the chosen player's group, as
/// reader and as the "may" actor.
#[test]
fn a_they_may_reading_a_chosen_clause_declaration_names_the_chosen_player_as_actor() {
    let parsed = serde_json::to_value(parse_card(
        "Choose target player. They may discard up to X cards. Then they draw a card for each card discarded this way.",
        "Mode",
        &[],
        &["Sorcery"],
    ))
    .unwrap();
    let declaring = &parsed["abilities"][0];
    let reader = &declaring["sub_ability"];
    for slot in [&reader["effect"]["target"], &reader["optional_player"]] {
        assert_eq!(slot["type"], "DeclaredPlayer");
        assert_eq!(slot["group"], declaring["declares_chosen_group"]);
    }
    assert!(reader["optional"].as_bool().unwrap());
}

/// CR 608.2c: a bare "they" names one declared player only when the clause before it announces
/// exactly one; two target players ("Parker Luck") and "any other target" (Screaming Nemesis) leave
/// "they" unlinked.
#[test]
fn a_they_after_a_plural_or_any_target_declaration_is_not_a_declared_player_reader() {
    // (linked `DeclaredPlayer` reads, `ParentTargetController` fallbacks)
    let counts = |name: &str, types: &[&str], oracle: &str| {
        let parsed = serde_json::to_value(parse_card(oracle, name, &[], types)).unwrap();
        let (mut reads, mut tags) = Default::default();
        declared_player_groups(&parsed, &mut reads, &mut tags);
        (
            reads.values().sum::<usize>(),
            parsed.to_string().matches("ParentTargetController").count(),
        )
    };
    let (reach_reads, _) = counts(
        "Reach Guard",
        &["Sorcery"],
        "Target player gains 2 life. They draw a card.",
    );
    assert!(
        reach_reads > 0,
        "one declared player: the they-reader is linked"
    );
    // An unlinked "they" parses to `ParentTargetController`, not a `DeclaredPlayer` read.
    let (reads, ptc) = counts(
        "Parker Luck",
        &["Enchantment"],
        "At the beginning of your end step, two target players each reveal the top card of their library. They each lose life equal to the mana value of the card revealed by the other player. Then they each put the card they revealed into their hand.",
    );
    assert_eq!(reads + ptc, 0);
    let (reads, ptc) = counts(
        "Screaming Nemesis",
        &["Creature"],
        "Haste\nWhenever this creature is dealt damage, it deals that much damage to any other target. If a player is dealt damage this way, they can't gain life for the rest of the game.",
    );
    assert_eq!(reads + ptc, 0);
}

/// CR 608.2c + CR 115.1: a player-declaring clause carries its tag whether or not a later clause
/// reads it; the runtime keys "this node declares its own player" on the tag alone.
#[test]
fn every_player_declaring_clause_carries_a_distinct_tag() {
    let parsed = serde_json::to_value(parse_card(
        "Target player draws a card. Target opponent loses 2 life.",
        "Two Slots",
        &[],
        &["Sorcery"],
    ))
    .unwrap();
    let (mut reads, mut tags) = Default::default();
    declared_player_groups(&parsed, &mut reads, &mut tags);
    assert!(reads.is_empty());
    assert_eq!(tags.len(), 2, "{tags:?}");
}
