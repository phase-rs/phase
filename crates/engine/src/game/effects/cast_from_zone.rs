use crate::game::zones;
use crate::types::ability::{
    CastingPermission, Duration, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaCost;
use crate::types::zones::Zone;

/// CR 115.1 + CR 601.2c: "You may cast a spell ... from your hand without paying
/// its mana cost" (Electrodominance, Baral's Expertise) has no "target" word —
/// the spell is chosen at resolution from the granting player's hand via
/// `EffectZoneChoice`, not stack-time targeting.
fn open_private_zone_cast_selection(
    state: &mut GameState,
    ability: &ResolvedAbility,
    target_filter: &TargetFilter,
    source_zone: Zone,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    let Some(player) = state.players.iter().find(|p| p.id == ability.controller) else {
        return Err(EffectError::PlayerNotFound);
    };
    let cards_iter = match source_zone {
        Zone::Hand => player.hand.iter(),
        _ => unreachable!("private CastFromZone selection is currently hand-only"),
    };
    let eligible: Vec<_> = cards_iter
        .copied()
        .filter(|id| crate::game::filter::matches_target_filter(state, *id, target_filter, &ctx))
        .collect();

    if eligible.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::CastFromZone,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let mut stash = ability.clone();
    stash.targets.clear();
    crate::game::effects::append_to_pending_continuation(state, Some(Box::new(stash)));
    state.waiting_for = WaitingFor::EffectZoneChoice {
        player: ability.controller,
        cards: eligible,
        count: 1,
        min_count: 0,
        up_to: true,
        source_id: ability.source_id,
        effect_kind: EffectKind::CastFromZone,
        zone: source_zone,
        destination: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enter_transformed: false,
        enters_under_player: None,
        enters_attacking: false,
        owner_library: false,
        track_exiled_by_source: false,
        // CR 708.2a: cast-from-zone selection is not a face-down entry.
        face_down_profile: None,
        count_param: 0,
    };
    Ok(())
}

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
        _duration,
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
        if let Some(source_zone) = target_filter.extract_in_zone() {
            if source_zone == Zone::Hand {
                return open_private_zone_cast_selection(
                    state,
                    ability,
                    target_filter,
                    source_zone,
                    events,
                );
            }
        }
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

    // CR 608.2g: A targeted free-cast of a card the controller could never
    // surface a *later* cast for must also be driven DURING resolution. Memory
    // Plunder's "you may cast target instant or sorcery card from an opponent's
    // graveyard without paying its mana cost" leaves the card in the OPPONENT's
    // graveyard; a lingering `ExileWithAltCost` grant is inert there because the
    // graveyard cast surface (`graveyard_spell_objects_available_to_cast`) only
    // offers cards in the *controller's own* graveyard (`obj.owner == player`),
    // so the granted player can never act on the permission (issue #2884:
    // accepting the "you may cast" prompt did nothing). When the single resolved
    // target is a card in a graveyard owned by another player, the only
    // rules-correct casting mechanism is CR 608.2g — cast it as this effect
    // resolves. The Suspend/Rebound self-cast (`driver` + `target == source`) is
    // the other during-resolution case; both share the same casting authority.
    let foreign_graveyard_free_cast = without_paying
        && alt_ability_cost.is_none()
        && target_ids.len() == 1
        && target_is_in_other_players_graveyard(state, target_ids[0], ability.controller);

    if self_free_cast || foreign_graveyard_free_cast {
        return cast_single_target_during_resolution(
            state,
            ability,
            target_ids[0],
            constraint.clone(),
            cast_transformed,
            events,
        );
    }

    grant_lingering_permissions(state, ability, &target_ids, events)?;

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CastFromZone,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 608.2g: True when `card` currently sits in a graveyard owned by a player
/// other than `controller`. Such a card can never be cast via a lingering
/// `ExileWithAltCost` grant — the graveyard cast surface only offers cards in
/// the controller's own graveyard — so a targeted free-cast of it must happen
/// during resolution (Memory Plunder, issue #2884).
fn target_is_in_other_players_graveyard(
    state: &GameState,
    card: ObjectId,
    controller: crate::types::player::PlayerId,
) -> bool {
    state
        .objects
        .get(&card)
        .is_some_and(|obj| obj.zone == Zone::Graveyard && obj.owner != controller)
}

/// CR 608.2g + CR 601.2a–i: Cast a single targeted card DURING the resolution of
/// this effect, for free, via the same authority Cascade/Discover/Suspend use.
///
/// Shared by the Suspend/Rebound self-cast (`target == source`) and the
/// foreign-graveyard free-cast (Memory Plunder). `initiate_cast_during_resolution`
/// grants the zero-cost `ExileWithAltCost` permission keyed with a
/// `ResolutionCastCleanup` marker (which authorizes the cast from the card's
/// current zone and arms the CR 608.2g sorcery-speed / empty-stack timing bypass
/// in `restrictions::check_spell_timing`), prepares the cast, and continues it on
/// `Auto` payment. The returned `WaitingFor` (target selection if the cast spell
/// targets, else priority with it on the stack) becomes the resolution's pending
/// prompt.
fn cast_single_target_during_resolution(
    state: &mut GameState,
    ability: &ResolvedAbility,
    card: ObjectId,
    constraint: Option<crate::types::ability::CastPermissionConstraint>,
    cast_transformed: bool,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CastFromZone,
        source_id: ability.source_id,
    });
    // CR 702.62a's "if you don't, it remains exiled" disposition is `RemainExiled`
    // (only reached if a future free-cast adds an MV gate; these carry none).
    // There are no dig misses for a targeted single-card free-cast.
    let cleanup = crate::types::ability::ResolutionCastCleanup {
        exiled_misses: Vec::new(),
        reject_action: crate::types::ability::ResolutionMvRejectAction::RemainExiled,
        success_action: crate::types::ability::ResolutionCastSuccessAction::BottomMisses,
    };
    state.waiting_for = crate::game::casting::initiate_cast_during_resolution(
        state,
        ability.controller,
        card,
        constraint,
        cast_transformed,
        cleanup,
        events,
    )
    .map_err(|e| EffectError::InvalidParam(e.to_string()))?;
    Ok(())
}

