use crate::game::zone_pipeline::{self, BatchMoveResult, ZoneMoveRequest};
use crate::types::ability::{
    AbilityCost, CastingPermission, Duration, Effect, EffectError, EffectKind, ResolvedAbility,
    SpellStackToGraveyardReplacement, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{BatchCompletion, CastingVariant, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaCost;
use crate::types::zones::Zone;

/// CR 400.1/400.2: Recursively extract a filter's own `controller` axis,
/// looking through the composed forms (`Not`/`And`/`Or`) a real card's target
/// filter may be built from rather than only matching a bare `Typed`. `And`/
/// `Or` return the first branch that carries a controller axis — composed
/// filters in this codebase don't mix two different explicit player axes on
/// the same object filter, so first-found is unambiguous.
fn extract_controller_ref(filter: &TargetFilter) -> Option<&crate::types::ability::ControllerRef> {
    match filter {
        TargetFilter::Typed(tf) => tf.controller.as_ref(),
        TargetFilter::Not { filter } => extract_controller_ref(filter),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().find_map(extract_controller_ref)
        }
        _ => None,
    }
}

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
    // CR 400.1/400.2 + CR 109.4: A hand-scoped cast filter's own `controller`
    // axis names WHOSE hand is the candidate pool. Buster-Sword-class filters
    // ("cast a spell from your hand") carry no controller (or `You`) and keep
    // scanning the caster's own hand. Silent-Blade Oni's "cast a spell from
    // among those cards" (bound to the damaged player's hand via
    // `ControllerRef::TriggeringPlayer`, issue #5240) needs a DIFFERENT
    // player's hand as the pool — `ability.controller` alone can't express
    // that, so resolve the filter's own controller axis through the single
    // `ControllerRef` authority instead of hardcoding the caster. Recurses
    // through `Not`/`And`/`Or` (extract_controller_ref) so a composed filter
    // (e.g. a future card combining a type restriction with a player axis via
    // `And`) isn't silently treated as caster-scoped.
    let hand_owner = extract_controller_ref(target_filter)
        .and_then(|cref| {
            crate::game::filter::controller_ref_player(
                state,
                ability.source_id,
                Some(ability.controller),
                Some(ability),
                cref,
            )
        })
        .unwrap_or(ability.controller);
    let Some(player) = state.players.iter().find(|p| p.id == hand_owner) else {
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
            subject: None,
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
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        count_param: 0,
        library_position: None,
        is_cost_payment: false,
        enters_modified_if: None,
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
        duration,
        driver,
        mana_spend_permission,
    ) = match &ability.effect {
        Effect::CastFromZone {
            target,
            without_paying_mana_cost,
            cast_transformed,
            alt_ability_cost,
            constraint,
            duration,
            driver,
            mana_spend_permission,
            ..
        } => (
            target,
            *without_paying_mana_cost,
            *cast_transformed,
            alt_ability_cost.clone(),
            constraint.clone(),
            duration.clone(),
            *driver,
            *mana_spend_permission,
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

    // CR 701.20e + CR 608.2c: Look-then-cast chains (Kiora) inject the legal
    // looked-at library cards as targets at the chain seam
    // (`inject_last_revealed_targets`), already filtered through this cast
    // filter's `ExiledBySource`→`LastRevealed` remap. Explicitly-supplied
    // targets from ordinary CastFromZone paths (graveyard/exile free-cast,
    // Bring to Light, Urza) must NOT be re-filtered through that remap, which
    // would drop every target not in `last_revealed_ids`. The remap therefore
    // only applies on the empty-target fallback below.
    if target_ids.is_empty() && target_filter.references_exiled_by_source() {
        let linked = crate::game::players::linked_exile_cards_for_source(state, ability.source_id);
        let current_linked_ids: Vec<_> = state
            .last_zone_changed_ids
            .iter()
            .copied()
            .filter(|id| linked.iter().any(|link| link.exiled_id == *id))
            .collect();
        let candidate_ids: Vec<_> = if current_linked_ids.is_empty() {
            linked.iter().map(|link| link.exiled_id).collect()
        } else {
            current_linked_ids
        };
        // CR 607.2a + CR 608.2c: For an immediately chained "exiled this way"
        // cast grant, bind the filter's object-scope reads to the current
        // resolution's linked cards, not the source's lifetime exile pile.
        let mut scoped_ability = ability.clone();
        scoped_ability.targets = candidate_ids
            .iter()
            .copied()
            .map(TargetRef::Object)
            .collect();
        let ctx = crate::game::filter::FilterContext::from_ability(&scoped_ability);
        target_ids = candidate_ids
            .iter()
            .copied()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.zone == Zone::Exile)
                    && crate::game::filter::matches_target_filter(state, *id, target_filter, &ctx)
            })
            .collect();
        // CR 701.20e + CR 608.2c: Look-then-cast chains (Kiora, Sovereign of
        // the Deep) leave the looked-at cards in the library. `Dig { keep_count:
        // 0 }` publishes them via `last_revealed_ids`, not exile links, but the
        // parser still binds the cast step to `ExiledBySource`.
        if target_ids.is_empty() && !state.last_revealed_ids.is_empty() {
            target_ids =
                crate::game::filter::last_revealed_library_ids_matching(state, target_filter, &ctx);
        }
    }

    // CR 310.11b + CR 608.2c: "exile it, then you may cast it transformed" —
    // the SelfRef filter resolves to the source object itself. When
    // `ability.targets` is empty (no pre-selected target, as is typical for
    // Siege defeat and Suspend self-cast triggers), fall back to the source
    // directly so the card can be cast during resolution rather than silently
    // staying in exile.
    if target_ids.is_empty()
        && matches!(target_filter, TargetFilter::SelfRef)
        && ability.source_is_current(state)
    {
        target_ids = vec![ability.source_id];
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
        // CR 701.20a + CR 608.2c: "Draw three cards and reveal them. You may cast
        // one of them" (Mad Wizard's Lair) leaves the revealed cards in hand;
        // `LastRevealed` must open a hand selection among them, not resolve to
        // `.first()` or filter them out via the library-only reveal injector.
        if matches!(target_filter, TargetFilter::LastRevealed)
            && state.last_revealed_ids.iter().any(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.zone == Zone::Hand)
            })
        {
            return open_private_zone_cast_selection(
                state,
                ability,
                target_filter,
                Zone::Hand,
                events,
            );
        }
        // No targets resolved — nothing to cast.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::CastFromZone,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // CR 608.2g: A `DuringResolution` cast-from-zone casts the single resolved
    // target, for free, AS THE GRANTING ABILITY RESOLVES — the card goes onto
    // the stack immediately rather than being deferred to a lingering
    // permission the player acts on at a later priority window. Two producers
    // share this path:
    //
    //   - CR 702.62a + CR 702.62d: Suspend's last-time-counter ability casts
    //     the card it is attached to (the single resolved target IS the
    //     ability's source). Issue #1520: accepting the optional "cast it?"
    //     prompt appeared to do nothing because only a permission was stamped —
    //     the spell was never put on the stack, and a sorcery like Treasure
    //     Cruise was additionally blocked by the sorcery-speed timing gate at
    //     upkeep.
    //   - CR 701.23 + CR 608.2g (tutor-and-cast): Bring to Light tutors a card into the
    //     controller's OWN exile, then "you may cast it without paying its mana
    //     cost." The tutored card is NOT the source (target != source) and sits
    //     in the controller's own exile, so the Suspend-specific
    //     `target == source` defense and the foreign-graveyard defense below
    //     both miss it. Issue #2880: it fell to `grant_lingering_permissions`,
    //     which stamped an indefinite `ExileWithAltCost { duration: None }` —
    //     a free-cast permission that persists forever instead of being a
    //     one-shot resolution offer.
    //
    // Drive the cast immediately through the same cast-during-resolution
    // authority Cascade/Discover use (`initiate_cast_during_resolution`).
    //
    // The router reads the EXPLICIT `driver` discriminator
    // (`CastFromZoneDriver::DuringResolution`), NOT `duration`. `duration` is
    // CR 611.2a permission-expiry and says nothing about the casting mechanism;
    // routing on it conflated two axes. The structural-shape guard here
    // (`without_paying` + no alt-cost + single target) gates only the DIRECT
    // free-cast path: when it holds, the during-resolution cast of that single
    // card is free, since `initiate_cast_during_resolution` defaults a `None`
    // `alt_mana_cost` to zero. A `DuringResolution` body is NOT universally a
    // free cast, though — when the body carries an `alt_ability_cost` (The Face
    // of Boe's borrowed Suspend cost, CR 118.9 + CR 702.62a), this guard's
    // `alt_ability_cost.is_none()` clause fails and the cast is routed through
    // the resolution-time hand pick (`complete_hand_pick_cast_from_zone`), which
    // threads the resolved non-zero `alt_mana_cost` into
    // `initiate_cast_during_resolution`. The Suspend-era
    // `target == source` clause is intentionally dropped: every existing
    // `DuringResolution` producer (Suspend) uses `target: SelfRef`, so
    // `target == source` still holds for them, and the tutor-and-cast producer
    // (Bring to Light, `target != source` but in the controller's own exile)
    // must reach this path.
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
    let driver_free_cast = driver.is_during_resolution()
        && without_paying
        && alt_ability_cost.is_none()
        && target_ids.len() == 1;

    // CR 608.2g: A targeted immediate free-cast of a card in a graveyard must
    // be driven DURING resolution — the controller chooses whether to cast as
    // this effect resolves (Torrential Gearhulk / Memory Plunder / Toshiro
    // class). A lingering `ExileWithAltCost` grant is wrong here:
    //   - opponent-graveyard targets are inert on the graveyard cast surface
    //     (issue #2884 — accepting did nothing);
    //   - own-graveyard targets defer the cast to a later priority window,
    //     which violates CR 608.2g for resolution-time "you may cast" with no
    //     standing duration (issue #852).
    // Timed grants (`duration: Some(_)`) and paid casts stay on the lingering
    // permission path (Emry, Urza-class deferred play).
    let immediate_graveyard_free_cast = without_paying
        && alt_ability_cost.is_none()
        && duration.is_none()
        && target_ids.len() == 1
        && state
            .objects
            .get(&target_ids[0])
            .is_some_and(|obj| obj.zone == Zone::Graveyard);

    // CR 608.2g + CR 609.4b: paid during-resolution graveyard cast (Quistis Trepe,
    // Tinybones the Pickpocket). Not without_paying — the caster pays the real cost
    // with any-type mana. Offered accept/decline, resolved by
    // initiate_cast_during_resolution with ResolutionCastCost::FullCost. Replaces
    // the wrong lingering-permission path (#2884: the offer was inert on
    // opponent-graveyard targets, and own-graveyard targets deferred the cast to a
    // later priority window instead of a resolution-time offer).
    let graveyard_paid_cast = !without_paying
        && mana_spend_permission.is_some()
        && driver.is_during_resolution()
        && alt_ability_cost.is_none()
        && duration.is_none()
        && target_ids.len() == 1
        && state
            .objects
            .get(&target_ids[0])
            .is_some_and(|o| o.zone == Zone::Graveyard);
    if graveyard_paid_cast {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::CastFromZone,
            source_id: ability.source_id,
            subject: None,
        });
        state.waiting_for = WaitingFor::CastOffer {
            player: ability.controller,
            kind: crate::types::game_state::CastOfferKind::GraveyardPaidCast {
                hit_card: target_ids[0],
                mana_spend_permission,
                graveyard_replacement: cast_from_zone_graveyard_destination(ability),
                cast_transformed,
                constraint: constraint.clone(),
            },
        };
        return Ok(());
    }

    if driver_free_cast || immediate_graveyard_free_cast {
        // CR 608.2g: both gates require `alt_ability_cost.is_none()`, so the
        // pre-targeted free-cast path never carries a borrowed keyword cost —
        // The Face of Boe (alt=Some) reaches the hand-pick path instead.
        if is_stack_spell_copy(state, target_ids[0]) {
            return cast_stack_spell_copy_during_resolution(state, ability, target_ids[0], events);
        }
        return cast_single_target_during_resolution(
            state,
            ability,
            target_ids[0],
            constraint.clone(),
            cast_transformed,
            None,
            events,
        );
    }

    match grant_lingering_permissions(state, ability, &target_ids, events)? {
        LingeringPermissionGrantResult::Immediate => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::CastFromZone,
                source_id: ability.source_id,
                subject: None,
            });
        }
        LingeringPermissionGrantResult::ExileDeliveryComplete => {}
        // CR 614.1 + CR 616.1: A current-zone-to-Exile delivery may park for
        // replacement ordering. Its typed batch completion records the
        // permission and emits `EffectResolved` only after the delivery settles,
        // so this resolver must not run either tail early.
        LingeringPermissionGrantResult::NeedsChoice => {
            return Ok(());
        }
    }

    Ok(())
}

