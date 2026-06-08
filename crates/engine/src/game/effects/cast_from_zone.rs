use crate::game::zones;
use crate::types::ability::{
    CastingPermission, Duration, Effect, EffectError, EffectKind, ResolvedAbility, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::mana::ManaCost;
use crate::types::zones::Zone;

/// CR 601.2a + CR 118.9: Cast a card from a zone without paying its mana cost.
///
/// Grants a `CastingPermission::ExileWithAltCost` on the target card(s),
/// following the same pattern as Discover (CR 701.57a). If the card is not
/// already in exile, it is moved there first — the casting pipeline expects
/// cards with exile-cast permissions to be in the exile zone.
///
/// After granting the permission, the resolver returns and the player receives
/// priority. They can then cast the card via the normal `GameAction::CastSpell`
/// flow, which handles target selection (CR 601.2c), modal choices, X costs,
/// additional costs, and all other casting steps.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (
        target_filter,
        without_paying,
        cast_transformed,
        alt_ability_cost,
        constraint,
        duration,
        driver,
    ) = match &ability.effect {
        Effect::CastFromZone {
            target,
            without_paying_mana_cost,
            cast_transformed,
            alt_ability_cost,
            constraint,
            duration,
            driver,
            ..
        } => (
            target,
            *without_paying_mana_cost,
            *cast_transformed,
            alt_ability_cost.clone(),
            constraint.clone(),
            duration.clone(),
            *driver,
        ),
        _ => return Err(EffectError::MissingParam("CastFromZone".to_string())),
    };

    // Collect target object IDs from the resolved ability's targets.
    let mut target_ids: Vec<_> = ability
        .targets
        .iter()
        .filter_map(|t| {
            if let TargetRef::Object(id) = t {
                Some(*id)
            } else {
                None
            }
        })
        .collect();

    if target_ids.is_empty() && target_filter.references_exiled_by_source() {
        let ctx = crate::game::filter::FilterContext::from_ability(ability);
        target_ids = crate::game::players::linked_exile_cards_for_source(state, ability.source_id)
            .iter()
            .map(|link| link.exiled_id)
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.zone == Zone::Exile)
                    && crate::game::filter::matches_target_filter(state, *id, target_filter, &ctx)
            })
            .collect();
    }

    if target_ids.is_empty() {
        // No targets resolved — nothing to cast.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::CastFromZone,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 702.62a + CR 608.2g: Suspend's last-time-counter ability casts the
    // card it is attached to, for free, AS THE TRIGGER RESOLVES. The card casts
    // itself (the single resolved target IS the ability's source), there is no
    // mana cost (`without_paying`), and no replacement alt-cost
    // (`alt_ability_cost == None`). Per CR 702.62a/702.62d the cast happens
    // during resolution — it must NOT be deferred to a lingering permission the
    // player acts on at a later priority window (issue #1520: accepting the
    // optional "cast it?" prompt appeared to do nothing because only a
    // permission was stamped — the spell was never put on the stack, and a
    // sorcery like Treasure Cruise was additionally blocked by the
    // sorcery-speed timing gate at upkeep). Drive the cast immediately through
    // the same cast-during-resolution authority Cascade/Discover use
    // (`initiate_cast_during_resolution`).
    //
    // The router reads the EXPLICIT `driver` discriminator
    // (`CastFromZoneDriver::DuringResolution`, set by
    // `build_suspend_last_counter_cast_trigger`), NOT `duration`. `duration` is
    // CR 611.2a permission-expiry and says nothing about the casting mechanism;
    // routing on it conflated two axes. The structural-shape guard
    // (`without_paying` + no alt-cost + single self target) is retained as a
    // defense-in-depth invariant — a `DuringResolution` body must always be a
    // self-free-cast, since `initiate_cast_during_resolution` casts the single
    // card object itself at zero cost.
    //
    // FOLLOW-UP (#1520 twin): Rebound (CR 702.88a) is still a
    // `LingeringPermission` driver because its recast permission legitimately
    // needs `duration: Some(UntilEndOfTurn)` to prune on decline (see the
    // `consuming_vapors_rebound` suite). A rebounding SORCERY recast at upkeep
    // therefore still passes through the lingering path; whether it hits the
    // sorcery-speed gate is tracked separately. Routing Rebound through
    // `DuringResolution` would regress that durational-prune contract, so it is
    // intentionally left on the permission path under the explicit `driver`
    // signal rather than forced through during-resolution here.
    //
    // Nashi/Jeleva-style "you may cast [other] exiled cards" (target != source,
    // `ExiledBySource` filter, or an `alt_ability_cost`) are also
    // `LingeringPermission`: the controller casts them during the granting
    // effect's own priority window.
    let self_free_cast = driver.is_during_resolution()
        && without_paying
        && alt_ability_cost.is_none()
        && target_ids.len() == 1
        && target_ids[0] == ability.source_id;
    if self_free_cast {
        let card = target_ids[0];
        // CR 601.2a: ensure the card is in exile before the cast (it already is
        // for Suspend/Rebound; this mirrors the permission path's invariant).
        if state.objects.get(&card).map(|o| o.zone) != Some(Zone::Exile) {
            zones::move_to_zone(state, card, Zone::Exile, events);
        }
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::CastFromZone,
            source_id: ability.source_id,
        });
        // CR 702.62d / CR 601.2b: casting as an effect follows the alternative-
        // cost rules. `initiate_cast_during_resolution` grants the zero-cost
        // `ExileWithAltCost` permission, prepares the cast (the Suspend variant
        // is detected by `prepare_spell_cast`'s effective-keyword scan), and
        // continues it on `Auto` payment. The returned `WaitingFor` (target
        // selection if the spell targets, else priority with the spell on the
        // stack) becomes the resolution's pending prompt.
        //
        // CR 608.2g: the cast happens DURING resolution, so the sorcery-speed /
        // empty-stack / active-player timing gates must NOT apply (Treasure
        // Cruise is a sorcery cast at upkeep, with the trigger still on the
        // stack). The during-resolution `ResolutionCastCleanup` marker keys that
        // timing bypass in `restrictions::check_spell_timing`. There are no dig
        // misses, and CR 702.62a's "if you don't, it remains exiled" disposition
        // is `RemainExiled` (only reached if a future free-cast adds an MV gate;
        // Suspend carries none).
        let cleanup = crate::types::ability::ResolutionCastCleanup {
            exiled_misses: Vec::new(),
            reject_action: crate::types::ability::ResolutionMvRejectAction::RemainExiled,
            success_action: crate::types::ability::ResolutionCastSuccessAction::BottomMisses,
        };
        state.waiting_for = crate::game::casting::initiate_cast_during_resolution(
            state,
            ability.controller,
            card,
            constraint.clone(),
            cast_transformed,
            cleanup,
            events,
        )
        .map_err(|e| EffectError::InvalidParam(e.to_string()))?;
        return Ok(());
    }

    for &obj_id in &target_ids {
        // CR 601.2a: Impulse-draw and similar grants move non-exile cards to
        // exile before attaching `ExileWithAltCost`. Targeted graveyard grants
        // (Emry, Lurker in the Loch) keep the card in the graveyard and grant
        // a durational permission the casting pipeline consumes in place.
        let current_zone = state.objects.get(&obj_id).map(|o| o.zone);
        if current_zone.is_some_and(|z| z != Zone::Exile && z != Zone::Graveyard) {
            zones::move_to_zone(state, obj_id, Zone::Exile, events);
        }

        // CR 118.9: Grant casting permission. Three cases:
        //   - `alt_ability_cost: Some(_)` → `ExileWithAltAbilityCost` (Nashi:
        //     "pay life equal to its mana value rather than paying its mana
        //     cost" — non-mana alt cost replaces the mana cost).
        //   - `without_paying_mana_cost: true` → `ExileWithAltCost { zero }`
        //     (Discover, Suspend, "without paying its mana cost").
        //   - otherwise → `ExileWithAltCost { mana_cost }` (Nashi-style "you
        //     may play one of those cards" with normal mana payment).
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            // CR 611.2a + CR 118.9: The cast-from-zone effect is granted by an
            // ability whose controller is the player allowed to cast the
            // exiled card. Without this binding, an `ExileWithAltCost` on a
            // card owned by another player would fall back to the
            // `obj.owner == player` rule in `has_exile_cast_permission` and
            // surface the cast option to the wrong player. Jeleva, Nephalia's
            // Scourge exiles cards from each opponent's library on ETB; the
            // attack trigger's cast permission must be scoped to Jeleva's
            // controller, not to each card's owner.
            let granted_to = Some(ability.controller);
            let permission = if let Some(cost) = alt_ability_cost.clone() {
                CastingPermission::ExileWithAltAbilityCost {
                    cost,
                    constraint: constraint.clone(),
                    granted_to,
                }
            } else {
                let cost = if without_paying {
                    ManaCost::zero()
                } else {
                    obj.mana_cost.clone()
                };
                CastingPermission::ExileWithAltCost {
                    cost,
                    cast_transformed,
                    constraint: constraint.clone(),
                    granted_to,
                    resolution_cleanup: None,
                    // CR 611.2a: continuous-effect duration plumbing.
                    // CR 702.88a: Rebound's upkeep recast permission expires.
                    // Forward `duration` from the `Effect::CastFromZone` so
                    // durational grants (Rebound's `UntilEndOfTurn` upkeep
                    // recast offer) are pruned at the correct boundary.
                    // `None` (the common case) preserves the standing
                    // semantics used by Discover, Suspend, Nashi, etc.
                    // Emry-class graveyard grants default to UntilEndOfTurn
                    // when the parser did not carry an explicit duration.
                    duration: duration.clone().or_else(|| {
                        (current_zone == Some(Zone::Graveyard)).then_some(Duration::UntilEndOfTurn)
                    }),
                }
            };
            if !obj.casting_permissions.contains(&permission) {
                obj.casting_permissions.push(permission);
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CastFromZone,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        CardPlayMode, CastPermissionConstraint, Comparator, Effect, QuantityExpr, TargetFilter,
        TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{ExileLink, ExileLinkKind};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_test_state() -> GameState {
        GameState::new_two_player(42)
    }

    fn add_card_to_exile(state: &mut GameState, owner: PlayerId, card_id: CardId) -> ObjectId {
        let obj_id = create_object(state, card_id, owner, "Test Spell".to_string(), Zone::Exile);
        state.objects.get_mut(&obj_id).unwrap().mana_cost = ManaCost::generic(3);
        obj_id
    }

    fn add_card_to_hand(state: &mut GameState, owner: PlayerId, card_id: CardId) -> ObjectId {
        let obj_id = create_object(state, card_id, owner, "Hand Spell".to_string(), Zone::Hand);
        state.objects.get_mut(&obj_id).unwrap().mana_cost = ManaCost::generic(2);
        obj_id
    }

    fn add_card_to_graveyard(state: &mut GameState, owner: PlayerId, card_id: CardId) -> ObjectId {
        let obj_id = create_object(
            state,
            card_id,
            owner,
            "Graveyard Artifact".to_string(),
            Zone::Graveyard,
        );
        state.objects.get_mut(&obj_id).unwrap().mana_cost = ManaCost::zero();
        obj_id
    }

    #[test]
    fn graveyard_target_grant_stays_in_graveyard_with_timed_permission() {
        let mut state = make_test_state();
        let obj_id = add_card_to_graveyard(&mut state, PlayerId(0), CardId(400));

        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::ParentTarget,
                without_paying_mana_cost: false,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: Some(Duration::UntilEndOfTurn),
                driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.zone, Zone::Graveyard);
        assert!(obj.casting_permissions.iter().any(|p| matches!(
            p,
            CastingPermission::ExileWithAltCost {
                cost,
                duration: Some(Duration::UntilEndOfTurn),
                granted_to: Some(PlayerId(0)),
                ..
            } if *cost == ManaCost::zero()
        )));
    }

    /// Issue #1520 — suspend last-time-counter free cast must actually CAST the
    /// card as the trigger resolves (CR 702.62a), not merely grant a lingering
    /// `ExileWithAltCost` permission that the player has to act on later. The
    /// reported bug: removing the last time counter from a suspended Treasure
    /// Cruise prompts "cast it?", but accepting does nothing — the spell is
    /// never put on the stack because the resolver only stamped a permission.
    ///
    /// Discriminator: drive the synthesized last-counter `CastFromZone` body
    /// (self-targeting, `without_paying_mana_cost`) on a suspended sorcery and
    /// assert the spell lands on the stack at zero cost. Pre-fix the stack is
    /// empty (the card sits in exile holding only a permission); post-fix the
    /// cast-during-resolution path puts it on the stack.
    #[test]
    fn suspend_last_counter_free_cast_puts_spell_on_stack() {
        use crate::types::keywords::Keyword;
        use crate::types::mana::ManaCost as MC;
        use crate::types::phase::Phase;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::Upkeep;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // A suspended sorcery owned/controlled by PlayerId(0) — Treasure Cruise
        // is a sorcery with no targets ("Draw three cards"). It sits in exile
        // with the Suspend keyword; its last time counter has just been removed.
        let suspended = create_object(
            &mut state,
            CardId(7001),
            PlayerId(0),
            "Treasure Cruise".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&suspended).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.base_card_types = obj.card_types.clone();
            obj.mana_cost = MC::generic(7);
            obj.keywords.push(Keyword::Suspend {
                count: 0,
                cost: MC::zero(),
            });
            obj.base_keywords = obj.keywords.clone();
        }

        // The synthesized last-counter cast trigger body (CR 702.62a):
        // `build_suspend_last_counter_cast_trigger` executes this exact effect
        // when the final time counter is removed.
        let cast_ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::SelfRef,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: crate::types::ability::CastFromZoneDriver::DuringResolution,
            },
            vec![TargetRef::Object(suspended)],
            suspended,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &cast_ability, &mut events).unwrap();

        // CR 702.62a: the player accepted the optional cast — the spell must be
        // cast as the trigger resolves and placed on the stack. A bare
        // permission grant (card still in exile, empty stack) is the bug.
        assert_eq!(
            state.stack.len(),
            1,
            "suspend last-counter cast (CR 702.62a) must put the spell on the \
             stack, not just grant a lingering ExileWithAltCost permission"
        );
        assert_eq!(
            state.objects.get(&suspended).map(|o| o.zone),
            Some(Zone::Stack),
            "the suspended card must move to the stack when cast for free"
        );
        // CR 702.62a: cast WITHOUT paying its mana cost — no mana was spent.
        assert!(
            state.players.iter().all(|p| p.mana_pool.total() == 0),
            "the free cast must not require or consume mana"
        );
    }

    #[test]
    fn grants_zero_cost_permission_on_exiled_card() {
        let mut state = make_test_state();
        let obj_id = add_card_to_exile(&mut state, PlayerId(1), CardId(100));

        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::Any,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Card should remain in exile with a zero-cost casting permission.
        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.zone, Zone::Exile);
        assert!(obj.casting_permissions.iter().any(|p| matches!(
            p,
            CastingPermission::ExileWithAltCost { cost, .. } if *cost == ManaCost::zero()
        )));
    }

    #[test]
    fn exiles_card_not_in_exile_then_grants_permission() {
        let mut state = make_test_state();
        let obj_id = add_card_to_hand(&mut state, PlayerId(1), CardId(200));

        // Card starts in opponent's hand.
        assert_eq!(state.objects.get(&obj_id).unwrap().zone, Zone::Hand);

        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::Any,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Card should have been moved to exile and granted permission.
        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.zone, Zone::Exile);
        assert!(obj.casting_permissions.iter().any(|p| matches!(
            p,
            CastingPermission::ExileWithAltCost { cost, .. } if *cost == ManaCost::zero()
        )));
    }

    #[test]
    fn without_paying_false_uses_card_mana_cost() {
        let mut state = make_test_state();
        let obj_id = add_card_to_exile(&mut state, PlayerId(1), CardId(300));

        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::Any,
                without_paying_mana_cost: false,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Permission should use the card's own mana cost ({3}).
        let obj = state.objects.get(&obj_id).unwrap();
        assert!(obj.casting_permissions.iter().any(|p| matches!(
            p,
            CastingPermission::ExileWithAltCost { cost, .. } if *cost == ManaCost::generic(3)
        )));
    }

    #[test]
    fn exiled_by_source_filter_materializes_linked_exile_cards_without_targets() {
        let mut state = make_test_state();
        let source = create_object(
            &mut state,
            CardId(999),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let instant = add_card_to_exile(&mut state, PlayerId(1), CardId(301));
        state
            .objects
            .get_mut(&instant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        let creature = add_card_to_exile(&mut state, PlayerId(1), CardId(302));
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.exile_links.push(ExileLink {
            exiled_id: instant,
            source_id: source,
            kind: ExileLinkKind::TrackedBySource,
        });
        state.exile_links.push(ExileLink {
            exiled_id: creature,
            source_id: source,
            kind: ExileLinkKind::TrackedBySource,
        });

        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::And {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                        TargetFilter::ExiledBySource,
                    ],
                },
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.objects[&instant]
            .casting_permissions
            .iter()
            .any(|p| matches!(
                p,
                CastingPermission::ExileWithAltCost { cost, .. } if *cost == ManaCost::zero()
            )));
        assert!(
            state.objects[&creature].casting_permissions.is_empty(),
            "composed filter must preserve the typed restriction"
        );
    }

    #[test]
    fn no_targets_emits_resolved_event() {
        let mut state = make_test_state();

        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::Any,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should emit EffectResolved with no errors.
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::CastFromZone,
                ..
            }
        )));
    }

    #[test]
    fn grants_mana_value_constraint_on_permission() {
        let mut state = make_test_state();
        let obj_id = add_card_to_exile(&mut state, PlayerId(0), CardId(400));
        let constraint = CastPermissionConstraint::ManaValue {
            comparator: Comparator::LE,
            value: QuantityExpr::Fixed { value: 4 },
        };

        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::Any,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: Some(constraint.clone()),
                duration: None,
                driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&obj_id).unwrap();
        assert!(obj.casting_permissions.iter().any(|p| matches!(
            p,
            CastingPermission::ExileWithAltCost {
                constraint: Some(found),
                ..
            } if *found == constraint
        )));
    }
}