/// CR 614.1a: Toshiro / Torrential Gearhulk class — the parser currently
/// represents "If that spell would be put into a graveyard, exile it instead"
/// as a sequential `ChangeZone` rider on `CastFromZone`. Runtime consumes that
/// rider as permission metadata, not as an immediate zone move.
pub(crate) fn is_graveyard_exile_rider_subability(ability: &ResolvedAbility) -> bool {
    matches!(
        &ability.effect,
        Effect::ChangeZone {
            destination: Zone::Exile,
            target: TargetFilter::ParentTarget,
            ..
        }
    )
}

fn cast_from_zone_has_graveyard_exile_rider(ability: &ResolvedAbility) -> bool {
    ability
        .sub_ability
        .as_deref()
        .is_some_and(is_graveyard_exile_rider_subability)
}

/// CR 118.9: Stamp `ExileWithAltCost` / `ExileWithAltAbilityCost` on resolved
/// targets. Shared by the direct resolve path and the `EffectZoneChoice` resume
/// path (Electrodominance hand pick).
pub(crate) fn grant_lingering_permissions(
    state: &mut GameState,
    ability: &ResolvedAbility,
    target_ids: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (without_paying, cast_transformed, alt_ability_cost, constraint, duration) =
        match &ability.effect {
            Effect::CastFromZone {
                without_paying_mana_cost,
                cast_transformed,
                alt_ability_cost,
                constraint,
                duration,
                ..
            } => (
                *without_paying_mana_cost,
                *cast_transformed,
                alt_ability_cost.clone(),
                constraint.clone(),
                duration.clone(),
            ),
            _ => return Err(EffectError::MissingParam("CastFromZone".to_string())),
        };
    let exile_instead_of_graveyard_on_resolve = cast_from_zone_has_graveyard_exile_rider(ability);

    for &obj_id in target_ids {
        // CR 601.2a: Impulse-draw and similar grants move non-exile cards to
        // exile before attaching `ExileWithAltCost`. Targeted graveyard grants
        // (Emry, Lurker in the Loch) and resolution-time hand picks
        // (Electrodominance) keep the card in its source zone and grant a
        // permission the casting pipeline consumes in place.
        let current_zone = state.objects.get(&obj_id).map(|o| o.zone);
        if current_zone.is_some_and(|z| z != Zone::Exile && z != Zone::Graveyard && z != Zone::Hand)
        {
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
                    exile_instead_of_graveyard_on_resolve,
                }
            };
            if !obj.casting_permissions.contains(&permission) {
                obj.casting_permissions.push(permission);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        CardPlayMode, CastFromZoneDriver, CastPermissionConstraint, Comparator, ControllerRef,
        Effect, FilterProp, QuantityExpr, TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{ExileLink, ExileLinkKind, WaitingFor};
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

    fn electrodominance_hand_ability(max_value: i32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::Typed(
                    TypedFilter::default()
                        .with_type(TypeFilter::Card)
                        .controller(ControllerRef::You)
                        .properties(vec![
                            FilterProp::InZone { zone: Zone::Hand },
                            FilterProp::Cmc {
                                comparator: Comparator::LE,
                                value: QuantityExpr::Fixed { value: max_value },
                            },
                        ]),
                ),
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: CastFromZoneDriver::LingeringPermission,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        )
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

    /// Issue #2884 — Memory Plunder: "you may cast target instant or sorcery card
    /// from an opponent's graveyard without paying its mana cost". The target is
    /// in the OPPONENT's graveyard, where a lingering `ExileWithAltCost` grant is
    /// inert (the graveyard cast surface only offers cards in the controller's own
    /// graveyard). The free cast must therefore be driven DURING resolution
    /// (CR 608.2g): the card moves directly from that graveyard to the stack
    /// under CR 601.2a, rather than staying in the graveyard with a dead
    /// permission or detouring through exile.
    ///
    /// Discriminator vs. `graveyard_target_grant_stays_in_graveyard_with_timed_permission`:
    /// the only difference is the target's owner — own-graveyard → lingering
    /// permission (stays put); opponent-graveyard → cast during resolution.
    #[test]
    fn opponent_graveyard_free_cast_moves_directly_to_stack() {
        let mut state = make_test_state();
        // Target sits in PlayerId(1)'s graveyard; the ability controller is P0.
        let obj_id = {
            let id = add_card_to_graveyard(&mut state, PlayerId(1), CardId(2884));
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Instant);
            id
        };

        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::ParentTarget,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: CastFromZoneDriver::LingeringPermission,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 608.2g + CR 601.2a: the card was cast during resolution, moving
        // from the opponent's graveyard directly to the stack. A graveyard→exile
        // pre-move would make this rules-incorrect for zone-change consumers.
        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(
            obj.zone,
            Zone::Stack,
            "the free cast must put the targeted spell on the stack during resolution"
        );
        assert!(
            events.iter().any(|event| {
                matches!(
                    event,
                    GameEvent::ZoneChanged {
                        object_id,
                        from: Some(Zone::Graveyard),
                        to: Zone::Stack,
                        ..
                    } if *object_id == obj_id
                )
            }),
            "the free cast must move from the opponent's graveyard directly to the stack"
        );
        assert!(
            !events.iter().any(|event| {
                matches!(
                    event,
                    GameEvent::ZoneChanged {
                        object_id,
                        from: Some(Zone::Graveyard),
                        to: Zone::Exile,
                        ..
                    } if *object_id == obj_id
                )
            }),
            "Memory Plunder must not fake an exile origin before casting"
        );
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
        let obj_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(1),
            "Library Spell".to_string(),
            Zone::Library,
        );

        assert_eq!(state.objects.get(&obj_id).unwrap().zone, Zone::Library);

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

        // Non-hand, non-graveyard cards should be moved to exile and granted permission.
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
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(
            state.objects[&instant].zone,
            Zone::Exile,
            "linked exile cards stay in exile while the cast permission is stamped"
        );
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

    /// Issue #1313 — Electrodominance's "you may cast a spell with mana value X
    /// or less from your hand" must open a resolution-time hand pick, not
    /// silently no-op when `ability.targets` is empty.
    #[test]
    fn hand_cast_without_targets_emits_effect_zone_choice() {
        let mut state = make_test_state();
        let cheap = add_card_to_hand(&mut state, PlayerId(0), CardId(501));
        state.objects.get_mut(&cheap).unwrap().mana_cost = ManaCost::generic(2);
        let expensive = add_card_to_hand(&mut state, PlayerId(0), CardId(502));
        state.objects.get_mut(&expensive).unwrap().mana_cost = ManaCost::generic(5);

        let ability = electrodominance_hand_ability(3);

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                min_count,
                up_to,
                effect_kind,
                zone,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 1);
                assert_eq!(*min_count, 0);
                assert!(*up_to);
                assert_eq!(*effect_kind, EffectKind::CastFromZone);
                assert_eq!(*zone, Zone::Hand);
                assert!(cards.contains(&cheap));
                assert!(!cards.contains(&expensive));
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
    }

    #[test]
    fn hand_cast_without_eligible_cards_resolves_without_prompt() {
        let mut state = make_test_state();
        let expensive = add_card_to_hand(&mut state, PlayerId(0), CardId(503));
        state.objects.get_mut(&expensive).unwrap().mana_cost = ManaCost::generic(5);

        let ability = electrodominance_hand_ability(3);
        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!matches!(
            &state.waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ));
        assert!(state.pending_continuation.is_none());
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::CastFromZone,
                ..
            }
        )));
    }

    #[test]
    fn hand_cast_decline_consumes_prompt_without_permission() {
        let mut state = make_test_state();
        let cheap = add_card_to_hand(&mut state, PlayerId(0), CardId(504));
        let ability = electrodominance_hand_ability(3);

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();
        apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] }).unwrap();

        assert!(state.pending_continuation.is_none());
        assert_eq!(state.objects[&cheap].zone, Zone::Hand);
        assert!(state.objects[&cheap].casting_permissions.is_empty());
    }

    #[test]
    fn hand_cast_selection_grants_zero_cost_permission_in_hand() {
        let mut state = make_test_state();
        let cheap = add_card_to_hand(&mut state, PlayerId(0), CardId(505));
        let ability = electrodominance_hand_ability(3);

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();
        apply_as_current(&mut state, GameAction::SelectCards { cards: vec![cheap] }).unwrap();

        assert_eq!(state.objects[&cheap].zone, Zone::Hand);
        assert!(state.objects[&cheap]
            .casting_permissions
            .iter()
            .any(|p| matches!(
                p,
                CastingPermission::ExileWithAltCost {
                    cost,
                    granted_to: Some(PlayerId(0)),
                    ..
                } if *cost == ManaCost::zero()
            )));
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
    fn graveyard_cast_exile_rider_stamps_permission_flag() {
        let mut state = make_test_state();
        let instant = {
            let obj_id = add_card_to_graveyard(&mut state, PlayerId(0), CardId(2937));
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj_id
        };

        let mut ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::ParentTarget,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: CastFromZoneDriver::LingeringPermission,
            },
            vec![TargetRef::Object(instant)],
            ObjectId(999),
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::ChangeZone {
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
                face_down_profile: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        )));

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.objects[&instant]
            .casting_permissions
            .iter()
            .any(|p| matches!(
                p,
                CastingPermission::ExileWithAltCost {
                    exile_instead_of_graveyard_on_resolve: true,
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