/// CR 608.2g + CR 601.2a: After a resolution-time hand pick for a free
/// `CastFromZone` (Expertise cycle, Electrodominance), cast the chosen spell
/// during resolution instead of granting a lingering hand permission.
pub(crate) fn complete_hand_pick_cast_from_zone(
    state: &mut GameState,
    ability: &ResolvedAbility,
    card: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<bool, EffectError> {
    let (without_paying, cast_transformed, alt_ability_cost, constraint, driver) =
        match &ability.effect {
            Effect::CastFromZone {
                without_paying_mana_cost,
                cast_transformed,
                alt_ability_cost,
                constraint,
                driver,
                ..
            } => (
                *without_paying_mana_cost,
                *cast_transformed,
                alt_ability_cost.as_ref(),
                constraint.clone(),
                *driver,
            ),
            _ => return Err(EffectError::MissingParam("CastFromZone".to_string())),
        };

    let during_resolution = driver.is_during_resolution()
        || (without_paying
            && alt_ability_cost.is_none()
            && matches!(
                &ability.effect,
                Effect::CastFromZone { target, .. }
                    if target.extract_in_zone() == Some(Zone::Hand)
            ));

    if during_resolution {
        // CR 118.9 + CR 702.62a: read the borrowed keyword cost (The Face of Boe's
        // suspend cost) from the picked card so the during-resolution cast
        // overrides its mana cost with that cost rather than casting it free.
        let alt_mana_cost = match alt_ability_cost {
            Some(AbilityCost::KeywordCostOfCastSpell { keyword }) => {
                let Some(cost) =
                    crate::game::keywords::effective_keyword_mana_cost(state, card, *keyword)
                else {
                    // CR 118.9: `effective_keyword_mana_cost` returns `None` only as
                    // the documented defensive refusal that surfaces a misparse
                    // (see `keywords::effective_keyword_mana_cost`). The
                    // during-resolution path must NOT downgrade that refusal into a
                    // `{0}` free cast (`initiate_cast_during_resolution` defaults a
                    // `None` `alt_mana_cost` to zero) — that inverts the contract and
                    // would miscost the spell. Abort the cast instead: leave the
                    // picked card untouched in its current zone and resolve the
                    // granting effect as a no-op rather than free-casting.
                    events.push(GameEvent::EffectResolved {
                        kind: EffectKind::CastFromZone,
                        source_id: ability.source_id,
                        subject: None,
                    });
                    return Ok(false);
                };
                Some(cost)
            }
            _ => None,
        };
        cast_single_target_during_resolution(
            state,
            ability,
            card,
            constraint.or_else(|| effective_cast_from_zone_constraint(ability)),
            cast_transformed,
            alt_mana_cost,
            events,
        )?;
        return Ok(true);
    }

    Ok(matches!(
        grant_lingering_permissions(state, ability, std::slice::from_ref(&card), events)?,
        LingeringPermissionGrantResult::NeedsChoice
    ))
}

fn effective_cast_from_zone_constraint(
    ability: &ResolvedAbility,
) -> Option<crate::types::ability::CastPermissionConstraint> {
    let Effect::CastFromZone { target, .. } = &ability.effect else {
        return None;
    };
    let TargetFilter::Typed(filter) = target else {
        return None;
    };
    filter.properties.iter().find_map(|prop| {
        if let crate::types::ability::FilterProp::Cmc { comparator, value } = prop {
            Some(crate::types::ability::CastPermissionConstraint::ManaValue {
                comparator: *comparator,
                value: value.clone(),
            })
        } else {
            None
        }
    })
}

/// CR 707.10 + CR 608.2g: A `CopySpell` that put a spell copy onto the stack
/// (Isochron Scepter / Spellbinder) is not yet cast. A chained `CastFromZone {
/// ParentTarget, DuringResolution }` completes that cast without moving zones.
fn is_stack_spell_copy(state: &GameState, object_id: ObjectId) -> bool {
    state.objects.get(&object_id).is_some_and(|obj| {
        obj.zone == Zone::Stack && state.stack.iter().any(|entry| entry.id == object_id)
    })
}

/// CR 707.10 + CR 118.9: Finish casting a spell copy that `CopySpell` already
/// placed on the stack — emit `SpellCast`, open CR 707.10c retarget selection
/// when needed, and do not route through `initiate_cast_during_resolution`
/// (Stack is not a castable origin zone).
fn cast_stack_spell_copy_during_resolution(
    state: &mut GameState,
    ability: &ResolvedAbility,
    copy_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CastFromZone,
        source_id: ability.source_id,
        subject: None,
    });

    let Some(obj) = state.objects.get(&copy_id).cloned() else {
        return Err(EffectError::InvalidParam(format!(
            "stack spell copy {copy_id:?} not found"
        )));
    };
    if obj.zone != Zone::Stack {
        return Err(EffectError::InvalidParam(format!(
            "ParentTarget {copy_id:?} is not a stack spell copy"
        )));
    }

    let origin = obj.cast_from_zone.unwrap_or(Zone::Exile);
    events.push(GameEvent::SpellCast {
        card_id: obj.card_id,
        controller: ability.controller,
        object_id: copy_id,
    });
    crate::game::restrictions::record_spell_cast_from_zone(
        state,
        ability.controller,
        &obj,
        origin,
        CastingVariant::Normal,
    );

    if crate::game::effects::prepare::open_copy_target_selection(
        state,
        copy_id,
        ability.controller,
        None,
    )
    .map_err(EffectError::InvalidParam)?
    {
        return Ok(());
    }

    state.waiting_for = WaitingFor::Priority {
        player: ability.controller,
    };
    Ok(())
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
    alt_mana_cost: Option<ManaCost>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CastFromZone,
        source_id: ability.source_id,
        subject: None,
    });
    // CR 702.62a's "if you don't, it remains exiled" disposition is `RemainExiled`
    // (only reached if a future free-cast adds an MV gate; these carry none).
    // There are no dig misses for a targeted single-card free-cast.
    let cleanup = crate::types::ability::ResolutionCastCleanup {
        source_id: ability.source_id,
        exiled_misses: Vec::new(),
        reject_action: crate::types::ability::ResolutionMvRejectAction::RemainExiled,
        success_action: crate::types::ability::ResolutionCastSuccessAction::BottomMisses,
    };
    let graveyard_replacement = cast_from_zone_graveyard_destination(ability);
    state.waiting_for = crate::game::casting::initiate_cast_during_resolution(
        state,
        ability.controller,
        card,
        crate::game::casting::ResolutionCastRequest {
            constraint,
            cast_transformed,
            cleanup,
            graveyard_replacement,
            cost: match alt_mana_cost {
                Some(c) => crate::types::ability::ResolutionCastCost::AlternativeMana { cost: c },
                None => crate::types::ability::ResolutionCastCost::Free,
            },
        },
        events,
    )
    .map_err(|e| EffectError::InvalidParam(e.to_string()))?;
    Ok(())
}

/// CR 614.1a + CR 608.2n: Torrential Gearhulk / Kylox's Voltstrider class — the
/// parser represents "If that spell would be put into a graveyard, [exile it /
/// put it on the bottom of its owner's library / return it to its owner's hand]
/// instead" as a sequential rider sub-ability on `CastFromZone`, targeting the
/// cast spell (`ParentTarget`). Runtime consumes that rider as permission
/// metadata (the CR 608.2n redirect destination), not as an immediate zone
/// move. Returns the redirect destination the rider encodes, or `None` when the
/// sub-ability is not such a rider.
pub(crate) fn graveyard_destination_rider(
    ability: &ResolvedAbility,
) -> Option<SpellStackToGraveyardReplacement> {
    match &ability.effect {
        Effect::ChangeZone {
            destination: Zone::Exile,
            target: TargetFilter::ParentTarget,
            ..
        } => Some(SpellStackToGraveyardReplacement::Exile),
        Effect::ChangeZone {
            destination: Zone::Hand,
            target: TargetFilter::ParentTarget,
            ..
        } => Some(SpellStackToGraveyardReplacement::Hand),
        Effect::PutAtLibraryPosition {
            target: TargetFilter::ParentTarget,
            position,
            ..
        } => Some(SpellStackToGraveyardReplacement::Library {
            position: position.clone(),
        }),
        _ => None,
    }
}

/// Exile-only view of [`graveyard_destination_rider`] — the structural marker
/// that suppresses the counter path's immediate graveyard→exile sub-ability and
/// is the only destination the COUNTER rider ever encodes (Force of Negation,
/// No More Lies; the counter library/hand redirect rides `countered_spell_zone`
/// instead, never a sub-ability).
pub(crate) fn is_graveyard_exile_rider_subability(ability: &ResolvedAbility) -> bool {
    matches!(
        graveyard_destination_rider(ability),
        Some(SpellStackToGraveyardReplacement::Exile)
    )
}

fn cast_from_zone_graveyard_destination(
    ability: &ResolvedAbility,
) -> Option<SpellStackToGraveyardReplacement> {
    ability
        .sub_ability
        .as_deref()
        .and_then(graveyard_destination_rider)
}

/// CR 614.1c + CR 122.1: Osteomancer Adept / The Tomb of Aclazotz class — the
/// parser represents "the creature cast this way enters with a [counter] counter
/// on it" as a sequential `AddPendingETBCounters` rider on `CastFromZone`. The
/// rider's target is the *future* spell cast via the granted permission, not the
/// current trigger event, so it is consumed as permission metadata rather than
/// resolved in place (a standalone `AddPendingETBCounters` reads a `SpellCast`
/// event that does not exist when the permission-granting ability resolves).
pub(crate) fn is_enters_with_counter_rider_subability(ability: &ResolvedAbility) -> bool {
    matches!(&ability.effect, Effect::AddPendingETBCounters { .. })
}

/// Extract the counter the cast-this-way creature enters with, if the
/// `CastFromZone` carries an enters-with-counter rider sub-ability. Returns the
/// rider's counter type; the count is fixed at one per CR 122.1 (the printed
/// rider is always "a [counter] counter").
fn cast_from_zone_enters_with_counter(
    ability: &ResolvedAbility,
) -> Option<crate::types::counter::CounterType> {
    let sub = ability.sub_ability.as_deref()?;
    if !is_enters_with_counter_rider_subability(sub) {
        return None;
    }
    match &sub.effect {
        Effect::AddPendingETBCounters { counter_type, .. } => Some(counter_type.clone()),
        _ => None,
    }
}

/// CR 205.1b + CR 613.1d: The Tomb of Aclazotz class — extract the enters-with
/// continuous modifications ("… is a Vampire in addition to its other types")
/// the cast-this-way creature gains. The `AddPendingEntersModifications` rider
/// sits at depth 0 (a type-only grant, `CastFromZone.sub_ability`) or depth 1
/// (nested under the enters-with-counter rider, as Tomb produces: the counter
/// clause's own `sub_ability`). Walks the sub-ability chain and returns the
/// first rider's modifications, or an empty `Vec` if none is present. Consumed
/// as permission metadata (never resolved in place), mirroring
/// `cast_from_zone_enters_with_counter`.
fn cast_from_zone_enters_with_modifications(
    ability: &ResolvedAbility,
) -> Vec<crate::types::ability::ContinuousModification> {
    let mut cursor = ability.sub_ability.as_deref();
    while let Some(sub) = cursor {
        if let Effect::AddPendingEntersModifications { modifications } = &sub.effect {
            return modifications.clone();
        }
        cursor = sub.sub_ability.as_deref();
    }
    Vec::new()
}

/// Result of a lingering cast permission grant. An exile-delivery batch owns
/// `EffectResolved` so its tail cannot run before a parked replacement choice.
pub(crate) enum LingeringPermissionGrantResult {
    /// Every resolved target already occupies an in-place supported zone.
    Immediate,
    /// One batch delivered every current-zone-to-Exile target through the
    /// replacement pipeline synchronously; its completion recorded only
    /// settled exile cards and emitted the resolution tail.
    ExileDeliveryComplete,
    /// The replacement pipeline parked at CR 616.1; its completion owns the
    /// permission and resolution tail once the choice settles.
    NeedsChoice,
}

/// CR 118.9: Stamp `ExileWithAltCost` / `ExileWithAltAbilityCost` on resolved
/// targets. Shared by the direct resolve path and the `EffectZoneChoice` resume
/// path (Electrodominance hand pick).
pub(crate) fn grant_lingering_permissions(
    state: &mut GameState,
    ability: &ResolvedAbility,
    target_ids: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<LingeringPermissionGrantResult, EffectError> {
    let mut in_place_ids = Vec::new();
    let mut exile_delivery_ids = Vec::new();
    for &obj_id in target_ids {
        let Some(current_zone) = state.objects.get(&obj_id).map(|object| object.zone) else {
            continue;
        };
        if matches!(current_zone, Zone::Exile | Zone::Graveyard | Zone::Hand) {
            in_place_ids.push(obj_id);
        } else {
            exile_delivery_ids.push(obj_id);
        }
    }

    if exile_delivery_ids.is_empty() {
        record_lingering_permissions(state, ability, &in_place_ids)?;
        return Ok(LingeringPermissionGrantResult::Immediate);
    }

    // CR 614.1 + CR 616.1: The impulse-draw-class current-zone-to-Exile
    // instruction is a replaceable effect-owned event. Keep the permission
    // recording and resolution event in the typed completion so a replacement
    // choice cannot expose either tail before the whole batch settles.
    let requests = exile_delivery_ids
        .iter()
        .map(|&obj_id| ZoneMoveRequest::effect(obj_id, Zone::Exile, ability.source_id))
        .collect();
    let result = zone_pipeline::move_objects_simultaneously_then(
        state,
        requests,
        Some(BatchCompletion::CastFromZoneExileDeliveryComplete {
            ability: Box::new(ability.clone()),
            in_place_ids,
            exile_delivery_ids,
        }),
        events,
    );
    Ok(match result {
        BatchMoveResult::Done => LingeringPermissionGrantResult::ExileDeliveryComplete,
        BatchMoveResult::NeedsChoice => LingeringPermissionGrantResult::NeedsChoice,
    })
}

/// CR 118.9: Construct the object-local casting permissions after the caller
/// established that each target is in a zone the permission can authorize.
fn record_lingering_permissions(
    state: &mut GameState,
    ability: &ResolvedAbility,
    target_ids: &[ObjectId],
) -> Result<(), EffectError> {
    let (
        without_paying,
        cast_transformed,
        alt_ability_cost,
        constraint,
        duration,
        mana_spend_permission,
    ) = match &ability.effect {
        Effect::CastFromZone {
            without_paying_mana_cost,
            cast_transformed,
            alt_ability_cost,
            constraint,
            duration,
            mana_spend_permission,
            ..
        } => (
            *without_paying_mana_cost,
            *cast_transformed,
            alt_ability_cost.clone(),
            constraint.clone(),
            duration.clone(),
            *mana_spend_permission,
        ),
        _ => return Err(EffectError::MissingParam("CastFromZone".to_string())),
    };
    let graveyard_replacement = cast_from_zone_graveyard_destination(ability);
    // CR 614.1c + CR 122.1: "the creature cast this way enters with a [counter]
    // counter on it" — recorded on the granted permission so the cast
    // finalization (`casting_costs::finalize`) registers a pending ETB counter
    // on the cast object (Osteomancer Adept, The Tomb of Aclazotz).
    let enters_with_counter = cast_from_zone_enters_with_counter(ability);
    // CR 205.1b + CR 613.1d: "… is a [type] in addition to its other types" —
    // the additive type grant recorded on the granted permission so the cast
    // finalization applies it as a Permanent continuous effect on the cast
    // object (The Tomb of Aclazotz).
    let enters_with_modifications = cast_from_zone_enters_with_modifications(ability);

    for &obj_id in target_ids {
        // CR 601.2a: Targeted graveyard grants (Emry, Lurker in the Loch) and
        // resolution-time hand picks (Electrodominance) keep the card in its
        // source zone and grant a permission the casting pipeline consumes in
        // place. Current-zone-to-Exile targets arrive here only after their
        // replacement-safe delivery settled in exile.
        let current_zone = state.objects.get(&obj_id).map(|o| o.zone);
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
                    // semantics used by Discover, Suspend, Nashi, etc., whose
                    // cards are exiled and stay castable until they leave exile
                    // (cleared by `zones::apply_zone_exit_cleanup`).
                    // CR 611.2a: An *in-place* grant on a card left in the hand
                    // or graveyard (Emry, Sunforger searching to hand,
                    // Electrodominance) is a continuous effect from this
                    // ability's resolution; it must expire at cleanup if the
                    // cast is declined, since the card never leaves a zone that
                    // would trigger permission cleanup.
                    // Default both in-place origins to UntilEndOfTurn when the
                    // parser carried no explicit duration. (Exile-origin grants
                    // keep `None` — they are pruned on leaving exile instead.)
                    duration: duration.clone().or_else(|| {
                        matches!(current_zone, Some(Zone::Graveyard | Zone::Hand))
                            .then_some(Duration::UntilEndOfTurn)
                    }),
                    graveyard_replacement: graveyard_replacement.clone(),
                    enters_with_counter: enters_with_counter.clone(),
                    enters_with_modifications: enters_with_modifications.clone(),
                    // CR 609.4b: Forward "mana of any type can be spent to cast
                    // that spell" (Quistis Trepe, Tinybones the Pickpocket) onto
                    // the grant so the concession is scoped to this specific
                    // cast, read at payment by
                    // `player_can_spend_as_any_color_for_optional_spell`.
                    mana_spend_permission,
                }
            };
            if !obj.casting_permissions.contains(&permission) {
                obj.casting_permissions.push(permission);
            }
        }
    }
    Ok(())
}

/// CR 614.1 + CR 616.1 + CR 611.2a: Complete an impulse-draw-class exile
/// delivery only after every proposed move settles. A redirected card receives
/// no exile permission; existing Hand/Graveyard/Exile targets retain their
/// established in-place grant behavior.
pub(crate) fn complete_lingering_permissions_after_exile_delivery(
    state: &mut GameState,
    ability: &ResolvedAbility,
    in_place_ids: &[ObjectId],
    exile_delivery_ids: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    let mut permission_ids = in_place_ids.to_vec();
    permission_ids.extend(exile_delivery_ids.iter().copied().filter(|obj_id| {
        state
            .objects
            .get(obj_id)
            .is_some_and(|object| object.zone == Zone::Exile)
    }));
    record_lingering_permissions(state, ability, &permission_ids)
        .expect("CastFromZone batch completion carries a CastFromZone ability");
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CastFromZone,
        source_id: ability.source_id,
        subject: None,
    });
    BatchMoveResult::Done
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::zones;
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
                mana_spend_permission: None,
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
                mana_spend_permission: None,
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

    /// Issue #2884 / #852 — Memory Plunder (opponent graveyard) and Torrential
    /// Gearhulk (own graveyard): immediate "you may cast target … from a
    /// graveyard without paying its mana cost" must cast during resolution.
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
                mana_spend_permission: None,
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

    #[test]
    fn own_graveyard_immediate_free_cast_moves_directly_to_stack() {
        let mut state = make_test_state();
        let obj_id = {
            let id = add_card_to_graveyard(&mut state, PlayerId(0), CardId(852));
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
                mana_spend_permission: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&obj_id).map(|obj| obj.zone),
            Some(Zone::Stack),
            "Torrential Gearhulk class free casts must move from own graveyard to stack during resolution"
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
                mana_spend_permission: None,
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

    /// CR 707.10 + CR 608.2g (issue #4792): Isochron Scepter copies an imprinted
    /// instant onto the stack, then a chained `CastFromZone { ParentTarget,
    /// DuringResolution }` completes the free cast. The copy must not be routed
    /// through `initiate_cast_during_resolution` — Stack is not a castable zone.
    #[test]
    fn stack_spell_copy_parent_target_casts_without_zone_move() {
        use std::sync::Arc;

        use crate::game::effects::copy_spell;
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, CopyRetargetPermission, QuantityExpr,
        };
        use crate::types::game_state::{StackEntry, StackEntryKind};

        let mut state = make_test_state();
        let scepter_id = ObjectId(5);
        let target_creature = create_object(
            &mut state,
            CardId(99),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }

        let imprint_spell = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
        );
        let imprint_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Shock".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&imprint_id).unwrap().abilities = Arc::new(vec![imprint_spell]);
        state
            .tracked_object_sets
            .insert(crate::types::identifiers::TrackedSetId(0), vec![imprint_id]);

        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::TrackedSet {
                    id: crate::types::identifiers::TrackedSetId(0),
                },
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: vec![],
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            scepter_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        copy_spell::resolve(&mut state, &copy_ability, &mut events).unwrap();
        let copy_id = state.stack.back().expect("copy on stack").id;

        let cast_ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::ParentTarget,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: CastFromZoneDriver::DuringResolution,
                mana_spend_permission: None,
            },
            vec![TargetRef::Object(copy_id)],
            scepter_id,
            PlayerId(0),
        );
        resolve(&mut state, &cast_ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&imprint_id).map(|o| o.zone),
            Some(Zone::Exile),
            "imprinted card stays in exile"
        );
        assert_eq!(
            state.objects.get(&copy_id).map(|o| o.zone),
            Some(Zone::Stack),
            "copy remains on the stack"
        );
        assert!(
            events.iter().any(|event| {
                matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == copy_id)
            }),
            "CastFromZone must complete the copy cast with SpellCast"
        );
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::CopyRetarget { copy_id: cid, .. } if cid == copy_id
            ),
            "targeted copy must open retarget selection, got {:?}",
            state.waiting_for
        );

        // Choose a target and finalize the cast.
        let _ = apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target_creature)),
            },
        )
        .expect("choose shock target");
        assert!(
            state.stack.iter().any(|entry| {
                matches!(
                    entry,
                    StackEntry {
                        id,
                        kind: StackEntryKind::Spell { .. },
                        ..
                    } if *id == copy_id
                )
            }),
            "copy spell must remain on the stack after targeting"
        );
    }

    /// CR 310.11b (#2876): Siege defeat — "exile it, then you may cast it
    /// transformed". The `CastFromZone { target: SelfRef }` sub-ability fires
    /// with an EMPTY `ability.targets` (the exile step doesn't pre-select a
    /// target; the source IS the card to cast). Without the SelfRef fallback in
    /// `resolve`, `target_ids` stays empty and the function returns early,
    /// leaving the Siege card in exile forever. With the fix, the source id is
    /// used directly and the card is cast onto the stack.
    ///
    /// Discriminating assertion: stack grows by 1 and the exiled card moves to
    /// Zone::Stack. Reverting the SelfRef fallback makes target_ids stay empty,
    /// hitting the early-return path — stack stays 0, card stays in exile.
    #[test]
    fn siege_self_ref_cast_with_empty_targets_casts_from_exile() {
        let mut state = make_test_state();
        let siege_id = create_object(
            &mut state,
            CardId(9001),
            PlayerId(0),
            "Invasion of Ikoria".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&siege_id).unwrap();
            obj.card_types.core_types.push(CoreType::Battle);
            obj.mana_cost = ManaCost::generic(4);
        }
        let captured_incarnation = state.objects[&siege_id].incarnation;

        // The Siege defeat sub-ability has SelfRef target and DuringResolution
        // driver. Crucially, ability.targets is EMPTY — the Siege card is the
        // source, not a pre-selected target.
        let mut ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::SelfRef,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: true,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: CastFromZoneDriver::DuringResolution,
                mana_spend_permission: None,
            },
            vec![], // empty — the bug: source is the card to cast, not a named target
            siege_id,
            PlayerId(0),
        );
        ability.set_source_incarnation_recursive(Some(captured_incarnation));

        // CR 400.7j: mirror `resolve_top` — during resolution the resolving entry is
        // stashed in `state.resolving_stack_entry`, and the self-move re-latch reads
        // it to record that the resolving ability moved its OWN source. Without this
        // (as in production) the relatch cannot fire.
        state.resolving_stack_entry = Some(crate::types::game_state::StackEntry {
            id: siege_id,
            source_id: siege_id,
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::ActivatedAbility {
                source_id: siege_id,
                ability: ability.clone(),
            },
        });

        let mut events = Vec::new();
        zones::move_to_zone(&mut state, siege_id, Zone::Exile, &mut events);
        // CR 400.7: under all-zone incarnation semantics the BF→Exile self-move now
        // bumps the epoch (it no longer stays stable). CR 400.7j: the re-latch record
        // captures the from→to incarnation so the same-resolution self-cast still
        // finds the moved source instead of going stale.
        assert!(
            state.objects[&siege_id].incarnation > captured_incarnation,
            "CR 400.7: the BF→Exile self-move bumps the Siege's incarnation"
        );
        assert!(
            ability.source_is_current(&state),
            "CR 400.7j: the re-latch keeps the self-cast source current after the move"
        );
        events.clear();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.stack.len(),
            1,
            "CR 310.11b: Siege defeat must put the card on the stack (cast it), \
             not silently return with it still in exile"
        );
        assert_eq!(
            state.objects.get(&siege_id).map(|o| o.zone),
            Some(Zone::Stack),
            "the Siege card must move from exile to the stack on defeat"
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
                mana_spend_permission: None,
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
                mana_spend_permission: None,
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
                mana_spend_permission: None,
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
                mana_spend_permission: None,
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

    /// Issue #2019 — Kiora, Sovereign of the Deep: look-then-cast chains leave
    /// cards in the library via `last_revealed_ids`, but the parser binds the
    /// cast step to `ExiledBySource`. Without the library fallback the cast
    /// sub-ability silently no-ops.
    #[test]
    fn look_peek_exiled_by_source_cast_uses_last_revealed_library_cards() {
        let mut state = make_test_state();
        let source = create_object(
            &mut state,
            CardId(999),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let instant = create_object(
            &mut state,
            CardId(301),
            PlayerId(0),
            "Looked Instant".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&instant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        state.objects.get_mut(&instant).unwrap().mana_cost = ManaCost::generic(3);
        state.last_revealed_ids = vec![instant];

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
                mana_spend_permission: None,
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&instant).expect("looked card");
        assert_eq!(obj.zone, Zone::Exile, "library cast grant exiles the card");
        assert!(
            obj.casting_permissions.iter().any(|p| matches!(
                p,
                CastingPermission::ExileWithAltCost { cost, .. } if *cost == ManaCost::zero()
            )),
            "looked library card must receive a free cast permission"
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
    fn hand_cast_selection_casts_during_resolution_without_lingering_permission() {
        let mut state = make_test_state();
        let cheap = add_card_to_hand(&mut state, PlayerId(0), CardId(505));
        let ability = electrodominance_hand_ability(3);

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();
        apply_as_current(&mut state, GameAction::SelectCards { cards: vec![cheap] }).unwrap();

        assert_eq!(state.objects[&cheap].zone, Zone::Stack);
        assert!(
            matches!(
                state.objects[&cheap].casting_permissions.as_slice(),
                [CastingPermission::ExileWithAltCost {
                    resolution_cleanup: None,
                    mana_spend_permission: None,
                    graveyard_replacement: None,
                    enters_with_counter: None,
                    enters_with_modifications,
                    ..
                }] if enters_with_modifications.is_empty()
            ),
            "the consumed hand-cast permission must remain only as a neutral stable slot"
        );

        crate::game::stack::resolve_top(&mut state, &mut events);
        assert!(
            state.objects[&cheap].casting_permissions.is_empty(),
            "normal Stack exit cleanup must remove the neutral consumed slot"
        );
    }

    /// CR 118.9 + CR 702.62a + CR 608.2g: The Face of Boe RUNTIME proof. Picking a
    /// suspend sorcery during resolution casts it WITHOUT paying its printed mana
    /// cost ({5}) and instead pays its colored suspend cost ({1}{U}) via the
    /// `ExileWithAltCost` override under `Auto` payment. The load-bearing delta:
    /// the controller's mana pool drains by exactly the suspend cost, not the
    /// printed cost, and the spell lands on the stack. This is the first
    /// during-resolution cast charging a non-zero, colored alternative cost.
    #[test]
    fn face_of_boe_picks_suspend_card_and_pays_suspend_cost() {
        use crate::types::keywords::Keyword;
        use crate::types::mana::{ManaCost as MC, ManaCostShard, ManaType, ManaUnit};

        let mut state = make_test_state();

        // A suspended sorcery in hand: printed {5}, Suspend 4—{1}{U}.
        let suspended = create_object(
            &mut state,
            CardId(7100),
            PlayerId(0),
            "Suspended Sorcery".to_string(),
            Zone::Hand,
        );
        let suspend_cost = MC::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        };
        {
            let obj = state.objects.get_mut(&suspended).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.base_card_types = obj.card_types.clone();
            obj.mana_cost = MC::generic(5);
            obj.keywords.push(Keyword::Suspend {
                count: 4,
                cost: suspend_cost.clone(),
            });
            obj.base_keywords = obj.keywords.clone();
        }

        // Fund the pool with {U}{U} — one blue pays the {U} pip, the other the
        // {1} generic. (If the override leaked the printed {5}, this could not pay
        // and the spell would not reach the stack.)
        for _ in 0..2 {
            state.add_mana_to_pool(
                PlayerId(0),
                ManaUnit::new(ManaType::Blue, suspended, false, Vec::new()),
            );
        }
        assert_eq!(state.players[0].mana_pool.total(), 2);

        // The Face of Boe's cast clause: hand-origin suspend filter, alt suspend
        // cost, during-resolution driver.
        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::Typed(
                    TypedFilter::default()
                        .with_type(TypeFilter::Card)
                        .controller(ControllerRef::You)
                        .properties(vec![
                            FilterProp::WithKeyword {
                                value: Keyword::Suspend {
                                    count: 0,
                                    cost: MC::zero(),
                                },
                            },
                            FilterProp::InZone { zone: Zone::Hand },
                        ]),
                ),
                without_paying_mana_cost: false,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: Some(
                    crate::types::ability::AbilityCost::KeywordCostOfCastSpell {
                        keyword: crate::types::keywords::KeywordKind::Suspend,
                    },
                ),
                constraint: None,
                duration: None,
                driver: CastFromZoneDriver::DuringResolution,
                mana_spend_permission: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();
        apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![suspended],
            },
        )
        .unwrap();

        // The spell is on the stack...
        assert_eq!(
            state.objects[&suspended].zone,
            Zone::Stack,
            "the picked suspend card must be cast onto the stack"
        );
        // ...and the pool drained by exactly the {1}{U} suspend cost, NOT {5}.
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "the suspend cost {{1}}{{U}} (2 mana) must have been auto-paid from the pool; \
             a leaked printed {{5}} would leave mana unspent or fail the cast"
        );
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Blue),
            0,
            "both blue pips were spent on the {{U}} pip and the {{1}} generic"
        );
    }

    #[test]
    fn hand_pick_aborts_when_borrowed_keyword_cost_is_unreadable() {
        use crate::types::keywords::KeywordKind;
        use crate::types::mana::ManaCost as MC;

        let mut state = make_test_state();

        // A card picked from hand that does NOT expose the borrowed keyword
        // (no Suspend present). This stands in for the defensive case where
        // `effective_keyword_mana_cost` returns `None` — e.g. a misparse that
        // bound a `KeywordCostOfCastSpell` to a card lacking that keyword. CR
        // 118.9 requires this surface a refusal, never a silent free cast.
        let picked = create_object(
            &mut state,
            CardId(7200),
            PlayerId(0),
            "Costless Pick".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&picked).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.base_card_types = obj.card_types.clone();
            obj.mana_cost = MC::generic(5);
        }

        // During-resolution cast that borrows a Suspend cost the picked card
        // cannot supply.
        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::Typed(
                    TypedFilter::default()
                        .with_type(TypeFilter::Card)
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
                ),
                without_paying_mana_cost: false,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: Some(
                    crate::types::ability::AbilityCost::KeywordCostOfCastSpell {
                        keyword: KeywordKind::Suspend,
                    },
                ),
                constraint: None,
                duration: None,
                driver: CastFromZoneDriver::DuringResolution,
                mana_spend_permission: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        let used_during_resolution =
            complete_hand_pick_cast_from_zone(&mut state, &ability, picked, &mut events).unwrap();

        // The cast aborts rather than free-casting at {0}.
        assert!(
            !used_during_resolution,
            "an unreadable borrowed keyword cost must abort, not initiate a during-resolution cast"
        );
        assert_eq!(
            state.objects[&picked].zone,
            Zone::Hand,
            "the picked card must stay in hand — no cast, no free-cast leak"
        );
        assert!(
            state.objects[&picked].casting_permissions.is_empty(),
            "no lingering free-cast permission may be granted on the abort path; got {:?}",
            state.objects[&picked].casting_permissions
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::CastFromZone,
                    ..
                }
            )),
            "the granting effect must still resolve (as a no-op)"
        );
    }

    #[test]
    fn hand_in_place_grant_defaults_to_until_end_of_turn() {
        let mut state = make_test_state();
        let cheap = add_card_to_hand(&mut state, PlayerId(0), CardId(515));
        let ability = electrodominance_hand_ability(3);

        let mut events = vec![];
        grant_lingering_permissions(&mut state, &ability, &[cheap], &mut events).unwrap();

        assert_eq!(state.objects[&cheap].zone, Zone::Hand);
        assert!(
            state.objects[&cheap]
                .casting_permissions
                .iter()
                .any(|p| matches!(
                    p,
                    CastingPermission::ExileWithAltCost {
                        duration: Some(Duration::UntilEndOfTurn),
                        granted_to: Some(PlayerId(0)),
                        ..
                    }
                )),
            "hand-origin in-place grant must default to UntilEndOfTurn so a \
             declined offer expires at cleanup; got {:?}",
            state.objects[&cheap].casting_permissions
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
                mana_spend_permission: None,
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
                mana_spend_permission: None,
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        )));

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&instant].zone, Zone::Stack);
        assert!(
            state.objects[&instant].casting_permissions.iter().any(|p| {
                matches!(
                    p,
                    CastingPermission::ExileWithAltCost {
                        graveyard_replacement: Some(SpellStackToGraveyardReplacement::Exile),
                        ..
                    }
                )
            }) || !state.objects[&instant].replacement_definitions.is_empty(),
            "exile rider must stamp either the permission or a graveyard redirect"
        );
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
                mana_spend_permission: None,
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
